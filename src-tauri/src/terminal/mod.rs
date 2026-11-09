pub mod buffer;

use std::{
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::Duration,
};

use base64::Engine;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::ipc::Channel;

use self::buffer::Backlog;

const RING_CAP: usize = 1024 * 1024; // 1 MiB backlog
const FLUSH_INTERVAL: Duration = Duration::from_millis(10);
const FLUSH_SIZE: usize = 64 * 1024;
/// Claude Code reads + resizes a pasted image off-thread; allow for big screenshots.
const IMAGE_ATTACH_TIMEOUT: Duration = Duration::from_secs(8);
/// Grace period between seeing the marker and pressing Enter. The marker is
/// rendered before Claude clears its paste-in-progress flag, so Enter must not
/// follow too closely or it is swallowed.
const IMAGE_ATTACH_SETTLE: Duration = Duration::from_millis(400);
/// Text arrives as one burst, which Claude Code handles like a paste; an
/// Enter in the same burst is swallowed. Give the TUI a beat, then press
/// Enter on its own.
const ENTER_SETTLE: Duration = Duration::from_millis(250);

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TerminalEvent {
    /// `seq` = total bytes written to this terminal after this batch.
    /// `reset` = consumer should clear its view first (bytes start mid-stream).
    Data {
        seq: u64,
        b64: String,
        reset: bool,
    },
    Exit {
        code: Option<i32>,
    },
}

#[allow(dead_code)] // conv_id/folder/started_at_ms kept for diagnostics & future UI use
pub struct TerminalHandle {
    pub id: u64,
    pub conv_id: String,
    pub session_id: String,
    pub folder: String,
    pub command: String,
    pub started_at_ms: i64,
    pub exited: Mutex<bool>,
    /// Shared with the transcript follower; thinking and tool calls count as replies.
    pub assistant_since_prompt: Arc<AtomicBool>,
    /// When the PTY last produced output (unix millis). The idle-terminal
    /// reclaimer treats recent output as "still running".
    last_output_ms: AtomicI64,
    backlog: Mutex<Backlog>,
    subs: Mutex<Vec<Channel<TerminalEvent>>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    child: Mutex<Option<Box<dyn Child + Send>>>,
}

impl TerminalHandle {
    /// Spawns `claude` inside a PTY rooted at `folder`.
    ///
    /// `resume` decides between `--session-id <id>` (fresh session, the JSONL
    /// path is predictable) and `--resume <id>` (continue existing session;
    /// never `--fork-session`, which would mint a new id and break the path).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        id: u64,
        conv_id: String,
        folder: String,
        session_id: String,
        claude_path: &str,
        resume: bool,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Self>, String> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("openpty: {e}"))?;

        let mut cmd = CommandBuilder::new(claude_path);
        if resume {
            cmd.arg("--resume");
            cmd.arg(&session_id);
        } else {
            cmd.arg("--session-id");
            cmd.arg(&session_id);
        }
        cmd.cwd(&folder);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // If YiTerm itself was launched from a Claude Code session (dev
        // workflows, terminal multiplexers), the child would inherit the
        // parent's session markers — most notably CLAUDE_CODE_CHILD_SESSION,
        // which disables transcript saving and silently breaks the JSONL
        // follower. Strip per-session markers; keep auth/provider vars.
        for var in [
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_PID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
        ] {
            cmd.env_remove(var);
        }
        // Belt and braces: even if another marker slips through, ask claude
        // to persist the transcript — the chat panel depends on the JSONL.
        cmd.env("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE", "1");

        let command = format!(
            "claude {} {}",
            if resume { "--resume" } else { "--session-id" },
            session_id
        );

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("spawn claude: {e}"))?;
        // Drop the slave so EOF propagates when the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("clone reader: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("take writer: {e}"))?;

        let started_at_ms = chrono_millis();

        let handle = Arc::new(Self {
            id,
            conv_id,
            session_id,
            folder,
            command,
            started_at_ms,
            exited: Mutex::new(false),
            assistant_since_prompt: Arc::new(AtomicBool::new(false)),
            last_output_ms: AtomicI64::new(chrono_millis()),
            backlog: Mutex::new(Backlog::new(RING_CAP)),
            subs: Mutex::new(Vec::new()),
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(Some(pair.master)),
            child: Mutex::new(Some(child)),
        });

        spawn_reader_thread(handle.clone(), reader);
        Ok(handle)
    }

    /// Writes `text` as a bracketed paste, without submitting.
    ///
    /// Claude Code only turns a pasted image *path* into a real `[Image #N]`
    /// attachment inside its paste handler — the same path arriving as typed
    /// keys is left as literal text. So image paths must use this, not `write`.
    pub fn paste(&self, text: &str) -> Result<(), String> {
        if *self.exited.lock().map_err(|e| e.to_string())? {
            return Err("terminal exited".into());
        }
        let mut guard = self.writer.lock().map_err(|e| e.to_string())?;
        let w = guard.as_mut().ok_or("terminal closed")?;
        w.write_all(&bracketed_paste(text))
            .and_then(|_| w.flush())
            .map_err(|e| format!("pty paste: {e}"))
    }

    /// Sends one composer turn: optional text plus zero or more image paths.
    ///
    /// Ordering matters. Claude Code reads and resizes a pasted image
    /// asynchronously, and while that is in flight it swallows Enter and never
    /// re-issues it — a `\r` written in the same burst as the image paste leaves
    /// the prompt sitting unsent in its input box. So: paste the paths, wait for
    /// the `[Image #N]` marker to come back on the PTY, then type the text and
    /// press Enter through `submit_text`.
    pub async fn send_prompt(&self, text: &str, image_paths: &[String]) -> Result<(), String> {
        if image_paths.is_empty() {
            return self.submit_text(text).await;
        }
        let baseline = self.backlog.lock().map_err(|e| e.to_string())?.written();
        self.paste(&image_paths.join("\n"))?;
        if !self.wait_for_image_attach(baseline, IMAGE_ATTACH_TIMEOUT).await {
            // Still submit: the path is in Claude's input either way and it can
            // read the file itself. Stranding the turn would be worse.
            tracing::warn!(target: "yiterm", "image attach marker not seen; submitting anyway");
        }
        self.submit_text(text).await
    }

    /// Types `text`, then presses Enter. A text burst is processed like a
    /// paste by Claude Code (especially when it is long or multi-line), and
    /// an Enter in the same burst is swallowed — the prompt then just sits in
    /// the input box. Write the text first, let the TUI settle, and press
    /// Enter separately.
    async fn submit_text(&self, text: &str) -> Result<(), String> {
        self.write(text, false)?;
        tokio::time::sleep(ENTER_SETTLE).await;
        self.write("", true)
    }

    /// Polls the backlog for the attachment marker rendered after a paste.
    async fn wait_for_image_attach(&self, baseline: u64, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        loop {
            // Scoped so the std guard is dropped before any await: this future
            // must stay `Send` for Tauri's async command.
            let attached = {
                let backlog = self.backlog.lock().unwrap_or_else(|e| e.into_inner());
                let (_, bytes) = backlog.replay_after(baseline);
                image_attach_marker(&bytes)
            };
            if attached {
                // Let the render settle and Claude's paste-in-progress flag
                // clear before Enter goes out.
                tokio::time::sleep(IMAGE_ATTACH_SETTLE).await;
                return true;
            }
            let exited = *self.exited.lock().unwrap_or_else(|e| e.into_inner());
            if exited || start.elapsed() >= timeout {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Writes text to the PTY; `submit` appends `\r` (presses Enter).
    pub fn write(&self, text: &str, submit: bool) -> Result<(), String> {
        if *self.exited.lock().map_err(|e| e.to_string())? {
            return Err("terminal exited".into());
        }
        let mut guard = self.writer.lock().map_err(|e| e.to_string())?;
        let w = guard.as_mut().ok_or("terminal closed")?;
        w.write_all(text.as_bytes())
            .and_then(|_| {
                if submit {
                    w.write_all(b"\r")
                } else {
                    Ok(())
                }
            })
            .and_then(|_| w.flush())
            .map_err(|e| format!("pty write: {e}"))
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let guard = self.master.lock().map_err(|e| e.to_string())?;
        let m = guard.as_ref().ok_or("terminal closed")?;
        m.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("pty resize: {e}"))
    }

    pub fn kill(&self) {
        if let Ok(mut guard) = self.child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
    }

    /// Last PTY output time (unix millis), for idle reclamation.
    pub fn last_output_ms(&self) -> i64 {
        self.last_output_ms.load(Ordering::Relaxed)
    }

    pub fn is_exited(&self) -> bool {
        *self.exited.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn exit_code(&self) -> Option<Option<i32>> {
        self.child
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().map(|c| c.wait().ok().map(|s| s.exit_code() as i32)))
    }

    /// Replay the backlog into `channel` then subscribe it for live data.
    ///
    /// Ordering guarantee: the flusher holds `subs` while appending to the
    /// backlog and broadcasting; attach also holds `subs` while replaying and
    /// subscribing — so a flush cannot interleave between replay and subscribe
    /// (no gaps, no duplicates).
    pub fn attach(&self, seq: u64, channel: Channel<TerminalEvent>) {
        let mut subs = self.subs.lock().unwrap();
        let (reset, bytes) = {
            let backlog = self.backlog.lock().unwrap();
            backlog.replay_after(seq)
        };
        if !bytes.is_empty() {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let _ = channel.send(TerminalEvent::Data {
                seq: self.backlog.lock().unwrap().written(),
                b64,
                reset,
            });
        }
        subs.retain(|c| c.id() != channel.id());
        subs.push(channel);
    }

    fn append_and_broadcast(&self, bytes: &[u8]) {
        self.last_output_ms.store(chrono_millis(), Ordering::Relaxed);
        // Caller holds `subs` lock; lock order is always subs → backlog.
        let mut subs = self.subs.lock().unwrap();
        let seq = {
            let mut backlog = self.backlog.lock().unwrap();
            backlog.append(bytes)
        };
        if subs.is_empty() {
            return;
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let event = TerminalEvent::Data {
            seq,
            b64,
            reset: false,
        };
        subs.retain(|c| c.send(event.clone()).is_ok());
    }

    fn broadcast_exit(&self, code: Option<i32>) {
        if let Ok(mut exited) = self.exited.lock() {
            *exited = true;
        }
        if let Ok(mut subs) = self.subs.lock() {
            subs.retain(|c| c.send(TerminalEvent::Exit { code }).is_ok());
            subs.clear();
        }
    }
}

/// Frames `text` as a bracketed paste (the terminal protocol Claude Code enables
/// with `ESC [?2004h`). Deliberately carries no `\r`.
fn bracketed_paste(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(text.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// True once Claude Code has rendered an attached-image marker.
///
/// The TUI draws it with cursor positioning rather than a literal space —
/// `[Image`, `ESC [10G`, `#1]` — so escapes and whitespace are both dropped
/// before matching, leaving `[Image#1]`.
fn image_attach_marker(bytes: &[u8]) -> bool {
    let mut plain = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            i += 1;
            match bytes.get(i) {
                // CSI: params/intermediates are all < 0x40, final byte is 0x40..=0x7e.
                Some(b'[') => {
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                // OSC: terminated by BEL or ST.
                Some(b']') => {
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                // Charset designation takes one more byte.
                Some(b'(' | b')' | b'*' | b'+') => i += 2,
                // Any other two-byte escape (ESC 7, ESC =, ESC >, ...).
                Some(_) => i += 1,
                None => break,
            }
            continue;
        }
        if b > 0x20 && b != 0x7f {
            plain.push(b);
        }
        i += 1;
    }
    plain.windows(7).any(|w| w == b"[Image#")
}

fn chrono_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// PTY reads are blocking, so the reader lives on a dedicated OS thread and
/// hands chunks to a flusher thread which coalesces them (~10 ms / 64 KiB)
/// before appending to the ring and broadcasting to subscribers.
fn spawn_reader_thread(handle: Arc<TerminalHandle>, mut reader: Box<dyn Read + Send>) {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // Dropping tx closes the channel → flusher drains and exits.
    });

    std::thread::spawn(move || {
        let mut pending: Vec<u8> = Vec::new();
        loop {
            match rx.recv_timeout(FLUSH_INTERVAL) {
                Ok(chunk) => {
                    pending.extend_from_slice(&chunk);
                    if pending.len() >= FLUSH_SIZE {
                        handle.append_and_broadcast(&pending);
                        pending.clear();
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if !pending.is_empty() {
                        handle.append_and_broadcast(&pending);
                        pending.clear();
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    if !pending.is_empty() {
                        handle.append_and_broadcast(&pending);
                        pending.clear();
                    }
                    let code = handle.exit_code();
                    tracing::info!(terminal = handle.id, ?code, "terminal exited");
                    handle.broadcast_exit(code.flatten());
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_frames_without_submitting() {
        assert_eq!(
            bracketed_paste("/tmp/a.png"),
            b"\x1b[200~/tmp/a.png\x1b[201~".to_vec()
        );
        // No CR anywhere: Enter must be a separate, later write.
        assert!(!bracketed_paste("/tmp/a.png").contains(&b'\r'));
    }

    #[test]
    fn paste_keeps_spaces_and_multiple_paths_intact() {
        let paths = "/Users/x/Application Support/app/images/c/a.png\n/tmp/b.png";
        let framed = bracketed_paste(paths);
        assert_eq!(&framed[6..framed.len() - 6], paths.as_bytes());
    }

    #[test]
    fn marker_matches_real_claude_code_output() {
        // Verbatim bytes captured from Claude Code 2.1.316 after a bracketed
        // paste of an image path: the space in "[Image #1]" is a cursor move.
        let raw = b"\x1b(B\x0f\x1b[?2004h\x1b[2D\x1b[3B\r\x1b[2C\x1b[3A[Image\x1b[10G#1]\x1b[7m \r\x1b[47C\x1b[2B\x1b[27m";
        assert!(image_attach_marker(raw));
    }

    #[test]
    fn marker_ignores_unrelated_output() {
        assert!(!image_attach_marker(b"\x1b[?2004h\r\n hello world \x1b[0m"));
        assert!(!image_attach_marker(b""));
        // A truncated escape must not panic or run off the end.
        assert!(!image_attach_marker(b"\x1b["));
        assert!(!image_attach_marker(b"\x1b"));
        assert!(!image_attach_marker(b"\x1b]0;title"));
    }

    #[test]
    fn marker_matches_plain_and_later_numbering() {
        // Whitespace is dropped too, so both the cursor-move form and a literal
        // space render match.
        assert!(image_attach_marker(b"[Image #1]"));
        assert!(image_attach_marker(b"[Image#2]"));
        assert!(image_attach_marker(b"prefix\x1b[2K[Image\x1b[10G#12]suffix"));
    }
}
