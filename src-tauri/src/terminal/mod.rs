pub mod buffer;
pub mod plan_approval;

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

use self::{
    buffer::Backlog,
    plan_approval::{
        InputOperation, PlanApprovalStatus, PlanApprovalTracker, PlanExecutionMode,
        TranscriptBoundary,
    },
};

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
    exited: AtomicBool,
    /// Shared with the transcript follower; thinking and tool calls count as replies.
    pub assistant_since_prompt: Arc<AtomicBool>,
    pub plan_approval: Arc<Mutex<PlanApprovalTracker>>,
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
        transcript_boundary: TranscriptBoundary,
    ) -> Result<Arc<Self>, String> {
        let plan_approval = Arc::new(Mutex::new(PlanApprovalTracker::new(
            conv_id.clone(),
            session_id.clone(),
            id,
            transcript_boundary,
            rows,
            cols,
        )));
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

        let started_at_ms = unix_millis();

        let handle = Arc::new(Self {
            id,
            conv_id,
            session_id,
            folder,
            command,
            started_at_ms,
            exited: AtomicBool::new(false),
            assistant_since_prompt: Arc::new(AtomicBool::new(false)),
            plan_approval,
            last_output_ms: AtomicI64::new(unix_millis()),
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
    #[allow(dead_code)] // Raw paste entry point; the composer uses its reservation.
    pub fn paste(&self, text: &str) -> Result<(), String> {
        self.write_bytes(&bracketed_paste(text), None)
    }

    pub fn reserve_input_operation(&self) -> Result<InputOperation, String> {
        InputOperation::begin(self.plan_approval.clone())
    }

    /// Lock order for all input, approval, resize and retirement is tracker
    /// before writer/master. Reservations carry only a revision across awaits.
    fn write_bytes(&self, bytes: &[u8], operation: Option<&InputOperation>) -> Result<(), String> {
        let mut tracker = self.plan_approval.lock().map_err(|e| e.to_string())?;
        if let Some(operation) = operation {
            operation.validate(&tracker)?;
        } else {
            if self.is_exited() {
                return Err("terminal exited".into());
            }
            tracker.invalidate_input();
        }
        let mut guard = self.writer.lock().map_err(|e| e.to_string())?;
        let w = guard.as_mut().ok_or("terminal closed")?;
        w.write_all(bytes)
            .and_then(|_| w.flush())
            .map_err(|e| format!("pty write: {e}"))
    }

    pub fn write_operation(
        &self,
        operation: &InputOperation,
        text: &str,
        submit: bool,
    ) -> Result<(), String> {
        let mut bytes = text.as_bytes().to_vec();
        if submit {
            bytes.push(b'\r');
        }
        self.write_bytes(&bytes, Some(operation))
    }

    pub fn plan_approval_status(
        &self,
        conv_id: &str,
        call_id: &str,
    ) -> Result<PlanApprovalStatus, String> {
        self.plan_approval
            .lock()
            .map_err(|e| e.to_string())?
            .status(conv_id, &self.session_id, self.id, call_id)
    }

    pub fn plan_approval_decide(
        &self,
        conv_id: &str,
        call_id: &str,
        token: u64,
        mode: PlanExecutionMode,
    ) -> Result<(), String> {
        let mut tracker = self.plan_approval.lock().map_err(|e| e.to_string())?;
        let mut writer = self.writer.lock().map_err(|e| e.to_string())?;
        let writer = writer.as_mut().ok_or("terminal closed")?;
        let digit = tracker.consume(conv_id, &self.session_id, self.id, call_id, token, mode)?;
        // A receipt only: the matching transcript result settles approval.
        writer
            .write_all(&[digit])
            .and_then(|_| writer.flush())
            .map_err(|e| format!("pty approval write: {e}"))
    }

    pub fn retire(&self) {
        self.plan_approval
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retire();
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
        let operation = self.reserve_input_operation()?;
        if image_paths.is_empty() {
            return self.submit_text(&operation, text).await;
        }
        let baseline = self.backlog.lock().map_err(|e| e.to_string())?.written();
        self.write_bytes(&bracketed_paste(&image_paths.join("\n")), Some(&operation))?;
        if !self
            .wait_for_image_attach(baseline, IMAGE_ATTACH_TIMEOUT)
            .await
        {
            // Still submit: the path is in Claude's input either way and it can
            // read the file itself. Stranding the turn would be worse.
            tracing::warn!(target: "yiterm", "image attach marker not seen; submitting anyway");
        }
        self.submit_text(&operation, text).await
    }

    /// Types `text`, then presses Enter. A text burst is processed like a
    /// paste by Claude Code (especially when it is long or multi-line), and
    /// an Enter in the same burst is swallowed — the prompt then just sits in
    /// the input box. Write the text first, let the TUI settle, and press
    /// Enter separately.
    async fn submit_text(&self, operation: &InputOperation, text: &str) -> Result<(), String> {
        self.write_operation(operation, text, false)?;
        tokio::time::sleep(ENTER_SETTLE).await;
        self.write_operation(operation, "", true)
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
            if self.is_exited() || start.elapsed() >= timeout {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Writes text to the PTY; `submit` appends `\r` (presses Enter).
    pub fn write(&self, text: &str, submit: bool) -> Result<(), String> {
        let mut bytes = text.as_bytes().to_vec();
        if submit {
            bytes.push(b'\r');
        }
        self.write_bytes(&bytes, None)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let mut tracker = self.plan_approval.lock().map_err(|e| e.to_string())?;
        let guard = self.master.lock().map_err(|e| e.to_string())?;
        let m = guard.as_ref().ok_or("terminal closed")?;
        m.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("pty resize: {e}"))?;
        tracker.resize(rows, cols);
        Ok(())
    }

    pub fn kill(&self) {
        self.retire();
        self.exited.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.child.lock()
            && let Some(child) = guard.as_mut()
        {
            let _ = child.kill();
        }
    }

    /// Last PTY output time (unix millis), for idle reclamation.
    pub fn last_output_ms(&self) -> i64 {
        self.last_output_ms.load(Ordering::Relaxed)
    }

    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// The child's exit code, once it has been reaped.
    pub fn exit_code(&self) -> Option<i32> {
        self.child
            .lock()
            .ok()?
            .as_mut()?
            .wait()
            .ok()
            .map(|status| status.exit_code() as i32)
    }

    /// Replay the backlog into `channel` then subscribe it for live data.
    ///
    /// Ordering guarantee: the flusher holds `subs` while appending to the
    /// backlog and broadcasting; attach also holds `subs` while replaying and
    /// subscribing — so a flush cannot interleave between replay and subscribe
    /// (no gaps, no duplicates).
    pub fn attach(&self, seq: u64, channel: Channel<TerminalEvent>) {
        let mut subs = self.subs.lock().unwrap();
        let (reset, bytes, written) = {
            let backlog = self.backlog.lock().unwrap();
            let (reset, bytes) = backlog.replay_after(seq);
            (reset, bytes, backlog.written())
        };
        if !bytes.is_empty() {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let _ = channel.send(TerminalEvent::Data {
                seq: written,
                b64,
                reset,
            });
        }
        subs.retain(|c| c.id() != channel.id());
        subs.push(channel);
    }

    fn append_and_broadcast(&self, bytes: &[u8]) {
        self.last_output_ms.store(unix_millis(), Ordering::Relaxed);
        // Lock order is always subs → backlog (see `attach`).
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
        self.retire();
        self.exited.store(true, Ordering::SeqCst);
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

/// Unix millis — the clock shared by PTY output stamps, terminal start times
/// and conversation creation times.
pub(crate) fn unix_millis() -> i64 {
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
    let ingest = handle.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    ingest
                        .plan_approval
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .feed_output(&buf[..n]);
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        // Retire immediately at EOF, before the flusher drains or waits for
        // the process. Cloned old handles can no longer send an approval.
        ingest.retire();
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
                    handle.broadcast_exit(code);
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::plan_approval::tests::{call, menu_bytes, observe, tracker};
    use super::*;

    pub(crate) type Writes = Arc<Mutex<Vec<Vec<u8>>>>;

    struct RecordingWriter {
        writes: Writes,
        fail: bool,
    }
    impl Write for RecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes.lock().unwrap().push(bytes.to_vec());
            if self.fail {
                return Err(std::io::Error::other("mock write failure"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    pub(crate) fn mock_terminal(fail: bool) -> (TerminalHandle, Writes) {
        let writes = Arc::new(Mutex::new(Vec::new()));
        (
            TerminalHandle {
                id: 7,
                conv_id: "conv".into(),
                session_id: "sess".into(),
                folder: "/tmp".into(),
                command: "mock".into(),
                started_at_ms: 0,
                exited: AtomicBool::new(false),
                assistant_since_prompt: Arc::new(AtomicBool::new(false)),
                plan_approval: Arc::new(Mutex::new(tracker())),
                last_output_ms: AtomicI64::new(0),
                backlog: Mutex::new(Backlog::new(RING_CAP)),
                subs: Mutex::new(Vec::new()),
                writer: Mutex::new(Some(Box::new(RecordingWriter {
                    writes: writes.clone(),
                    fail,
                }))),
                master: Mutex::new(None),
                child: Mutex::new(None),
            },
            writes,
        )
    }

    fn ready(t: &TerminalHandle, optional: bool) -> u64 {
        let mut tracker = t.plan_approval.lock().unwrap();
        observe(&mut tracker, &call("approval"), 101);
        tracker.feed_output(&menu_bytes(optional));
        tracker
            .status("conv", "sess", 7, "approval")
            .unwrap()
            .token
            .unwrap_or(0)
    }

    #[test]
    fn approval_writes_exact_mapped_digit_once_with_no_enter() {
        for (optional, mode, expected) in [
            (false, PlanExecutionMode::Manual, b'2'),
            (false, PlanExecutionMode::Bypass, b'1'),
            (true, PlanExecutionMode::Manual, b'3'),
            (true, PlanExecutionMode::Bypass, b'2'),
        ] {
            let (terminal, writes) = mock_terminal(false);
            let token = ready(&terminal, optional);
            terminal
                .plan_approval_decide("conv", "approval", token, mode)
                .unwrap();
            assert_eq!(*writes.lock().unwrap(), vec![vec![expected]]);
            assert!(
                !terminal
                    .plan_approval_status("conv", "approval")
                    .unwrap()
                    .available
            );
            assert!(
                terminal
                    .plan_approval_decide("conv", "approval", token, mode)
                    .is_err()
            );
            assert_eq!(writes.lock().unwrap().len(), 1);
        }
    }

    #[test]
    fn simultaneous_decisions_consume_one_claim_and_write_one_digit() {
        let (terminal, writes) = mock_terminal(false);
        let terminal = Arc::new(terminal);
        let token = ready(&terminal, false);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let workers: Vec<_> = [PlanExecutionMode::Manual, PlanExecutionMode::Bypass]
            .into_iter()
            .map(|mode| {
                let terminal = terminal.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    terminal.plan_approval_decide("conv", "approval", token, mode)
                })
            })
            .collect();
        barrier.wait();
        let succeeded = workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(Result::is_ok)
            .count();
        assert_eq!(succeeded, 1);
        let writes = writes.lock().unwrap();
        assert_eq!(writes.len(), 1);
        assert!(writes[0] == b"1" || writes[0] == b"2");
    }

    #[test]
    fn failed_pty_resize_does_not_mutate_the_vt_screen_or_token() {
        let (terminal, _) = mock_terminal(false);
        let token = ready(&terminal, false);
        assert!(terminal.resize(10, 2).is_err());
        assert_eq!(
            terminal
                .plan_approval_status("conv", "approval")
                .unwrap()
                .token,
            Some(token)
        );
    }

    #[test]
    fn write_failure_consumes_claim_without_reporting_approval() {
        let (terminal, writes) = mock_terminal(true);
        let token = ready(&terminal, false);
        assert!(
            terminal
                .plan_approval_decide("conv", "approval", token, PlanExecutionMode::Manual)
                .is_err()
        );
        assert!(
            terminal
                .plan_approval_decide("conv", "approval", token, PlanExecutionMode::Manual)
                .is_err()
        );
        assert_eq!(writes.lock().unwrap().len(), 1);
    }

    #[test]
    fn raw_write_paste_and_retirement_make_prior_claim_unusable() {
        for action in 0..4 {
            let (terminal, writes) = mock_terminal(false);
            let token = ready(&terminal, false);
            match action {
                0 => terminal.write("3", false).unwrap(),
                1 => terminal.paste("feedback").unwrap(),
                2 => terminal.kill(),
                _ => terminal.broadcast_exit(Some(0)),
            }
            let before = writes.lock().unwrap().clone();
            assert!(
                terminal
                    .plan_approval_decide("conv", "approval", token, PlanExecutionMode::Manual)
                    .is_err()
            );
            assert_eq!(*writes.lock().unwrap(), before);
        }
    }

    #[tokio::test]
    async fn composer_rejects_confirmed_approval_before_any_text_or_image_write() {
        for paths in [vec![], vec!["/tmp/mock.png".into()]] {
            let (terminal, writes) = mock_terminal(false);
            ready(&terminal, false);
            assert!(terminal.send_prompt("do not write", &paths).await.is_err());
            assert!(writes.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn focused_feedback_disables_decisions_and_still_blocks_composer() {
        for feedback in ["Tell Claude what to change", "feedback already entered"] {
            let (terminal, writes) = mock_terminal(false);
            let bytes = String::from_utf8(menu_bytes(false))
                .unwrap()
                .replace("❯ 1.", "  1.")
                .replace(
                    "  3. Tell Claude what to change",
                    &format!("❯ 3. {feedback}"),
                );
            {
                let mut tracker = terminal.plan_approval.lock().unwrap();
                observe(&mut tracker, &call("approval"), 101);
                tracker.feed_output(bytes.as_bytes());
            }
            assert!(
                !terminal
                    .plan_approval_status("conv", "approval")
                    .unwrap()
                    .available
            );
            assert!(terminal.send_prompt("not feedback", &[]).await.is_err());
            assert!(writes.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn menu_during_composer_settle_blocks_both_approval_and_delayed_enter() {
        let (terminal, writes) = mock_terminal(false);
        let (sent, ()) = tokio::join!(terminal.send_prompt("draft", &[]), async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let token = ready(&terminal, false);
            assert!(
                terminal
                    .plan_approval_decide("conv", "approval", token, PlanExecutionMode::Manual)
                    .is_err()
            );
        });
        assert!(sent.is_err());
        assert_eq!(*writes.lock().unwrap(), vec![b"draft".to_vec()]);
    }

    #[test]
    fn delayed_escape_reservation_is_cancelled_by_raw_input() {
        let (terminal, writes) = mock_terminal(false);
        let operation = terminal.reserve_input_operation().unwrap();
        terminal.write("x", false).unwrap();
        assert!(terminal.write_operation(&operation, "\x1b", false).is_err());
        assert_eq!(*writes.lock().unwrap(), vec![b"x".to_vec()]);
    }

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
        assert!(image_attach_marker(
            b"prefix\x1b[2K[Image\x1b[10G#12]suffix"
        ));
    }
}
