//! Runtime-only proof that a main-session ExitPlanMode call is at a live menu.
//! All input paths take this lock before the PTY writer; no guard crosses await.

use std::{
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sessions::{ChatMsg, Role, is_interrupt_marker, is_synthetic_no_response};

const PROMPT: &str =
    "Claude has written up a plan and is ready to execute. Would you like to proceed?";
const MANUAL: &str = "Yes, manually approve edits";
const BYPASS: &str = "Yes, and switch to BYPASS PERMISSIONS (no further prompts) for this session";
const FEEDBACK: &str = "Tell Claude what to change";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PlanExecutionMode {
    Manual,
    Bypass,
}

#[derive(Debug, Serialize)]
pub struct PlanApprovalStatus {
    pub available: bool,
    pub token: Option<u64>,
    pub modes: Vec<PlanExecutionMode>,
}

impl PlanApprovalStatus {
    fn unavailable() -> Self {
        Self {
            available: false,
            token: None,
            modes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl FileIdentity {
    pub fn of(meta: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                dev: meta.dev(),
                ino: meta.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            Self {}
        }
    }
}

/// Captured before spawning the process, not at the follower's first poll.
#[derive(Clone)]
pub struct TranscriptBoundary {
    path: PathBuf,
    pub len: u64,
    pub identity: Option<FileIdentity>,
}

impl TranscriptBoundary {
    pub fn capture(path: PathBuf) -> Result<Self, String> {
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => Some(meta),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("transcript metadata: {e}")),
        };
        Ok(Self {
            path,
            len: meta.as_ref().map_or(0, std::fs::Metadata::len),
            identity: meta.as_ref().map(FileIdentity::of),
        })
    }
}

#[derive(Debug)]
struct Candidate {
    call_id: String,
    /// A historical call still requires a menu rendered by this incarnation.
    historical: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Menu {
    manual: u8,
    bypass: Option<u8>,
}

impl Menu {
    fn digit(&self, mode: PlanExecutionMode) -> Option<u8> {
        match mode {
            PlanExecutionMode::Manual => Some(self.manual),
            PlanExecutionMode::Bypass => self.bypass,
        }
    }
}

pub struct PlanApprovalTracker {
    conv_id: String,
    session_id: String,
    terminal_id: u64,
    boundary: TranscriptBoundary,
    observed_through: u64,
    suppress_through: u64,
    seen_calls: HashSet<String>,
    resolved_calls: HashSet<String>,
    assistant_snapshots: HashMap<String, u64>,
    candidate: Option<Candidate>,
    parser: vt100::Parser,
    menu: Option<Menu>,
    // After input/reset, an unchanged old menu cannot become fresh proof.
    menu_armed: bool,
    retired: bool,
    token: u64,
    input_revision: u64,
    input_operations: usize,
}

impl PlanApprovalTracker {
    pub fn new(
        conv_id: String,
        session_id: String,
        terminal_id: u64,
        boundary: TranscriptBoundary,
        rows: u16,
        cols: u16,
    ) -> Self {
        Self {
            conv_id,
            session_id,
            terminal_id,
            boundary,
            observed_through: 0,
            suppress_through: 0,
            seen_calls: HashSet::new(),
            resolved_calls: HashSet::new(),
            assistant_snapshots: HashMap::new(),
            candidate: None,
            parser: vt100::Parser::new(rows.max(1), cols.max(1), 0),
            menu: None,
            menu_armed: true,
            retired: false,
            token: 1,
            input_revision: 0,
            input_operations: 0,
        }
    }

    pub fn boundary(&self) -> &TranscriptBoundary {
        &self.boundary
    }

    fn advance(&mut self) {
        // Exhaustion retires the handle rather than ever recycling a claim.
        if let Some(next) = self.token.checked_add(1) {
            self.token = next;
        } else {
            self.retired = true;
        }
    }

    pub fn feed_output(&mut self, bytes: &[u8]) {
        if self.retired {
            return;
        }
        self.parser.process(bytes);
        self.menu = recognize_menu(self.parser.screen());
        if self.menu.is_none() {
            self.menu_armed = true;
        }
        // Also reject claims issued before a redraw or partially received CSI.
        self.advance();
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let had_menu = self.menu.is_some();
        self.parser.screen_mut().set_size(rows.max(1), cols.max(1));
        self.menu = None;
        self.menu_armed = !had_menu;
        self.advance();
    }

    pub fn retire(&mut self) {
        self.retired = true;
        self.candidate = None;
        self.menu = None;
        self.input_revision = self.input_revision.wrapping_add(1);
        self.advance();
    }

    pub fn reset_transcript(&mut self, len: u64, identity: Option<FileIdentity>) {
        self.boundary.len = len;
        self.boundary.identity = identity;
        self.observed_through = 0;
        self.suppress_through = len;
        self.seen_calls.clear();
        self.resolved_calls.clear();
        self.assistant_snapshots.clear();
        self.candidate = None;
        self.menu_armed = false;
        self.input_revision = self.input_revision.wrapping_add(1);
        self.advance();
    }

    /// Called under the same lock as the subsequent raw write. Stat includes
    /// records already on disk but not yet read by the 300 ms follower.
    pub fn invalidate_input(&mut self) {
        self.candidate = None;
        self.menu_armed = false;
        self.input_revision = self.input_revision.wrapping_add(1);
        self.suppress_through = self.suppress_through.max(self.observed_through);
        match std::fs::metadata(&self.boundary.path) {
            Ok(meta) => {
                self.suppress_through = self
                    .suppress_through
                    .max(meta.len())
                    .max(self.observed_through);
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound && self.boundary.identity.is_none() => {
            }
            // Without a readable boundary we cannot safely classify catch-up.
            Err(_) => {
                self.suppress_through = u64::MAX;
            }
        }
        self.advance();
    }

    /// Reduce exact main-session call/result identities, independently of the
    /// display follower's batch-wide `historical` flag.
    pub fn observe_record(&mut self, v: &Value, parsed: &[ChatMsg], start: u64, end: u64) {
        self.observed_through = self.observed_through.max(end);
        if self.retired || v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        if v.get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|id| id != self.session_id)
        {
            return;
        }
        let kind = v.get("type").and_then(Value::as_str);
        if !matches!(kind, Some("user" | "assistant")) {
            return;
        }
        for result in parsed.iter().filter_map(|m| m.tool_result.as_ref()) {
            self.resolved_calls.insert(result.call_id.clone());
            if self
                .candidate
                .as_ref()
                .is_some_and(|c| c.call_id == result.call_id)
            {
                self.candidate = None;
                self.advance();
            }
        }
        if kind == Some("user") {
            let interrupted = v.pointer("/message/content").is_some_and(|c| {
                c.as_str().is_some_and(is_interrupt_marker)
                    || c.as_array().is_some_and(|a| {
                        a.iter().any(|b| {
                            b.get("text")
                                .and_then(Value::as_str)
                                .is_some_and(is_interrupt_marker)
                        })
                    })
            });
            if interrupted
                || parsed
                    .iter()
                    .any(|m| m.role == Role::User && m.tool_result.is_none())
            {
                self.candidate = None;
                self.advance();
            }
            return;
        }
        let Some(message) = v.get("message") else {
            return;
        };
        if is_synthetic_no_response(message)
            || message
                .get("stop_reason")
                .and_then(Value::as_str)
                .is_some_and(|stop| stop != "tool_use")
        {
            self.candidate = None;
            self.advance();
            return;
        }
        let Some(msg) = parsed.iter().find(|m| m.role == Role::Assistant) else {
            return;
        };
        // Hash the JSON value directly: historical tool inputs/thinking can
        // be large, and deduplication must not retain a second transcript.
        let mut hasher = DefaultHasher::new();
        message.hash(&mut hasher);
        let fingerprint = hasher.finish();
        if self.assistant_snapshots.get(&msg.key) == Some(&fingerprint) {
            return;
        }
        self.assistant_snapshots
            .insert(msg.key.clone(), fingerprint);
        let calls: Vec<_> = msg
            .tool_calls
            .iter()
            .filter(|c| c.name == "ExitPlanMode")
            .collect();
        if calls.len() == 1 && msg.tool_calls.len() == 1 {
            let id = &calls[0].id;
            if !id.is_empty() && self.seen_calls.insert(id.clone()) {
                self.candidate =
                    if start >= self.suppress_through && !self.resolved_calls.contains(id) {
                        Some(Candidate {
                            call_id: id.clone(),
                            historical: start < self.boundary.len,
                        })
                    } else {
                        None
                    };
                self.advance();
            }
        } else {
            // New text, thinking, another tool, or an end-turn closes the claim.
            self.candidate = None;
            self.advance();
        }
    }

    fn validate_owner(
        &self,
        conv_id: &str,
        session_id: &str,
        terminal_id: u64,
    ) -> Result<(), String> {
        if self.conv_id != conv_id
            || self.session_id != session_id
            || self.terminal_id != terminal_id
        {
            return Err("计划所属的终端已变化，请在终端确认当前状态。".into());
        }
        Ok(())
    }

    pub fn status(
        &self,
        conv_id: &str,
        session_id: &str,
        terminal_id: u64,
        call_id: &str,
    ) -> Result<PlanApprovalStatus, String> {
        self.validate_owner(conv_id, session_id, terminal_id)?;
        if self.retired
            || !self.menu_armed
            || self.input_operations != 0
            || !self
                .candidate
                .as_ref()
                .is_some_and(|c| c.call_id == call_id)
        {
            return Ok(PlanApprovalStatus::unavailable());
        }
        let Some(menu) = &self.menu else {
            return Ok(PlanApprovalStatus::unavailable());
        };
        let mut modes = vec![PlanExecutionMode::Manual];
        if menu.bypass.is_some() {
            modes.push(PlanExecutionMode::Bypass);
        }
        Ok(PlanApprovalStatus {
            available: true,
            token: Some(self.token),
            modes,
        })
    }

    /// Consume before writing, including on an I/O failure: retrying a possibly
    /// delivered digit must never answer a subsequent CLI prompt.
    pub fn consume(
        &mut self,
        conv_id: &str,
        session_id: &str,
        terminal_id: u64,
        call_id: &str,
        token: u64,
        mode: PlanExecutionMode,
    ) -> Result<u8, String> {
        let status = self.status(conv_id, session_id, terminal_id, call_id)?;
        if !status.available || status.token != Some(token) {
            return Err("计划确认提示已变化，请在终端继续。".into());
        }
        let digit = self
            .menu
            .as_ref()
            .and_then(|m| m.digit(mode))
            .ok_or("当前无法选择此执行方式，请在终端继续。")?;
        tracing::debug!(
            terminal = self.terminal_id,
            historical = self.candidate.as_ref().is_some_and(|c| c.historical),
            ?mode,
            "consuming plan approval choice"
        );
        self.invalidate_input();
        Ok(digit)
    }

    pub fn confirmed_menu(&self) -> bool {
        if self.retired {
            return false;
        }
        // Feedback focus disables numeric approval, but it must still block
        // composer text/images from being injected into that feedback editor.
        plan_menu_options(self.parser.screen(), true)
            .is_some_and(|options| options.iter().filter(|o| o.focused).count() == 1)
    }
}

/// Reserves the whole multi-write operation, including image waits and delayed
/// Escape clearing. Raw input can cancel its revision without waiting for it.
pub struct InputOperation {
    tracker: Arc<Mutex<PlanApprovalTracker>>,
    revision: u64,
}

impl InputOperation {
    pub fn begin(tracker: Arc<Mutex<PlanApprovalTracker>>) -> Result<Self, String> {
        let revision = {
            let mut t = tracker.lock().map_err(|e| e.to_string())?;
            if t.retired {
                return Err("终端已退出，无法继续输入。".into());
            }
            if t.confirmed_menu() {
                return Err("计划正在等待确认，请先选择执行方式，或在终端提出修改意见。".into());
            }
            if t.input_operations != 0 {
                return Err("终端正在处理上一条输入，请稍后重试。".into());
            }
            t.invalidate_input();
            t.input_operations += 1;
            t.input_revision
        };
        Ok(Self { tracker, revision })
    }

    pub fn validate(&self, tracker: &PlanApprovalTracker) -> Result<(), String> {
        if tracker.retired || tracker.input_revision != self.revision {
            return Err("终端输入已中断。".into());
        }
        if tracker.confirmed_menu() {
            return Err("计划正在等待确认，请在终端继续。".into());
        }
        Ok(())
    }
}

impl Drop for InputOperation {
    fn drop(&mut self) {
        let mut tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
        tracker.input_operations -= 1;
        tracker.advance();
    }
}

#[derive(Debug)]
struct OptionRow {
    digit: u8,
    focused: bool,
    label: String,
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn option_start(text: &str) -> Option<OptionRow> {
    let text = text.trim();
    let (focused, text) = match text.strip_prefix('❯') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, text),
    };
    let bytes = text.as_bytes();
    if bytes.len() < 4 || !(b'1'..=b'9').contains(&bytes[0]) || bytes[1] != b'.' || bytes[2] != b' '
    {
        return None;
    }
    Some(OptionRow {
        digit: bytes[0],
        focused,
        label: normalized(&text[3..]),
    })
}

/// Only exact labels from the inspected 2.1.316 plan selector are supported.
fn ordinary_label(label: &str) -> bool {
    matches!(
        label,
        MANUAL
            | BYPASS
            | "Yes, auto-accept edits"
            | "Yes, and use auto mode"
            | "No, refine with Ultraplan in a cloud session"
    ) || clear_context_label(label)
}

fn clear_context_label(label: &str) -> bool {
    let Some(rest) = label.strip_prefix("Yes, clear context") else {
        return false;
    };
    let rest = if let Some(rest) = rest.strip_prefix(" (") {
        let Some((percent, tail)) = rest.split_once("% used)") else {
            return false;
        };
        if percent.parse::<u8>().ok().is_none_or(|n| n > 100) {
            return false;
        }
        tail
    } else {
        rest
    };
    matches!(
        rest,
        " and bypass permissions" | " and auto-accept edits" | " and use auto mode"
    )
}

fn logical_rows(screen: &vt100::Screen) -> Vec<String> {
    let (_, width) = screen.size();
    // Join true VT wraps without adding a space inside words. Ink word wraps
    // remain separate rows and are normalized with a space below.
    let mut lines: Vec<String> = Vec::new();
    for (row, text) in screen.rows(0, width).enumerate() {
        if row > 0 && screen.row_wrapped(row as u16 - 1) {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(&text);
            }
        } else {
            lines.push(text);
        }
    }
    lines
}

fn plan_menu_options(
    screen: &vt100::Screen,
    allow_focused_feedback: bool,
) -> Option<Vec<OptionRow>> {
    let lines = logical_rows(screen);
    let mut prompt_end = None;
    for (i, text) in lines.iter().enumerate() {
        if !text.trim().starts_with("Claude has written up a plan") {
            continue;
        }
        let mut prompt = String::new();
        for (j, line) in lines.iter().enumerate().skip(i) {
            if !prompt.is_empty() {
                prompt.push(' ');
            }
            prompt.push_str(line.trim());
            if normalized(&prompt) == PROMPT {
                if prompt_end.replace(j).is_some() {
                    return None;
                }
                break;
            }
            if !PROMPT.starts_with(&normalized(&prompt)) {
                break;
            }
        }
    }
    let prompt_end = prompt_end?;
    let mut options: Vec<OptionRow> = Vec::new();
    let mut feedback = false;
    for text in lines.iter().skip(prompt_end + 1) {
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(option) = option_start(text) {
            if feedback || option.digit != b'1' + options.len() as u8 {
                return None;
            }
            if let Some(prev) = options.last() {
                if !ordinary_label(&prev.label) {
                    return None;
                }
            }
            // Once the exact ordinary rows include manual approval, an
            // additional focused input row can contain arbitrary feedback,
            // including numbered/multiline text. It blocks composer writes;
            // it can never supply numeric execution choices.
            if allow_focused_feedback
                && option.focused
                && !ordinary_label(&option.label)
                && options.iter().any(|o| o.label == MANUAL)
                && options.iter().all(|o| !o.focused)
                && options.len() < 5
            {
                options.push(option);
                return Some(options);
            }
            feedback = option.label == FEEDBACK;
            options.push(option);
        } else if feedback {
            // Known editor/save hints below the selector do not add choices.
            if !(text.starts_with("ctrl+g to edit in ")
                || text.starts_with("shift+tab to approve with this feedback")
                || text == "Plan saved!")
            {
                return None;
            }
        } else {
            let option = options.last_mut()?;
            option.label.push(' ');
            option.label.push_str(text);
            option.label = normalized(&option.label);
            feedback = option.label == FEEDBACK;
        }
    }
    if !feedback
        || options.len() < 2
        || options.len() > 5
        || !options.iter().any(|o| o.label == MANUAL)
    {
        return None;
    }
    Some(options)
}

fn recognize_menu(screen: &vt100::Screen) -> Option<Menu> {
    let options = plan_menu_options(screen, false)?;
    let focused: Vec<_> = options.iter().filter(|o| o.focused).collect();
    if focused.len() != 1 || !ordinary_label(&focused[0].label) {
        return None;
    }
    // Ordinary-row focus is the selector's ❯ marker. Ink's physical cursor
    // can be parked below the menu and is not a focus signal.
    let manual: Vec<_> = options.iter().filter(|o| o.label == MANUAL).collect();
    let bypass: Vec<_> = options.iter().filter(|o| o.label == BYPASS).collect();
    if manual.len() != 1 || bypass.len() > 1 {
        return None;
    }
    Some(Menu {
        manual: manual[0].digit,
        bypass: bypass.first().map(|o| o.digit),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sessions::parse_line;
    use serde_json::json;
    use std::path::Path;

    pub(crate) fn menu_bytes(optional: bool) -> Vec<u8> {
        let rows = if optional {
            format!(
                "  1. Yes, clear context (42% used) and bypass permissions\r\n❯ 2. {BYPASS}\r\n  3. {MANUAL}\r\n  4. No, refine with Ultraplan in a cloud session\r\n  5. {FEEDBACK}"
            )
        } else {
            format!("❯ 1. {BYPASS}\r\n  2. {MANUAL}\r\n  3. {FEEDBACK}")
        };
        let cursor_row = if optional { 5 } else { 4 };
        format!("\x1b[2J\x1b[H{PROMPT}\r\n\r\n\r\n{rows}\x1b[{cursor_row};1H").into_bytes()
    }

    pub(crate) fn tracker() -> PlanApprovalTracker {
        PlanApprovalTracker::new(
            "conv".into(),
            "sess".into(),
            7,
            TranscriptBoundary {
                path: Path::new("/nonexistent/yiterm-test.jsonl").into(),
                len: 100,
                identity: None,
            },
            30,
            120,
        )
    }

    pub(crate) fn call(id: &str) -> Value {
        json!({"type":"assistant", "sessionId":"sess", "message":{"id":format!("msg-{id}"),"content":[{"type":"tool_use","id":id,"name":"ExitPlanMode","input":{"plan":"test plan"}}],"stop_reason":"tool_use"}})
    }

    pub(crate) fn observe(t: &mut PlanApprovalTracker, record: &Value, start: u64) {
        t.observe_record(record, &parse_line(record), start, start + 10);
    }

    fn available(t: &PlanApprovalTracker, id: &str) -> bool {
        t.status("conv", "sess", 7, id).unwrap().available
    }

    #[test]
    fn split_ansi_utf8_and_erase_use_current_visible_screen() {
        let bytes = menu_bytes(false);
        for split in 0..=bytes.len() {
            let mut t = tracker();
            observe(&mut t, &call("a"), 101);
            t.feed_output(&bytes[..split]);
            t.feed_output(&bytes[split..]);
            assert!(available(&t, "a"), "split {split}");
            t.feed_output(b"\x1b[2J\x1b[Hgone");
            assert!(!available(&t, "a"));
        }
    }

    #[test]
    fn optional_rows_map_meaning_to_actual_digits() {
        let mut t = tracker();
        observe(&mut t, &call("a"), 101);
        t.feed_output(&menu_bytes(true));
        let status = t.status("conv", "sess", 7, "a").unwrap();
        assert_eq!(
            status.modes,
            [PlanExecutionMode::Manual, PlanExecutionMode::Bypass]
        );
        assert_eq!(
            t.consume(
                "conv",
                "sess",
                7,
                "a",
                status.token.unwrap(),
                PlanExecutionMode::Manual
            )
            .unwrap(),
            b'3'
        );
    }

    #[test]
    fn call_and_menu_can_arrive_in_either_order_with_mixed_history() {
        for menu_first in [false, true] {
            let mut t = tracker();
            if menu_first {
                t.feed_output(&menu_bytes(false));
            }
            observe(&mut t, &call("old"), 10);
            assert_eq!(t.candidate.as_ref().unwrap().historical, true);
            observe(
                &mut t,
                &json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"old","is_error":true}]}}),
                30,
            );
            observe(&mut t, &call("new"), 110);
            assert!(!t.candidate.as_ref().unwrap().historical);
            if !menu_first {
                assert!(!available(&t, "new"));
                t.feed_output(&menu_bytes(false));
            }
            assert!(!available(&t, "old"));
            assert!(available(&t, "new"));
        }
        let mut historical = tracker();
        observe(&mut historical, &call("a"), 10);
        assert!(!available(&historical, "a"));
        historical.feed_output(&menu_bytes(false));
        assert!(available(&historical, "a"));
    }

    #[test]
    fn exact_results_duplicates_and_new_progress_reduce_safely() {
        let mut t = tracker();
        observe(&mut t, &call("a"), 101);
        t.feed_output(&menu_bytes(false));
        let token = t.token;
        observe(&mut t, &call("a"), 120);
        assert_eq!(t.token, token);
        let mut ignored = call("ignored");
        ignored["isSidechain"] = json!(true);
        observe(&mut t, &ignored, 130);
        ignored["type"] = json!("api-request-blob");
        ignored["isSidechain"] = json!(false);
        observe(&mut t, &ignored, 140);
        assert!(available(&t, "a"));
        for id in ["other", "a"] {
            observe(
                &mut t,
                &json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id}]}}),
                150,
            );
            assert_eq!(available(&t, "a"), id != "a");
        }
        observe(&mut t, &call("a"), 160);
        assert!(!available(&t, "a"));
        observe(&mut t, &call("b"), 170);
        assert!(available(&t, "b"));
        observe(
            &mut t,
            &json!({"type":"assistant","message":{"id":"later","content":[{"type":"thinking","thinking":"working"}]}}),
            180,
        );
        assert!(!available(&t, "b"));
    }

    #[test]
    fn large_snapshot_duplicates_and_changed_payloads_remain_distinct() {
        let mut t = tracker();
        let mut record = json!({
            "type":"assistant",
            "message":{"id":"large-history","content":[
                {"type":"thinking","thinking":"x".repeat(128 * 1024)},
                {"type":"tool_use","id":"write","name":"Write","input":{"content":"y".repeat(128 * 1024)}}
            ]}
        });
        observe(&mut t, &record, 10);
        let token = t.token;
        observe(&mut t, &record, 20);
        assert_eq!(t.token, token);
        record["message"]["content"][1]["input"]["content"] = json!("changed");
        observe(&mut t, &record, 30);
        assert!(t.token > token);
        let token = t.token;
        observe(&mut t, &record, 40);
        assert_eq!(t.token, token);
        assert_eq!(t.assistant_snapshots.len(), 1);
    }

    #[test]
    fn focused_feedback_soft_wraps_still_block_composer_reservations() {
        let original = String::from_utf8(menu_bytes(false)).unwrap();
        for cols in [20, 24, 40, 55] {
            for feedback in [FEEDBACK, "typed feedback\r\n1. a numbered feedback line"] {
                let mut t = tracker();
                t.resize(60, cols);
                observe(&mut t, &call("a"), 101);
                let bytes = original.replace("❯ 1.", "  1.").replace(
                    "  3. Tell Claude what to change",
                    &format!("❯ 3. {feedback}"),
                );
                t.feed_output(bytes.as_bytes());
                assert!(!available(&t, "a"), "columns {cols}");
                assert!(t.confirmed_menu(), "columns {cols}");
                assert!(InputOperation::begin(Arc::new(Mutex::new(t))).is_err());
            }
        }
    }

    #[test]
    fn quoted_menu_labels_without_selected_numbered_rows_do_not_block_composer() {
        for bytes in [
            format!("\x1b[2J\x1b[H{PROMPT}\r\nThe labels are: {MANUAL}\r\n{BYPASS}"),
            String::from_utf8(menu_bytes(false))
                .unwrap()
                .replace("❯ 1.", "  1."),
        ] {
            let mut t = tracker();
            t.resize(60, 24);
            t.feed_output(bytes.as_bytes());
            assert!(!t.confirmed_menu());
            assert!(InputOperation::begin(Arc::new(Mutex::new(t))).is_ok());
        }
    }

    #[test]
    fn unknown_clipped_and_focused_feedback_fail_closed() {
        let original = String::from_utf8(menu_bytes(false)).unwrap();
        for bytes in [
            original.replace(MANUAL, "Yes, unknown approval mode"),
            original.replace("❯ 1.", "  1.").replace("  3.", "❯ 3."),
            original.replace("  3. Tell Claude what to change", ""),
            original.replace("  2.", "  4."),
            original.replace(
                "  3. Tell Claude what to change",
                "  3. feedback already entered",
            ),
        ] {
            let mut t = tracker();
            observe(&mut t, &call("a"), 101);
            t.feed_output(bytes.as_bytes());
            assert!(!available(&t, "a"), "{bytes:?}");
        }
    }

    #[test]
    fn parked_cursor_does_not_override_selected_ordinary_row() {
        let mut t = tracker();
        observe(&mut t, &call("a"), 101);
        t.feed_output(&menu_bytes(false));
        t.feed_output(b"\x1b[20;1H\x1b[?25l");
        assert!(available(&t, "a"));
    }

    #[test]
    fn wraps_and_resize_require_complete_redrawn_menu() {
        let mut t = tracker();
        t.resize(30, 55);
        let bytes = format!(
            "\x1b[2J\x1b[HClaude has written up a plan and is ready to execute.\r\nWould you like to proceed?\r\n\r\n❯ 1. Yes, and switch to BYPASS PERMISSIONS\r\n     (no further prompts) for this session\r\n  2. {MANUAL}\r\n  3. {FEEDBACK}\x1b[4;1H"
        );
        observe(&mut t, &call("a"), 101);
        t.feed_output(bytes.as_bytes());
        assert!(available(&t, "a"));
        let token = t.token;
        t.resize(4, 20);
        assert!(!available(&t, "a"));
        assert!(
            t.consume("conv", "sess", 7, "a", token, PlanExecutionMode::Manual)
                .is_err()
        );
    }

    #[test]
    fn input_suppresses_unread_calls_and_unchanged_old_menu() {
        let mut t = tracker();
        t.observed_through = 200;
        t.feed_output(&menu_bytes(false));
        t.invalidate_input();
        observe(&mut t, &call("old-unread"), 150);
        t.feed_output(&menu_bytes(false));
        assert!(!available(&t, "old-unread"));
        observe(&mut t, &call("new"), 210);
        assert!(!available(&t, "new"));
        t.feed_output(b"\x1b[2J\x1b[Hworking");
        t.feed_output(&menu_bytes(false));
        assert!(available(&t, "new"));
    }

    #[test]
    fn stale_duplicate_owner_and_retired_claims_are_rejected() {
        let mut t = tracker();
        observe(&mut t, &call("a"), 101);
        t.feed_output(&menu_bytes(false));
        let token = t.token;
        assert!(t.status("wrong", "sess", 7, "a").is_err());
        assert!(t.status("conv", "wrong", 7, "a").is_err());
        assert!(t.status("conv", "sess", 8, "a").is_err());
        t.feed_output(b"\x1b[4;1H");
        assert!(
            t.consume("conv", "sess", 7, "a", token, PlanExecutionMode::Manual)
                .is_err()
        );
        let token = t.token;
        assert_eq!(
            t.consume("conv", "sess", 7, "a", token, PlanExecutionMode::Bypass)
                .unwrap(),
            b'1'
        );
        assert!(
            t.consume("conv", "sess", 7, "a", token, PlanExecutionMode::Manual)
                .is_err()
        );
        t.retire();
        observe(&mut t, &call("new"), 300);
        t.feed_output(&menu_bytes(false));
        assert!(!available(&t, "new"));
    }

    #[test]
    fn operation_reservation_blocks_decisions_and_raw_input_cancels_it() {
        let t = Arc::new(Mutex::new(tracker()));
        let op = InputOperation::begin(t.clone()).unwrap();
        {
            let mut tracker = t.lock().unwrap();
            observe(&mut tracker, &call("a"), 101);
            tracker.feed_output(&menu_bytes(false));
            assert!(!available(&tracker, "a"));
            assert!(op.validate(&tracker).is_err());
            tracker.invalidate_input();
            tracker.feed_output(b"\x1b[2J\x1b[H");
            assert!(op.validate(&tracker).is_err());
        }
        assert!(InputOperation::begin(t.clone()).is_err());
        drop(op);
        assert_eq!(t.lock().unwrap().input_operations, 0);
        let op = InputOperation::begin(t.clone()).unwrap();
        drop(op);
    }

    #[test]
    fn actual_vt_soft_wraps_preserve_label_meaning() {
        for cols in [40, 55, 70, 80] {
            let mut t = tracker();
            t.resize(40, cols);
            observe(&mut t, &call("a"), 101);
            t.feed_output(&menu_bytes(false));
            assert!(
                available(&t, "a"),
                "columns {cols}: {}",
                t.parser.screen().contents()
            );
        }
    }

    #[test]
    fn input_stat_barrier_covers_records_the_follower_has_not_read() {
        let path =
            std::env::temp_dir().join(format!("yiterm-approval-{}.jsonl", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"").unwrap();
        let boundary = TranscriptBoundary::capture(path.clone()).unwrap();
        let mut t = PlanApprovalTracker::new("conv".into(), "sess".into(), 7, boundary, 30, 120);
        let bytes = format!("{}\n", call("unread"));
        std::fs::write(&path, bytes.as_bytes()).unwrap();
        t.feed_output(&menu_bytes(false));
        t.invalidate_input();
        observe(&mut t, &call("unread"), 0);
        t.feed_output(b"\x1b[2J\x1b[H");
        t.feed_output(&menu_bytes(false));
        assert!(!available(&t, "unread"));
        observe(&mut t, &call("new"), bytes.len() as u64);
        assert!(available(&t, "new"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn end_turn_without_displayable_content_closes_claim() {
        let mut t = tracker();
        observe(&mut t, &call("a"), 101);
        t.feed_output(&menu_bytes(false));
        observe(
            &mut t,
            &json!({"type":"assistant","message":{"id":"end","content":[],"stop_reason":"end_turn"}}),
            150,
        );
        assert!(!available(&t, "a"));
    }

    #[test]
    fn replacement_interrupt_and_prompt_invalidate_candidates() {
        for record in [
            json!({"type":"user","uuid":"p","message":{"content":"new prompt"}}),
            json!({"type":"user","message":{"content":"[Request interrupted by user]"}}),
        ] {
            let mut t = tracker();
            observe(&mut t, &call("a"), 101);
            t.feed_output(&menu_bytes(false));
            observe(&mut t, &record, 120);
            assert!(!available(&t, "a"));
        }
        let mut t = tracker();
        observe(&mut t, &call("a"), 101);
        t.feed_output(&menu_bytes(false));
        t.reset_transcript(500, None);
        observe(&mut t, &call("a"), 10);
        assert!(!available(&t, "a"));
    }
}
