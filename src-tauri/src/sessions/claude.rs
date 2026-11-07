//! Discovery of Claude Code's own project/session records.
//!
//! Claude Code keeps one transcript per session at
//! `~/.claude/projects/<munged-cwd>/<session-id>.jsonl` and a project registry
//! in `~/.claude.json`. YiTerm reads both so the sidebar can show everything
//! Claude knows about, not just conversations created inside the app.

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use super::{Conversation, Role, jsonl_path, munge_dir, parse_line, truncate_label};

/// Transcript reads stop after this much data / this many lines. The first
/// `cwd` and the first user turn land well inside this window in practice,
/// while individual transcripts can be hundreds of megabytes.
const META_BYTE_CAP: u64 = 128 * 1024;
const META_LINE_CAP: usize = 400;
const TITLE_MAX: usize = 80;

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeSession {
    pub session_id: String,
    /// The transcript's recorded `cwd`, kept verbatim: it is the exact string
    /// Claude munges into the project directory name, so resuming from it is
    /// more reliable than from a canonicalized equivalent (e.g. `/tmp` vs
    /// `/private/tmp`).
    pub folder: String,
    pub title: String,
    pub modified_ms: u64,
}

/// Session ids are UUIDs we also use as conversation ids, chat-cache file
/// names and image-dir names — reject anything path-ish.
pub fn valid_session_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn projects_root(home: &Path) -> PathBuf {
    home.join(".claude").join("projects")
}

fn munge_map(projects: &[String]) -> HashMap<String, String> {
    projects.iter().map(|p| (munge_dir(p), p.clone())).collect()
}

/// Project directories registered in `~/.claude.json`, restricted to those
/// that still exist on disk (a row whose folder is gone cannot open a session).
pub fn projects(home: &Path) -> Vec<String> {
    let path = home.join(".claude.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(?path, error = %e, "unreadable ~/.claude.json");
            return Vec::new();
        }
    };
    let Some(obj) = value.get("projects").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = obj
        .keys()
        .map(|k| k.trim_end_matches('/').to_string())
        .filter(|k| !k.is_empty() && Path::new(k).is_dir())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Every top-level session transcript under `~/.claude/projects`.
///
/// Only immediate `*.jsonl` children of each project directory are read;
/// per-session directories (`<session-id>/subagents/*.jsonl`) are deliberately
/// ignored. Files with no resolvable workspace folder are skipped — there is
/// nowhere to run `claude --resume`.
pub fn sessions(home: &Path, projects: &[String]) -> Vec<ClaudeSession> {
    let root = projects_root(home);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let registered = munge_map(projects);
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = files
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        paths.sort();

        for path in paths {
            let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !valid_session_id(session_id) || seen.contains(session_id) {
                continue;
            }
            let (cwd, title) = read_meta(&path).unwrap_or((None, None));
            let Some(folder) = resolve_folder(cwd, &dir_name, &registered) else {
                tracing::debug!(?path, "session has no resolvable workspace folder");
                continue;
            };
            seen.insert(session_id.to_string());
            out.push(ClaudeSession {
                session_id: session_id.to_string(),
                title: title.unwrap_or_else(|| folder_name(&folder)),
                folder,
                modified_ms: modified_ms(&path),
            });
        }
    }
    out
}

/// Resolves the actual transcript path for a session id. Prefers the path
/// derived from the workspace folder, but falls back to scanning Claude's
/// project dirs — the recorded cwd and the canonicalized folder can munge to
/// different directory names even though they refer to the same place.
pub fn resolve_jsonl(home: &Path, folder: &str, session_id: &str) -> PathBuf {
    let expected = jsonl_path(home, folder, session_id);
    if expected.is_file() || !valid_session_id(session_id) {
        return expected;
    }
    find_jsonl(home, session_id).unwrap_or(expected)
}

/// Looks up one transcript by id, used when an imported row is opened and its
/// conversation has not been adopted (persisted) yet.
pub fn find_session(home: &Path, session_id: &str) -> Option<ClaudeSession> {
    if !valid_session_id(session_id) {
        return None;
    }
    let path = find_jsonl(home, session_id)?;
    let dir_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let (cwd, title) = read_meta(&path).unwrap_or((None, None));
    let registered = munge_map(&projects(home));
    let folder = resolve_folder(cwd, &dir_name, &registered)?;
    Some(ClaudeSession {
        session_id: session_id.to_string(),
        title: title.unwrap_or_else(|| folder_name(&folder)),
        folder,
        modified_ms: modified_ms(&path),
    })
}

fn find_jsonl(home: &Path, session_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(projects_root(home)).ok()?;
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .map(|d| d.join(format!("{session_id}.jsonl")))
        .find(|p| p.is_file())
}

fn resolve_folder(
    cwd: Option<String>,
    dir_name: &str,
    registered: &HashMap<String, String>,
) -> Option<String> {
    if let Some(cwd) = cwd.filter(|c| Path::new(c).is_dir()) {
        return Some(cwd);
    }
    registered
        .get(dir_name)
        .filter(|p| Path::new(p).is_dir())
        .cloned()
}

/// First `cwd` and first displayable user turn in a transcript prefix.
fn read_meta(path: &Path) -> Option<(Option<String>, Option<String>)> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(META_BYTE_CAP));
    let mut raw = Vec::new();
    let mut cwd = None;
    let mut title = None;
    for _ in 0..META_LINE_CAP {
        raw.clear();
        let n = reader.read_until(b'\n', &mut raw).ok()?;
        if n == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&raw);
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if cwd.is_none() {
            cwd = value
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .filter(|s| !s.is_empty());
        }
        if title.is_none() {
            title = parse_line(&value)
                .into_iter()
                .find(|m| {
                    m.role == Role::User && m.tool_result.is_none() && !m.text.trim().is_empty()
                })
                .map(|m| title_from_text(&m.text))
                .filter(|t| !t.is_empty());
        }
        if cwd.is_some() && title.is_some() {
            break;
        }
    }
    Some((cwd, title))
}

/// One-line sidebar label: strip image markers, collapse whitespace, cap length.
fn title_from_text(text: &str) -> String {
    let had_image = text.contains("[Image #");
    let cleaned = strip_image_markers(text);
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return if had_image {
            "[图片]".to_string()
        } else {
            String::new()
        };
    }
    truncate_label(&collapsed, TITLE_MAX)
}

fn strip_image_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("[Image #") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + "[Image #".len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && after.as_bytes().get(digits) == Some(&b']') {
            rest = &after[digits + 1..];
        } else {
            out.push_str("[Image #");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn folder_name(folder: &str) -> String {
    Path::new(folder)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.to_string())
}

fn modified_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Merge stored conversations with transcripts found on disk: one overview
/// call so the sidebar gets a consistent snapshot of both.
pub fn overview(home: &Path, stored: Vec<Conversation>) -> (Vec<Conversation>, Vec<String>) {
    let projects = projects(home);
    let found = sessions(home, &projects);
    (merge(stored, found), projects)
}

/// Pure merge (unit-tested without a filesystem): drop transcripts already
/// represented by a stored conversation, synthesize imported conversations for
/// the rest, and order newest-first.
pub fn merge(stored: Vec<Conversation>, found: Vec<ClaudeSession>) -> Vec<Conversation> {
    let mut known: HashSet<String> = HashSet::new();
    for c in &stored {
        known.insert(c.id.clone());
        known.insert(c.session_id.clone());
    }
    let mut list = stored;
    for s in found {
        if !known.insert(s.session_id.clone()) {
            continue;
        }
        list.push(Conversation {
            id: s.session_id.clone(),
            folder: s.folder,
            session_id: s.session_id,
            terminal_id: None,
            title: s.title,
            created_at: s.modified_ms.to_string(),
            imported: true,
        });
    }
    list.sort_by(|a, b| {
        let am = a.created_at.parse::<u64>().unwrap_or(0);
        let bm = b.created_at.parse::<u64>().unwrap_or(0);
        bm.cmp(&am).then_with(|| a.id.cmp(&b.id))
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TempHome(PathBuf);

    impl TempHome {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("yiterm-claude-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn mkdir(&self, rel: &str) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(&path).unwrap();
            path
        }

        fn write(&self, rel: &str, body: &str) -> PathBuf {
            let path = self.0.join(rel);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).unwrap();
            }
            std::fs::write(&path, body).unwrap();
            path
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn project_dir(t: &TempHome, folder: &Path) -> PathBuf {
        t.mkdir(&format!(
            ".claude/projects/{}",
            munge_dir(&folder.to_string_lossy())
        ))
    }

    fn jsonl(lines: &[serde_json::Value]) -> String {
        lines
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn projects_reads_registered_keys_and_skips_missing_dirs() {
        let t = TempHome::new();
        let a = t.mkdir("a");
        let b = t.mkdir("b");
        let mut projects_obj = serde_json::Map::new();
        projects_obj.insert(a.to_string_lossy().into_owned(), json!({}));
        projects_obj.insert(b.to_string_lossy().into_owned(), json!({}));
        projects_obj.insert("/definitely/not/here".into(), json!({}));
        projects_obj.insert(String::new(), json!({}));
        let registry = json!({ "projects": projects_obj, "numStartups": 12 });
        t.write(".claude.json", &registry.to_string());
        assert_eq!(
            projects(t.path()),
            vec![
                a.to_string_lossy().into_owned(),
                b.to_string_lossy().into_owned()
            ]
        );

        t.write(".claude.json", "{not json");
        assert!(projects(t.path()).is_empty());
    }

    #[test]
    fn scans_only_top_level_jsonl_and_extracts_cwd_after_mode_lines() {
        let t = TempHome::new();
        let work = t.mkdir("work");
        let dir = project_dir(&t, &work);
        let sid = "11111111-1111-4111-8111-111111111111";
        let body = jsonl(&[
            json!({ "type": "mode", "sessionId": sid }),
            json!({ "type": "permission-mode", "sessionId": sid }),
            json!({
                "type": "user", "uuid": "u1", "cwd": work.to_string_lossy(),
                "message": { "role": "user", "content": "Hello   world" }
            }),
        ]);
        t.write(
            &format!(
                ".claude/projects/{}/subagents/agent-x.jsonl",
                munge_dir(&work.to_string_lossy())
            ),
            "",
        );
        let path = dir.join(format!("{sid}.jsonl"));
        std::fs::write(&path, body).unwrap();

        let found = sessions(t.path(), &[]);
        assert_eq!(found.len(), 1, "subagent transcripts must be ignored");
        assert_eq!(found[0].session_id, sid);
        assert_eq!(found[0].folder, work.to_string_lossy());
        assert_eq!(found[0].title, "Hello world");
        assert_eq!(found[0].modified_ms, modified_ms(&path));
    }

    #[test]
    fn image_only_prompt_titles_are_labelled() {
        let t = TempHome::new();
        let work = t.mkdir("work");
        project_dir(&t, &work);
        let sid = "22222222-2222-4222-8222-222222222222";
        let body = jsonl(&[json!({
            "type": "user", "uuid": "u1", "cwd": work.to_string_lossy(),
            "message": { "role": "user", "content": [
                { "type": "text", "text": "[Image #1]" },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } }
            ] }
        })]);
        t.write(
            &format!(
                ".claude/projects/{}/{sid}.jsonl",
                munge_dir(&work.to_string_lossy())
            ),
            &body,
        );
        let found = sessions(t.path(), &[]);
        assert_eq!(found[0].title, "[图片]");
    }

    #[test]
    fn no_user_turn_falls_back_to_folder_name() {
        let t = TempHome::new();
        let work = t.mkdir("my-project");
        project_dir(&t, &work);
        let sid = "33333333-3333-4333-8333-333333333333";
        let body = jsonl(&[
            json!({ "type": "mode" }),
            json!({ "type": "attachment", "cwd": work.to_string_lossy() }),
            json!({ "type": "cost-state" }),
        ]);
        t.write(
            &format!(
                ".claude/projects/{}/{sid}.jsonl",
                munge_dir(&work.to_string_lossy())
            ),
            &body,
        );
        let found = sessions(t.path(), &[]);
        assert_eq!(found[0].title, "my-project");
    }

    #[test]
    fn missing_cwd_falls_back_to_registered_path_by_munged_name() {
        let t = TempHome::new();
        let work = t.mkdir("registered");
        project_dir(&t, &work);
        let sid = "44444444-4444-4444-8444-444444444444";
        t.write(
            &format!(
                ".claude/projects/{}/{sid}.jsonl",
                munge_dir(&work.to_string_lossy())
            ),
            "{\"type\":\"mode\"}\n",
        );
        let found = sessions(t.path(), &[work.to_string_lossy().into_owned()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].folder, work.to_string_lossy());
    }

    #[test]
    fn session_without_resolvable_folder_is_skipped() {
        let t = TempHome::new();
        let work = t.mkdir("gone");
        project_dir(&t, &work);
        let sid = "55555555-5555-4555-8555-555555555555";
        t.write(
            &format!(
                ".claude/projects/{}/{sid}.jsonl",
                munge_dir(&work.to_string_lossy())
            ),
            "{\"type\":\"mode\"}\n",
        );
        assert!(sessions(t.path(), &[]).is_empty());
    }

    #[test]
    fn ignores_corrupt_and_oversized_lines() {
        let t = TempHome::new();
        let work = t.mkdir("work");
        project_dir(&t, &work);
        let sid = "66666666-6666-4666-8666-666666666666";
        let huge = format!("\"{}\"", "x".repeat(META_BYTE_CAP as usize + 10));
        let cwd = serde_json::to_string(&work.to_string_lossy()).unwrap();
        let body = format!(
            "not json\n{{\"type\":\"attachment\",\"cwd\":{cwd}}}\n\
             {{\"type\":\"user\",\"uuid\":\"u1\",\"message\":{{\"role\":\"user\",\"content\":{huge}}}}}\n"
        );
        t.write(
            &format!(
                ".claude/projects/{}/{sid}.jsonl",
                munge_dir(&work.to_string_lossy())
            ),
            &body,
        );
        let found = sessions(t.path(), &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "work"); // no readable turn inside the cap
    }

    #[test]
    fn find_session_rejects_pathish_ids_and_finds_real_ones() {
        let t = TempHome::new();
        let work = t.mkdir("work");
        project_dir(&t, &work);
        let sid = "77777777-7777-4777-8777-777777777777";
        let body = jsonl(&[json!({
            "type": "user", "uuid": "u1", "cwd": work.to_string_lossy(),
            "message": { "role": "user", "content": "find me" }
        })]);
        t.write(
            &format!(
                ".claude/projects/{}/{sid}.jsonl",
                munge_dir(&work.to_string_lossy())
            ),
            &body,
        );

        let found = find_session(t.path(), sid).expect("session");
        assert_eq!(found.title, "find me");
        assert_eq!(found.folder, work.to_string_lossy());
        for bad in ["../x", "a/b", "", "a b"] {
            assert!(find_session(t.path(), bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn resolve_jsonl_prefers_derived_path_then_scans() {
        let t = TempHome::new();
        let work = t.mkdir("work");
        project_dir(&t, &work);
        let sid = "88888888-8888-4888-8888-888888888888";
        let actual = t.write(
            &format!(
                ".claude/projects/{}/{sid}.jsonl",
                munge_dir(&work.to_string_lossy())
            ),
            "",
        );

        // Derived path exists → used directly.
        assert_eq!(
            resolve_jsonl(t.path(), &work.to_string_lossy(), sid),
            actual
        );
        // A different spelling of the same folder munges elsewhere → scan wins.
        assert_eq!(resolve_jsonl(t.path(), "/some/other/spelling", sid), actual);
        // Unknown id → the derived path is where a new session will appear.
        let fresh = resolve_jsonl(t.path(), &work.to_string_lossy(), "not-a-session");
        assert_eq!(
            fresh,
            jsonl_path(t.path(), &work.to_string_lossy(), "not-a-session")
        );
    }

    #[test]
    fn merge_dedupes_by_stored_session_and_sorts_newest_first() {
        let stored = Conversation {
            id: "app-1".into(),
            folder: "/p".into(),
            session_id: "session-1".into(),
            terminal_id: None,
            title: "stored".into(),
            created_at: "1000".into(),
            imported: false,
        };
        let found = vec![
            ClaudeSession {
                session_id: "session-1".into(),
                folder: "/p".into(),
                title: "duplicate".into(),
                modified_ms: 9000,
            },
            ClaudeSession {
                session_id: "session-2".into(),
                folder: "/q".into(),
                title: "imported".into(),
                modified_ms: 5000,
            },
            ClaudeSession {
                session_id: "session-3".into(),
                folder: "/r".into(),
                title: "newest".into(),
                modified_ms: 10_000,
            },
        ];
        let merged = merge(vec![stored], found);
        assert_eq!(
            merged.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["session-3", "session-2", "app-1"]
        );
        assert!(merged[0].imported);
        assert!(!merged[2].imported);
        assert_eq!(merged[1].created_at, "5000");
    }

    #[test]
    fn overview_merges_registry_and_transcripts() {
        let t = TempHome::new();
        let work = t.mkdir("work");
        let mut projects_obj = serde_json::Map::new();
        projects_obj.insert(work.to_string_lossy().into_owned(), json!({}));
        let registry = json!({ "projects": projects_obj });
        t.write(".claude.json", &registry.to_string());
        project_dir(&t, &work);
        let body = jsonl(&[json!({
            "type": "user", "uuid": "u1", "cwd": work.to_string_lossy(),
            "message": { "role": "user", "content": "hi" }
        })]);
        t.write(
            &format!(
                ".claude/projects/{}/99999999-9999-4999-8999-999999999999.jsonl",
                munge_dir(&work.to_string_lossy())
            ),
            &body,
        );

        let (conversations, projects) = overview(t.path(), Vec::new());
        assert_eq!(projects, vec![work.to_string_lossy().into_owned()]);
        assert_eq!(conversations.len(), 1);
        assert!(conversations[0].imported);
    }

    #[test]
    fn image_markers_are_stripped() {
        assert_eq!(
            strip_image_markers("[Image #1] Take a look"),
            " Take a look"
        );
        assert_eq!(
            strip_image_markers("look [Image #12] and [Image #3]"),
            "look  and "
        );
        assert_eq!(strip_image_markers("[Image #x] keep"), "[Image #x] keep");
        assert_eq!(title_from_text("[Image #1]"), "[图片]");
    }
}
