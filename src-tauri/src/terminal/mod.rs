pub mod buffer;

use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
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
            backlog: Mutex::new(Backlog::new(RING_CAP)),
            subs: Mutex::new(Vec::new()),
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(Some(pair.master)),
            child: Mutex::new(Some(child)),
        });

        spawn_reader_thread(handle.clone(), reader);
        Ok(handle)
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
