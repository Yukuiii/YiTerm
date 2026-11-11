//! GitHub release check and in-place DMG update.
//!
//! The app is distributed as an unsigned DMG, so the official Tauri updater
//! (which needs an update-signing key and a signed bundle) is not a fit.
//! Instead we read the latest GitHub release, download its DMG, mount it and
//! swap the running `.app` bundle. Any failure falls back to opening the DMG
//! so the user can drag it into Applications manually.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, State, ipc::Channel};
use tokio::io::AsyncWriteExt;

use crate::state::AppState;

const RELEASES_API: &str = "https://api.github.com/repos/Yukuiii/YiTerm/releases/latest";
const DOWNLOAD_PREFIX: &str = "https://github.com/Yukuiii/YiTerm/releases/download/";
const UPDATE_DIR: &str = "yiterm-update";
const DMG_NAME: &str = "YiTerm-update.dmg";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: String,
    pub name: String,
    pub notes: String,
    pub asset_name: String,
    pub asset_url: String,
    pub published_at: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UpdateProgress {
    Progress { downloaded: u64, total: u64 },
    Done { path: String },
}

fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

fn is_newer(latest: &str, current: &str) -> bool {
    let latest = version_parts(latest);
    let current = version_parts(current);
    for i in 0..latest.len().max(current.len()) {
        let a = latest.get(i).copied().unwrap_or(0);
        let b = current.get(i).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    false
}

fn dmg_asset(release: &Value) -> Option<(String, String)> {
    let assets = release.get("assets")?.as_array()?;
    let is_dmg = |a: &&Value| {
        a.get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| n.ends_with(".dmg"))
    };
    let asset = assets
        .iter()
        .find(|a| {
            is_dmg(a)
                && a.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.contains("aarch64"))
        })
        .or_else(|| assets.iter().find(|a| is_dmg(a)))?;
    let name = asset.get("name")?.as_str()?.to_string();
    let url = asset.get("browser_download_url")?.as_str()?.to_string();
    Some((name, url))
}

async fn check(http: &reqwest::Client, current: &str) -> Result<Option<UpdateInfo>, String> {
    let release: Value = http
        .get(RELEASES_API)
        .timeout(Duration::from_secs(15))
        // GitHub rejects API requests without a User-Agent.
        .header("User-Agent", "YiTerm")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("检查更新失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("检查更新失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("检查更新响应解析失败: {e}"))?;

    let tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("更新信息缺少版本号")?
        .trim()
        .trim_start_matches('v')
        .to_string();
    if !is_newer(&tag, current) {
        return Ok(None);
    }
    let (asset_name, asset_url) = dmg_asset(&release).ok_or("该版本没有可用的 DMG 安装包")?;
    Ok(Some(UpdateInfo {
        current_version: current.to_string(),
        latest_version: tag,
        name: release
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("YiTerm")
            .to_string(),
        notes: release
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        asset_name,
        asset_url,
        published_at: release
            .get("published_at")
            .and_then(Value::as_str)
            .map(str::to_string),
    }))
}

#[tauri::command]
pub async fn update_check(state: State<'_, AppState>) -> Result<Option<UpdateInfo>, String> {
    check(&state.http, current_version()).await
}

#[tauri::command]
pub async fn update_download(
    state: State<'_, AppState>,
    url: String,
    channel: Channel<UpdateProgress>,
) -> Result<(), String> {
    let path = std::env::temp_dir().join(UPDATE_DIR).join(DMG_NAME);
    download(&state.http, &url, &path, |downloaded, total| {
        let _ = channel.send(UpdateProgress::Progress { downloaded, total });
    })
    .await?;
    let _ = channel.send(UpdateProgress::Done {
        path: path.to_string_lossy().into_owned(),
    });
    Ok(())
}

async fn download(
    http: &reqwest::Client,
    url: &str,
    path: &std::path::Path,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    if !url.starts_with(DOWNLOAD_PREFIX) {
        return Err("非预期的更新下载地址".into());
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("创建更新目录失败: {e}"))?;
    }
    let resp = http
        .get(url)
        .timeout(Duration::from_secs(10 * 60))
        .header("User-Agent", "YiTerm")
        .send()
        .await
        .map_err(|e| format!("下载更新失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("下载更新失败: {e}"))?;
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| format!("创建更新文件失败: {e}"))?;
    let mut stream = resp.bytes_stream();
    let mut downloaded = 0u64;
    let mut reported = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("下载更新失败: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("写入更新文件失败: {e}"))?;
        downloaded += chunk.len() as u64;
        if downloaded - reported >= 256 * 1024 || (total > 0 && downloaded == total) {
            reported = downloaded;
            on_progress(downloaded, total);
        }
    }
    file.flush()
        .await
        .map_err(|e| format!("写入更新文件失败: {e}"))?;
    on_progress(downloaded, total);
    Ok(())
}

fn hdiutil_detach(mount: &std::path::Path) {
    let _ = Command::new("/usr/bin/hdiutil")
        .args(["detach", "-quiet"])
        .arg(mount)
        .status();
}

fn open_dmg(dmg: &std::path::Path) {
    let _ = Command::new("/usr/bin/open").arg(dmg).spawn();
}

fn install_blocking(app: &AppHandle, dmg: &std::path::Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("定位当前应用失败: {e}"))?;
    let Some(bundle) = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(PathBuf::from)
    else {
        return Err("开发模式不支持自动安装，请手动安装 DMG".into());
    };
    // Running from the DMG's translocated copy: replacing that copy is useless.
    if bundle
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        open_dmg(dmg);
        return Err("请把 YiTerm 拖入 Applications 后再使用自动更新（已打开 DMG）".into());
    }

    let mount = std::env::temp_dir().join(format!("yiterm-update-mount-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&mount);
    std::fs::create_dir_all(&mount).map_err(|e| format!("创建挂载点失败: {e}"))?;

    let attach = Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
        .arg(&mount)
        .arg(dmg)
        .output()
        .map_err(|e| format!("挂载 DMG 失败: {e}"))?;
    if !attach.status.success() {
        let _ = std::fs::remove_dir_all(&mount);
        return Err(format!(
            "挂载 DMG 失败: {}",
            String::from_utf8_lossy(&attach.stderr).trim()
        ));
    }

    let source = std::fs::read_dir(&mount).ok().and_then(|entries| {
        entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "app"))
    });
    let Some(source) = source else {
        hdiutil_detach(&mount);
        return Err("DMG 中未找到 YiTerm.app".into());
    };

    let stage = std::env::temp_dir().join(format!("yiterm-update-stage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    let staged = Command::new("/usr/bin/ditto")
        .arg(&source)
        .arg(&stage)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !staged {
        hdiutil_detach(&mount);
        let _ = std::fs::remove_dir_all(&stage);
        return Err("复制新版本失败".into());
    }

    let backup = PathBuf::from(format!("{}.old-{}", bundle.display(), std::process::id()));
    if let Err(e) = std::fs::rename(&bundle, &backup) {
        hdiutil_detach(&mount);
        let _ = std::fs::remove_dir_all(&stage);
        open_dmg(dmg);
        return Err(format!(
            "无法替换应用（{e}），已打开 DMG，请手动拖入 Applications"
        ));
    }
    let copied = Command::new("/usr/bin/ditto")
        .arg(&stage)
        .arg(&bundle)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = std::fs::remove_dir_all(&stage);
    hdiutil_detach(&mount);
    if !copied {
        let _ = std::fs::rename(&backup, &bundle);
        open_dmg(dmg);
        return Err("替换应用失败，已恢复旧版本并打开 DMG".into());
    }
    let _ = std::fs::remove_dir_all(&backup);

    let _ = Command::new("/usr/bin/open").arg("-n").arg(&bundle).spawn();
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    let dmg = std::env::temp_dir().join(UPDATE_DIR).join(DMG_NAME);
    if !dmg.is_file() {
        return Err("更新包尚未下载".into());
    }
    tauri::async_runtime::spawn_blocking(move || install_blocking(&app, &dmg))
        .await
        .map_err(|e| format!("安装更新失败: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("1.2.3", "1.2.2"));
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(!is_newer("1.2.2", "1.2.2"));
        assert!(!is_newer("v1.2.1", "1.2.2"));
        assert!(is_newer("v2.0", "1.99.99"));
    }

    /// Live check of the GitHub API shape (network, not part of `cargo test`).
    #[tokio::test]
    #[ignore = "hits the GitHub releases API"]
    async fn live_check_reaches_github() {
        let http = reqwest::Client::new();
        let info = check(&http, "0.0.1")
            .await
            .expect("GitHub release check")
            .expect("an update");
        eprintln!("latest release: {info:?}");
        assert!(!info.latest_version.is_empty());
        assert!(info.asset_url.ends_with(".dmg"));
    }

    #[tokio::test]
    #[ignore = "downloads the release DMG"]
    async fn live_download_release_dmg() {
        let http = reqwest::Client::new();
        let info = check(&http, "0.0.1")
            .await
            .expect("GitHub release check")
            .expect("an update");
        let dir = std::env::temp_dir().join("yiterm-update-download-test");
        let path = dir.join("test.dmg");
        let mut last = 0u64;
        download(&http, &info.asset_url, &path, |downloaded, _| {
            last = downloaded;
        })
        .await
        .expect("download");
        let len = tokio::fs::metadata(&path).await.expect("file").len();
        eprintln!(
            "downloaded {len} bytes (last progress {last}) from {}",
            info.asset_url
        );
        assert!(len > 1_000_000);
        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[test]
    fn picks_the_apple_silicon_dmg() {
        let release = json!({
            "assets": [
                {"name": "YiTerm_1.2.3_x64.dmg", "browser_download_url": "https://github.com/x/y"},
                {"name": "YiTerm_1.2.3_aarch64.dmg", "browser_download_url": "https://github.com/a/b"}
            ]
        });
        assert_eq!(
            dmg_asset(&release),
            Some((
                "YiTerm_1.2.3_aarch64.dmg".to_string(),
                "https://github.com/a/b".to_string()
            ))
        );
        assert_eq!(dmg_asset(&json!({"assets": []})), None);
    }
}
