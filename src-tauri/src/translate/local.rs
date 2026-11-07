//! Local translation through the embedded Hy-MT2-1.8B model (Q3_K_M GGUF).
//!
//! The model file is downloaded once, on first launch, from HuggingFace into
//! the app's data directory and then loaded lazily on the first translation.
//! Inference is CPU-only (Candle) and runs on a blocking thread; deltas are
//! streamed through the same `Channel<TranslateEvent>` as the other modes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager};

use hy_mt_core::device::DeviceCtx;
use hy_mt_core::generate::Generator;
use hy_mt_core::gguf::HyGgufFile;
use hy_mt_core::model::HunyuanDense;
use hy_mt_core::sampling::SamplingParams;
use hy_mt_core::tokenizer::HyTokenizer;

use super::{chunk, protect_code, restore_code, safe_stream_prefix, Direction, TranslateEvent};
use crate::settings::TranslationSettings;
use crate::state::AppState;

pub const MODEL_FILE: &str = "Hy-MT2-1.8B.Q3_K_M.gguf";
/// Primary source plus a mirror, tried in order: HuggingFace is frequently
/// unreachable from mainland China without a proxy.
pub const MODEL_URLS: [&str; 2] = [
    "https://huggingface.co/mradermacher/Hy-MT2-1.8B-GGUF/resolve/main/Hy-MT2-1.8B.Q3_K_M.gguf",
    "https://hf-mirror.com/mradermacher/Hy-MT2-1.8B-GGUF/resolve/main/Hy-MT2-1.8B.Q3_K_M.gguf",
];
/// Size of the Q3_K_M build; used for progress until the server reports one.
pub const MODEL_BYTES: u64 = 951_022_336;

/// Greedy decoding is the standard for machine translation (matches the
/// reference `hy-mt` CLI defaults).
const MAX_NEW_TOKENS: usize = 1024;
/// Chunks stay well inside the model's context; long replies translate
/// progressively instead of risking a truncated tail.
const CHUNK_MAX: usize = 1200;
/// Progress events are throttled so a fast download can't flood the IPC.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Cached model+tokenizer. `Generator` needs `&mut`, so callers serialize
/// access through the surrounding `Mutex`.
pub struct LocalTranslator {
    model: HunyuanDense,
    tokenizer: HyTokenizer,
}

impl LocalTranslator {
    fn load(path: &Path) -> Result<Self, String> {
        let t0 = Instant::now();
        tracing::info!(path = %path.display(), "loading local translation model");
        let file = HyGgufFile::load(path).map_err(|e| format!("读取模型失败: {e}"))?;
        // Build the tokenizer from the GGUF metadata before the file (and
        // its mmap) is dropped; the weights are copied out at load time.
        let tokenizer =
            HyTokenizer::from_gguf(file.content()).map_err(|e| format!("加载 tokenizer 失败: {e}"))?;
        let device = DeviceCtx::cpu();
        let model =
            HunyuanDense::load_from(&file, &device).map_err(|e| format!("加载模型权重失败: {e}"))?;
        if tokenizer.family() != model.config.tokenizer_family {
            return Err("tokenizer 与模型家族不匹配，模型文件可能损坏".into());
        }
        tracing::info!(ms = t0.elapsed().as_millis() as u64, "local model ready");
        Ok(Self { model, tokenizer })
    }

    /// Translate one fragment, streaming decoded text into `emit`.
    /// Returns the restored (sentinels → original code) translation.
    fn translate_fragment(
        &mut self,
        target: &str,
        text: &str,
        blocks: &[super::ProtectedBlock],
        emit: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        let prompt = self
            .tokenizer
            .build_translate_prompt(target, text)
            .map_err(|e| format!("构建翻译提示失败: {e}"))?;
        let params = SamplingParams {
            temperature: 0.0,
            ..Default::default()
        };
        let mut generator = Generator::new(&mut self.model, &self.tokenizer, params, MAX_NEW_TOKENS);

        let mut acc = String::new();
        let mut pending = String::new(); // may end inside a sentinel
        generator
            .generate(&prompt, |step| {
                if step.text.is_empty() {
                    return true;
                }
                acc.push_str(&step.text);
                pending.push_str(&step.text);
                // Emit everything except a possibly-unfinished sentinel.
                let safe = safe_stream_prefix(&pending);
                if safe > 0 {
                    let out = restore_code(&pending[..safe], blocks);
                    pending.drain(..safe);
                    if !out.is_empty() {
                        emit(&out);
                    }
                }
                true
            })
            .map_err(|e| format!("本地推理失败: {e}"))?;
        if !pending.is_empty() {
            let out = restore_code(&pending, blocks);
            if !out.is_empty() {
                emit(&out);
            }
        }
        Ok(restore_code(&acc, blocks))
    }
}

// ---------- model paths ----------

/// Model directory. Uses `%LOCALAPPDATA%` on Windows: `%APPDATA%` roams and
/// a ~1 GB model must not be synced. Elsewhere it lives next to settings.
pub fn models_dir(app: &AppHandle) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let _ = app;
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .unwrap_or_else(std::env::temp_dir);
        base.join("com.yiterm.app").join("models")
    }
    #[cfg(not(target_os = "windows"))]
    {
        app.path()
            .app_data_dir()
            .expect("app data dir unavailable")
            .join("models")
    }
}

/// Settings may point at a user-supplied GGUF; else the managed location.
pub fn resolve_model_path(app: &AppHandle, settings: &TranslationSettings) -> PathBuf {
    match settings.local_model_path.as_deref().map(str::trim) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => models_dir(app).join(MODEL_FILE),
    }
}

// ---------- download ----------

#[derive(Debug, Default)]
pub struct ModelDownload {
    pub downloading: bool,
    pub downloaded: u64,
    pub total: u64,
    pub error: Option<String>,
}

pub type DownloadHandle = Arc<Mutex<ModelDownload>>;
pub type TranslatorHandle = Arc<tokio::sync::Mutex<Option<Arc<Mutex<LocalTranslator>>>>>;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    /// "missing" | "downloading" | "ready" | "error"
    pub state: String,
    pub downloaded: u64,
    pub total: u64,
    pub path: String,
    pub message: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelProgress {
    state: String,
    downloaded: u64,
    total: u64,
    message: Option<String>,
}

pub fn status(app: &AppHandle, state: &AppState) -> ModelStatus {
    let settings = state.settings.read().unwrap().clone();
    let path = resolve_model_path(app, &settings.translation);
    let dl = state.model.lock().unwrap();
    let (state_str, message) = if path.is_file() {
        ("ready", None)
    } else if dl.downloading {
        ("downloading", None)
    } else if let Some(e) = &dl.error {
        ("error", Some(e.clone()))
    } else {
        ("missing", None)
    };
    ModelStatus {
        state: state_str.to_string(),
        downloaded: if state_str == "ready" {
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(dl.downloaded)
        } else {
            dl.downloaded
        },
        total: if dl.total > 0 { dl.total } else { MODEL_BYTES },
        path: path.to_string_lossy().into_owned(),
        message,
    }
}

fn emit_progress(app: &AppHandle, state: &str, downloaded: u64, total: u64, message: Option<String>) {
    let _ = app.emit(
        "model-progress",
        ModelProgress {
            state: state.to_string(),
            downloaded,
            total,
            message,
        },
    );
}

/// Kicks off the background download unless one is running or the file is
/// already in place. Returns immediately; completion arrives as
/// `model-progress` events.
pub fn start_download(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let settings = state.settings.read().unwrap().clone();
    let path = resolve_model_path(app, &settings.translation);
    if path.is_file() {
        return Ok(());
    }
    {
        let mut dl = state.model.lock().unwrap();
        if dl.downloading {
            return Ok(());
        }
        dl.downloading = true;
        dl.error = None;
        dl.downloaded = 0;
        dl.total = MODEL_BYTES;
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = download_task(&app2).await {
            tracing::warn!(error = %e, "model download failed");
            let state = app2.state::<AppState>();
            {
                let mut dl = state.model.lock().unwrap();
                dl.downloading = false;
                dl.error = Some(e.clone());
            }
            emit_progress(&app2, "error", 0, MODEL_BYTES, Some(e));
        }
    });
    Ok(())
}

async fn download_task(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let settings = state.settings.read().unwrap().clone();
    let path = resolve_model_path(app, &settings.translation);
    if path.is_file() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| format!("创建模型目录失败: {e}"))?;
    }
    let part = path.with_extension("part");

    // Try the primary source, then the mirror. A mid-stream failure keeps
    // the `.part` file, so the next attempt resumes with a Range request.
    let mut last_err = String::new();
    for url in MODEL_URLS {
        match download_once(app, &path, &part, url).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(url, error = %e, "model download attempt failed");
                last_err = e;
            }
        }
    }
    Err(last_err)
}

/// One download attempt from `url`, resuming from `part` when it exists.
async fn download_once(
    app: &AppHandle,
    path: &Path,
    part: &Path,
    url: &str,
) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let state = app.state::<AppState>();
    let mut offset = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
    if offset > 0 {
        tracing::info!(offset, "resuming model download");
    }

    let mut req = state
        .http
        .get(url)
        // The client-wide timeout is 35 s; a ~1 GB body needs a much larger
        // per-request cap.
        .timeout(Duration::from_secs(3600));
    if offset > 0 {
        req = req.header("Range", format!("bytes={offset}-"));
    }
    let resp = req.send().await.map_err(|e| format!("下载请求失败: {e}"))?;
    let http_status = resp.status();
    if http_status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // The partial file doesn't line up with the remote; restart cleanly.
        let _ = tokio::fs::remove_file(part).await;
        return Err("分段下载已失效，请重试".into());
    }
    if !http_status.is_success() {
        return Err(format!("下载失败 HTTP {http_status}"));
    }

    let mut file = if http_status == reqwest::StatusCode::PARTIAL_CONTENT {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(part)
            .await
            .map_err(|e| format!("打开临时文件失败: {e}"))?
    } else {
        // Server ignored (or no) Range: start from scratch.
        offset = 0;
        tokio::fs::File::create(part)
            .await
            .map_err(|e| format!("创建临时文件失败: {e}"))?
    };
    let total = resp
        .content_length()
        .map(|l| l + offset)
        .unwrap_or(MODEL_BYTES);
    {
        let mut dl = state.model.lock().unwrap();
        dl.downloaded = offset;
        dl.total = total;
    }

    let mut downloaded = offset;
    let mut last_emit = Instant::now();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("下载中断: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入模型失败: {e}"))?;
        downloaded += chunk.len() as u64;
        {
            let mut dl = state.model.lock().unwrap();
            dl.downloaded = downloaded;
        }
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            emit_progress(app, "downloading", downloaded, total, None);
            last_emit = Instant::now();
        }
    }
    file.flush().await.map_err(|e| format!("写入模型失败: {e}"))?;
    drop(file);

    if downloaded < total {
        // Keep the .part file: the next attempt resumes from here.
        return Err(format!(
            "下载未完成（{downloaded}/{total} 字节），可重试续传"
        ));
    }
    tokio::fs::rename(part, path)
        .await
        .map_err(|e| format!("保存模型失败: {e}"))?;
    {
        let mut dl = state.model.lock().unwrap();
        dl.downloading = false;
        dl.error = None;
        dl.downloaded = downloaded;
        dl.total = total;
    }
    tracing::info!(path = %path.display(), bytes = downloaded, "model downloaded");
    emit_progress(app, "ready", downloaded, total, None);
    Ok(())
}

/// Deletes the model file (and any partial download), dropping the loaded
/// translator so a later translation reloads from disk.
pub async fn delete_model(app: &AppHandle, state: &AppState) -> Result<(), String> {
    {
        let dl = state.model.lock().unwrap();
        if dl.downloading {
            return Err("模型正在下载，无法删除".into());
        }
    }
    let settings = state.settings.read().unwrap().clone();
    let path = resolve_model_path(app, &settings.translation);
    // Never delete a file the user pointed at themselves — only the managed
    // download is ours to remove.
    if path != models_dir(app).join(MODEL_FILE) {
        return Err("当前使用自定义模型路径，请在文件系统中手动删除".into());
    }
    let part = path.with_extension("part");
    for p in [&path, &part] {
        match std::fs::remove_file(p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("删除模型失败: {e}")),
        }
    }
    *state.local.lock().await = None;
    {
        let mut dl = state.model.lock().unwrap();
        dl.downloaded = 0;
        dl.error = None;
    }
    Ok(())
}

// ---------- translation ----------

async fn get_translator(
    state: &AppState,
    path: &Path,
) -> Result<Arc<Mutex<LocalTranslator>>, String> {
    let mut guard = state.local.lock().await;
    if let Some(t) = guard.as_ref() {
        return Ok(Arc::clone(t));
    }
    if !path.is_file() {
        return Err(format!(
            "本地模型尚未下载（{}），请在设置中下载模型",
            path.display()
        ));
    }
    let path = path.to_path_buf();
    // Parsing + weight load takes seconds; keep it off the async runtime.
    let loaded = tauri::async_runtime::spawn_blocking(move || LocalTranslator::load(&path))
        .await
        .map_err(|e| format!("模型加载任务失败: {e}"))??;
    let translator = Arc::new(Mutex::new(loaded));
    *guard = Some(Arc::clone(&translator));
    Ok(translator)
}

/// Runs one local translation, streaming deltas into `channel` and sending
/// the terminal `Done`/`Error` event (the caller mirrors `translate::run`).
pub async fn translate(
    app: &AppHandle,
    state: &AppState,
    direction: Direction,
    text: &str,
    request_id: &str,
    channel: &Channel<TranslateEvent>,
) -> Result<String, String> {
    let emit_error = |message: String| -> Result<String, String> {
        let _ = channel.send(TranslateEvent::Error {
            request_id: request_id.to_string(),
            message: message.clone(),
        });
        Err(message)
    };

    if text.trim().is_empty() {
        return Ok(String::new());
    }

    // First use loads the ~1 GB model before any token can be emitted; send
    // an empty delta so the frontend's idle watchdog starts its window from
    // now instead of from the invoke call.
    let _ = channel.send(TranslateEvent::Delta {
        request_id: request_id.to_string(),
        text: String::new(),
    });

    let settings = state.settings.read().unwrap().clone();
    let path = resolve_model_path(app, &settings.translation);
    let translator = match get_translator(state, &path).await {
        Ok(t) => t,
        Err(e) => return emit_error(e),
    };

    let target = match direction {
        Direction::ZhToEn => "English",
        Direction::EnToZh => "Chinese",
    };
    let in_chars = text.chars().count();
    let text = text.to_string();
    let request_id_owned = request_id.to_string();
    let channel_owned = channel.clone();

    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut guard = translator
            .lock()
            .map_err(|_| "本地翻译器状态异常".to_string())?;
        let mut emit = |piece: &str| {
            let _ = channel_owned.send(TranslateEvent::Delta {
                request_id: request_id_owned.clone(),
                text: piece.to_string(),
            });
        };
        translate_blocking(&mut guard, target, &text, &mut emit)
    })
    .await
    .map_err(|e| format!("本地翻译任务失败: {e}"))?;

    match result {
        Ok(full) => {
            tracing::info!(
                mode = "local",
                direction = ?direction,
                in_chars,
                out_chars = full.chars().count(),
                "translate ok"
            );
            let _ = channel.send(TranslateEvent::Done {
                request_id: request_id.to_string(),
                text: full.clone(),
            });
            Ok(full)
        }
        Err(e) => {
            tracing::warn!(mode = "local", error = %e, "translate failed");
            emit_error(e)
        }
    }
}

fn translate_blocking(
    translator: &mut LocalTranslator,
    target: &str,
    text: &str,
    emit: &mut dyn FnMut(&str),
) -> Result<String, String> {
    // Swap code blocks / inline code for sentinels so they survive verbatim,
    // then hide paragraph breaks the same way — the model otherwise collapses
    // markdown paragraphs into single newlines.
    let (protected, mut blocks) = protect_code(text);
    let protected = super::protect_breaks(&protected, &mut blocks);
    let mut full = String::new();
    for part in chunk(&protected, CHUNK_MAX) {
        if part.trim().is_empty() {
            full.push_str(&part);
            continue;
        }
        let translated = translator.translate_fragment(target, &part, &blocks, emit)?;
        full.push_str(&translated);
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_urls_point_at_the_q3_k_m_build() {
        assert!(MODEL_FILE.ends_with(".Q3_K_M.gguf"));
        for url in MODEL_URLS {
            assert!(url.ends_with(MODEL_FILE), "unexpected URL: {url}");
        }
    }

    /// Opt-in end-to-end check against the real model — too slow for CI:
    ///
    /// ```bash
    /// YITERM_HYMT_MODEL=/path/to/Hy-MT2-1.8B.Q3_K_M.gguf \
    ///   cargo test --release --lib local_model -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn local_model_end_to_end() {
        let Ok(path) = std::env::var("YITERM_HYMT_MODEL") else {
            eprintln!("YITERM_HYMT_MODEL not set; skipping");
            return;
        };
        let mut translator = LocalTranslator::load(Path::new(&path)).expect("load model");
        let input = "运行 `npm install` 安装依赖。\n\n然后运行 `npm run dev` 启动服务器。\n\n```bash\necho hi\n```";
        let t0 = Instant::now();
        let mut streamed = String::new();
        let mut emit = |s: &str| streamed.push_str(s);
        let out = translate_blocking(&mut translator, "English", input, &mut emit)
            .expect("translate");
        eprintln!("{} ms, streamed {} chars", t0.elapsed().as_millis(), streamed.len());
        eprintln!("out: {out}");
        assert!(!out.contains("@@C"), "unrestored sentinel: {out}");
        assert!(out.contains("`npm install`"), "inline code lost: {out}");
        assert!(out.contains("```bash\necho hi\n```"), "fence lost: {out}");
        assert!(out.contains("\n\n"), "paragraph break lost: {out}");
    }

    #[test]
    fn chunking_covers_the_protected_text() {
        let text = "line one\n".repeat(500);
        let (protected, _) = protect_code(&text);
        let parts = chunk(&protected, CHUNK_MAX);
        assert!(parts.iter().all(|p| p.chars().count() <= CHUNK_MAX));
        assert_eq!(parts.concat(), protected);
    }
}
