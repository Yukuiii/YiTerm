use std::io::{self, Read};
use std::path::{Component, Path};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const STATUS_LIMIT: usize = 4 * 1024 * 1024;
const DIFF_LIMIT: usize = 1024 * 1024;
const HISTORY_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const COMMIT_DETAILS_LIMIT: usize = 4 * 1024 * 1024;
const STDERR_LIMIT: usize = 32 * 1024;
const DEADLINE: Duration = Duration::from_secs(10);
const STALE_SELECTION: &str = "文件状态已变化，请刷新 Git 状态后重试。";
const HISTORY_CHANGED: &str = "Git HEAD 或引用持续变化，请刷新提交历史后重试。";

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GitArea {
    Staged,
    Unstaged,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    path: String,
    original_path: Option<String>,
    status: String,
    conflicted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    root: Option<String>,
    branch: Option<String>,
    staged: Vec<GitFileChange>,
    unstaged: Vec<GitFileChange>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GitDiffKind {
    Text,
    Binary,
    Conflict,
    Empty,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiff {
    text: String,
    kind: GitDiffKind,
    truncated: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHistory {
    root: Option<String>,
    commits: Vec<GitCommit>,
    refs: Vec<GitRef>,
    head: GitHead,
    has_more: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommit {
    oid: String,
    parents: Vec<String>,
    subject: String,
    author_name: String,
    author_time: i64,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRef {
    name: String,
    short_name: String,
    kind: GitRefKind,
    oid: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GitRefKind {
    Branch,
    Remote,
    Tag,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHead {
    oid: Option<String>,
    ref_name: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitDetails {
    oid: String,
    message: String,
    files_changed: u64,
    insertions: u64,
    deletions: u64,
    github_url: Option<String>,
}

#[tauri::command]
pub async fn git_commit_details(folder: String, oid: String) -> Result<GitCommitDetails, String> {
    tauri::async_runtime::spawn_blocking(move || commit_details_in(Path::new(&folder), &oid))
        .await
        .map_err(|e| format!("读取 Git 提交详情失败：{e}"))?
}

#[tauri::command]
pub async fn git_history(folder: String, limit: usize) -> Result<GitHistory, String> {
    tauri::async_runtime::spawn_blocking(move || history_in(Path::new(&folder), limit))
        .await
        .map_err(|e| format!("读取 Git 提交历史失败：{e}"))?
}

#[tauri::command]
pub async fn git_status(folder: String) -> Result<GitStatus, String> {
    tauri::async_runtime::spawn_blocking(move || status_in(Path::new(&folder)))
        .await
        .map_err(|e| format!("读取 Git 状态失败：{e}"))?
}

#[tauri::command]
pub async fn git_diff(folder: String, area: GitArea, path: String) -> Result<GitDiff, String> {
    tauri::async_runtime::spawn_blocking(move || diff_in(Path::new(&folder), area, &path))
        .await
        .map_err(|e| format!("读取 Git 差异失败：{e}"))?
}

fn git_command(folder: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(folder)
        .args([
            "--no-pager",
            "--no-optional-locks",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.quotepath=false",
        ])
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("GIT_TERMINAL_PROMPT", "0");
    // The supplied folder determines the repository, rather than the app's environment.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
    ] {
        command.env_remove(key);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

#[derive(Debug)]
struct ProcessOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

fn read_pipe(mut pipe: impl Read, limit: usize, overflow: Arc<AtomicBool>) -> io::Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = match pipe.read(&mut buffer) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Ok(result);
        }
        let keep = count.min(limit.saturating_sub(result.len()));
        result.extend_from_slice(&buffer[..keep]);
        if keep < count {
            overflow.store(true, Ordering::Release);
        }
    }
}

fn kill_process(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // Also close pipes held by a helper if Git has already exited. Each invocation
        // owns its process group; no shell or PTY is used to launch Git.
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        unsafe {
            kill(-(child.id() as i32), 9);
        }
    }
    let _ = child.kill();
}

fn run_git(
    command: &mut Command,
    stdout_limit: usize,
    allow_truncation: bool,
    deadline: Duration,
) -> Result<ProcessOutput, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            "未找到 Git，请先安装 Git。".to_owned()
        } else {
            format!("无法启动 Git：{e}")
        }
    })?;
    let stdout_overflow = Arc::new(AtomicBool::new(false));
    let stderr_overflow = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_flag = stdout_overflow.clone();
    let stderr_flag = stderr_overflow.clone();
    let stdout_reader = thread::spawn(move || read_pipe(stdout, stdout_limit, stdout_flag));
    let stderr_reader = thread::spawn(move || read_pipe(stderr, STDERR_LIMIT, stderr_flag));
    let started = Instant::now();
    let mut exited = None;
    let mut error = None;
    loop {
        if stdout_overflow.load(Ordering::Acquire) || stderr_overflow.load(Ordering::Acquire) {
            kill_process(&mut child);
            break;
        }
        if started.elapsed() >= deadline {
            error = Some("Git 操作超时，请稍后重试。".to_owned());
            kill_process(&mut child);
            break;
        }
        if exited.is_none() {
            match child.try_wait() {
                Ok(status) => exited = status,
                Err(e) => {
                    error = Some(format!("等待 Git 退出失败：{e}"));
                    kill_process(&mut child);
                    break;
                }
            }
        }
        if exited.is_some() && stdout_reader.is_finished() && stderr_reader.is_finished() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    // Reap and join on every path, including timeouts and output overflows.
    let status = child.wait();
    let stdout = stdout_reader.join();
    let stderr = stderr_reader.join();
    if let Some(error) = error {
        return Err(error);
    }
    if stderr_overflow.load(Ordering::Acquire) {
        return Err("Git 错误输出超过大小限制。".to_owned());
    }
    let truncated = stdout_overflow.load(Ordering::Acquire);
    if truncated && !allow_truncation {
        return Err(format!("Git 输出超过 {stdout_limit} 字节限制。"));
    }
    let read_result = |result: thread::Result<io::Result<Vec<u8>>>| {
        result
            .map_err(|_| "读取 Git 输出的线程意外退出。".to_owned())?
            .map_err(|e| format!("读取 Git 输出失败：{e}"))
    };
    Ok(ProcessOutput {
        status: status.map_err(|e| format!("等待 Git 退出失败：{e}"))?,
        stdout: read_result(stdout)?,
        stderr: read_result(stderr)?,
        truncated,
    })
}

fn command_error(output: &ProcessOutput) -> String {
    let message = String::from_utf8_lossy(&output.stderr);
    let message = message.trim();
    if message.is_empty() {
        format!("Git 操作失败（退出状态：{}）。", output.status)
    } else {
        format!("Git 操作失败：{message}")
    }
}

/// Runs one Git command that must succeed with complete output.
fn git_output(command: &mut Command, limit: usize) -> Result<ProcessOutput, String> {
    let output = run_git(command, limit, false, DEADLINE)?;
    if !output.status.success() {
        return Err(command_error(&output));
    }
    Ok(output)
}

fn repository_root(folder: &Path) -> Result<Option<String>, String> {
    let metadata = std::fs::metadata(folder).map_err(|e| format!("无法访问目录：{e}"))?;
    if !metadata.is_dir() {
        return Err("指定路径不是目录。".to_owned());
    }
    let output = run_git(
        git_command(folder).args(["rev-parse", "--show-toplevel"]),
        STATUS_LIMIT,
        false,
        DEADLINE,
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.starts_with("fatal: not a git repository (or any of the parent directories):")
            || stderr.starts_with("fatal: not a git repository (or any parent up to mount point ")
        {
            return Ok(None);
        }
        return Err(command_error(&output));
    }
    let path = std::str::from_utf8(trim_newline(&output.stdout))
        .map_err(|_| "Git 仓库路径包含无法通过 UTF-8 传输的字符。".to_owned())?;
    if path.is_empty() || !Path::new(path).is_absolute() {
        return Err("Git 返回了无效的仓库根目录。".to_owned());
    }
    Ok(Some(path.to_owned()))
}

/// A single-line Git answer without its trailing newline.
fn trim_newline(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

/// A full ref name as Git prints it: under `refs/`, with no whitespace or
/// control characters.
fn valid_ref_name(name: &str) -> bool {
    name.starts_with("refs/") && !name.bytes().any(|byte| byte <= b' ' || byte == 127)
}

fn parse_oid(bytes: &[u8]) -> Result<String, String> {
    if !matches!(bytes.len(), 40 | 64)
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err("Git 返回了无效的对象 ID。".to_owned());
    }
    Ok(String::from_utf8(bytes.to_vec()).expect("hex object ID"))
}

/// A commit's parents: space-separated object IDs, empty for a root commit.
fn parse_parents(bytes: &[u8]) -> Result<Vec<String>, String> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    bytes.split(|byte| *byte == b' ').map(parse_oid).collect()
}

fn parse_history_log(bytes: &[u8]) -> Result<Vec<GitCommit>, String> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let records = bytes
        .strip_suffix(b"\0")
        .ok_or("Git 提交历史记录不完整。")?;
    let fields: Vec<_> = records.split(|byte| *byte == 0).collect();
    // Each record is oid, parents, author name, author time, subject.
    let (records, remainder) = fields.as_chunks::<5>();
    let text = |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| "Git 提交历史包含无法通过 UTF-8 传输的字符。".to_owned())
    };
    let mut commits = Vec::with_capacity(records.len());
    for [oid, parents, author, time, subject] in records {
        commits.push(GitCommit {
            oid: parse_oid(oid)?,
            parents: parse_parents(parents)?,
            author_name: text(author)?,
            author_time: text(time)?
                .parse()
                .map_err(|_| "Git 返回了无效的提交时间。".to_owned())?,
            subject: text(subject)?,
        });
    }
    if !remainder.is_empty() {
        return Err("Git 提交历史记录不完整。".to_owned());
    }
    Ok(commits)
}

fn parse_history_refs(bytes: &[u8]) -> Result<Vec<GitRef>, String> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let records = bytes.strip_suffix(b"\n").ok_or("Git 引用记录不完整。")?;
    let mut refs = Vec::new();
    for record in records.split(|byte| *byte == b'\n') {
        let record = record.strip_suffix(b"\0").ok_or("Git 引用记录不完整。")?;
        let record = std::str::from_utf8(record)
            .map_err(|_| "Git 引用包含无法通过 UTF-8 传输的字符。".to_owned())?;
        let fields: Vec<_> = record.split('\0').collect();
        let [name, oid, object_type, peeled_oid, peeled_type, symref] = fields.as_slice() else {
            return Err("Git 返回了无效的引用记录。".to_owned());
        };
        if !valid_ref_name(name) {
            return Err("Git 返回了无效的引用名称。".to_owned());
        }
        let (kind, short_name) = if let Some(short) = name.strip_prefix("refs/heads/") {
            (GitRefKind::Branch, short)
        } else if let Some(short) = name.strip_prefix("refs/remotes/") {
            (GitRefKind::Remote, short)
        } else if let Some(short) = name.strip_prefix("refs/tags/") {
            (GitRefKind::Tag, short)
        } else {
            // --all also traverses refs/stash and other namespaces. Snapshot them,
            // but expose only the three ref kinds supported by the IPC contract.
            continue;
        };
        if short_name.is_empty() {
            return Err("Git 返回了无效的引用名称。".to_owned());
        }
        let oid = parse_oid(oid.as_bytes())?;
        if !matches!(*object_type, "commit" | "tag" | "tree" | "blob") {
            return Err("Git 返回了无效的引用对象类型。".to_owned());
        }
        let oid = if *object_type == "tag" {
            // Git's starred for-each-ref atoms peel annotated tags recursively.
            let peeled_oid = parse_oid(peeled_oid.as_bytes())?;
            if !matches!(*peeled_type, "commit" | "tree" | "blob") {
                return Err("Git 返回了无效的标签目标类型。".to_owned());
            }
            if *peeled_type != "commit" {
                continue;
            }
            peeled_oid
        } else if *object_type == "commit" {
            oid
        } else {
            continue;
        };
        if kind == GitRefKind::Remote && short_name.ends_with("/HEAD") && !symref.is_empty() {
            continue;
        }
        refs.push(GitRef {
            name: (*name).to_owned(),
            short_name: short_name.to_owned(),
            kind,
            oid,
        });
    }
    Ok(refs)
}

#[derive(Debug, PartialEq, Eq)]
struct HistorySnapshot {
    head: GitHead,
    // Keep raw object IDs, including tag objects and ignored symbolic aliases,
    // so moving a ref to a different tag for the same commit still invalidates it.
    refs: Vec<u8>,
}

fn read_history_snapshot(root: &Path) -> Result<HistorySnapshot, String> {
    let symbolic = run_git(
        git_command(root).args(["symbolic-ref", "--quiet", "HEAD"]),
        HISTORY_OUTPUT_LIMIT,
        false,
        DEADLINE,
    )?;
    let ref_name = if symbolic.status.success() {
        let name = std::str::from_utf8(trim_newline(&symbolic.stdout))
            .map_err(|_| "Git HEAD 引用包含无法通过 UTF-8 传输的字符。".to_owned())?;
        if !valid_ref_name(name) {
            return Err("Git 返回了无效的 HEAD 引用。".to_owned());
        }
        Some(name.to_owned())
    } else if symbolic.status.code() == Some(1) {
        None
    } else {
        return Err(command_error(&symbolic));
    };
    let resolved = run_git(
        git_command(root).args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]),
        HISTORY_OUTPUT_LIMIT,
        false,
        DEADLINE,
    )?;
    let oid = if resolved.status.success() {
        Some(parse_oid(trim_newline(&resolved.stdout))?)
    } else if resolved.status.code() == Some(1) && ref_name.is_some() {
        // An absent symbolic target is unborn. A present but invalid HEAD is an error.
        let unpeeled = run_git(
            git_command(root).args(["rev-parse", "--verify", "--quiet", "HEAD"]),
            HISTORY_OUTPUT_LIMIT,
            false,
            DEADLINE,
        )?;
        if unpeeled.status.code() != Some(1) {
            return Err(command_error(&resolved));
        }
        None
    } else {
        return Err(command_error(&resolved));
    };
    let refs = git_output(
        git_command(root).args([
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(*objectname)%00%(*objecttype)%00%(symref)%00",
        ]),
        HISTORY_OUTPUT_LIMIT,
    )?;
    Ok(HistorySnapshot {
        head: GitHead { oid, ref_name },
        refs: refs.stdout,
    })
}

fn read_history_commits(
    root: &Path,
    head: &GitHead,
    limit: usize,
) -> Result<Vec<GitCommit>, String> {
    let mut command = git_command(root);
    command.args([
        "log",
        "--all",
        "--topo-order",
        &format!("--max-count={}", limit + 1),
        "-z",
        "--format=%H%x00%P%x00%an%x00%at%x00%s",
        "--encoding=UTF-8",
        "--no-decorate",
        "--no-color",
        "--no-show-signature",
        "--no-notes",
    ]);
    // --all alone need not include a detached HEAD's otherwise unreachable commits.
    if let Some(oid) = &head.oid {
        command.arg(oid);
    }
    command.arg("--");
    let output = git_output(&mut command, HISTORY_OUTPUT_LIMIT)?;
    parse_history_log(&output.stdout)
}

fn history_at(
    root: String,
    limit: usize,
    mut read_snapshot: impl FnMut(&Path) -> Result<HistorySnapshot, String>,
) -> Result<GitHistory, String> {
    let limit = limit.clamp(1, 1000);
    for _ in 0..2 {
        let before = read_snapshot(Path::new(&root))?;
        let data = (|| {
            let refs = parse_history_refs(&before.refs)?;
            let commits = read_history_commits(Path::new(&root), &before.head, limit)?;
            Ok::<_, String>((commits, refs))
        })();
        let after = read_snapshot(Path::new(&root))?;
        // Even failed log reads can be caused by concurrent ref changes; retry them.
        if before != after {
            continue;
        }
        let (mut commits, refs) = data?;
        let has_more = commits.len() > limit;
        commits.truncate(limit);
        return Ok(GitHistory {
            root: Some(root),
            commits,
            refs,
            head: before.head,
            has_more,
        });
    }
    Err(HISTORY_CHANGED.to_owned())
}

fn history_in(folder: &Path, limit: usize) -> Result<GitHistory, String> {
    let Some(root) = repository_root(folder)? else {
        return Ok(GitHistory {
            root: None,
            commits: Vec::new(),
            refs: Vec::new(),
            head: GitHead {
                oid: None,
                ref_name: None,
            },
            has_more: false,
        });
    };
    history_at(root, limit, read_history_snapshot)
}

fn parse_commit_message(bytes: &[u8], oid: &str) -> Result<(Vec<String>, String), String> {
    let record = bytes
        .strip_suffix(b"\0")
        .ok_or("Git 提交详情记录不完整。")?;
    let fields: Vec<_> = record.split(|byte| *byte == 0).collect();
    let [actual_oid, parents, message] = fields.as_slice() else {
        return Err("Git 返回了无效的提交详情记录。".to_owned());
    };
    if parse_oid(actual_oid)? != oid {
        return Err("Git 返回的提交 ID 与请求不一致。".to_owned());
    }
    let parents = parse_parents(parents)?;
    let message = std::str::from_utf8(message)
        .map_err(|_| "Git 提交说明包含无法通过 UTF-8 传输的字符。".to_owned())?;
    Ok((parents, message.to_owned()))
}

fn parse_commit_numstat(bytes: &[u8]) -> Result<(u64, u64, u64), String> {
    if bytes.is_empty() {
        return Ok((0, 0, 0));
    }
    let records = bytes
        .strip_suffix(b"\0")
        .ok_or("Git 提交统计记录不完整。")?;
    let mut records = records.split(|byte| *byte == 0);
    let mut files_changed = 0_u64;
    let mut insertions = 0_u64;
    let mut deletions = 0_u64;
    let invalid = || "Git 返回了无效的提交统计记录。".to_owned();
    while let Some(record) = records.next() {
        // Paths can contain tabs, newlines and non-UTF-8 bytes; only the first
        // two tabs delimit numbers. With -z, renames have two extra path fields.
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let added = fields.next().ok_or_else(invalid)?;
        let removed = fields.next().ok_or_else(invalid)?;
        let path = fields.next().ok_or_else(invalid)?;
        if path.is_empty() {
            for _ in 0..2 {
                if records.next().ok_or_else(invalid)?.is_empty() {
                    return Err(invalid());
                }
            }
        }
        let (added, removed) = if added == b"-" && removed == b"-" {
            (0, 0)
        } else {
            let number = |bytes: &[u8]| -> Result<u64, String> {
                if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
                    return Err(invalid());
                }
                std::str::from_utf8(bytes)
                    .map_err(|_| invalid())?
                    .parse()
                    .map_err(|_| invalid())
            };
            (number(added)?, number(removed)?)
        };
        files_changed = files_changed.checked_add(1).ok_or_else(invalid)?;
        insertions = insertions.checked_add(added).ok_or_else(invalid)?;
        deletions = deletions.checked_add(removed).ok_or_else(invalid)?;
    }
    Ok((files_changed, insertions, deletions))
}

fn github_commit_url(remote: &str, oid: &str) -> Option<String> {
    let remote = remote.trim();
    let scp_url;
    let remote = if let Some(path) = remote.strip_prefix("git@github.com:") {
        scp_url = format!("ssh://git@github.com/{path}");
        scp_url.as_str()
    } else {
        remote
    };
    let url = reqwest::Url::parse(remote).ok()?;
    if !matches!(url.scheme(), "https" | "http" | "ssh" | "git")
        || !url.host_str()?.eq_ignore_ascii_case("github.com")
    {
        return None;
    }
    let mut path = url.path().trim_matches('/').split('/');
    let owner = path.next()?;
    let repository = path.next()?;
    let repository = repository.strip_suffix(".git").unwrap_or(repository);
    let valid_component = |part: &str| {
        !part.is_empty()
            && !matches!(part, "." | "..")
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    if path.next().is_some() || !valid_component(owner) || !valid_component(repository) {
        return None;
    }
    // Build from the known host and path components, never forwarding userinfo,
    // query strings, fragments, or a remote URL's transport scheme and port.
    Some(format!(
        "https://github.com/{owner}/{repository}/commit/{oid}"
    ))
}

fn commit_details_in(folder: &Path, oid: &str) -> Result<GitCommitDetails, String> {
    let oid = parse_oid(oid.as_bytes())?;
    let root = repository_root(folder)?.ok_or("当前目录不是 Git 仓库。")?;
    let root = Path::new(&root);
    let output = git_output(
        git_command(root).args([
            "show",
            "-s",
            "-z",
            "--format=%H%x00%P%x00%B",
            "--encoding=UTF-8",
            "--no-notes",
            "--no-show-signature",
            "--no-decorate",
            "--no-color",
            &oid,
            "--",
        ]),
        COMMIT_DETAILS_LIMIT,
    )?;
    let (parents, message) = parse_commit_message(&output.stdout, &oid)?;
    let mut command = git_command(root);
    command.args([
        "diff-tree",
        "--numstat",
        "-z",
        "-r",
        "--root",
        "--no-commit-id",
        "--find-renames",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
    ]);
    if let Some(parent) = parents.first() {
        command.arg(parent);
    }
    command.args([&oid, "--"]);
    let output = git_output(&mut command, COMMIT_DETAILS_LIMIT)?;
    let (files_changed, insertions, deletions) = parse_commit_numstat(&output.stdout)?;
    let origin = run_git(
        git_command(root).args(["config", "--local", "--get", "remote.origin.url"]),
        COMMIT_DETAILS_LIMIT,
        false,
        DEADLINE,
    )?;
    let github_url = if origin.status.success() {
        std::str::from_utf8(&origin.stdout)
            .ok()
            .and_then(|remote| github_commit_url(remote, &oid))
    } else if origin.status.code() == Some(1) {
        None
    } else {
        return Err(command_error(&origin));
    };
    Ok(GitCommitDetails {
        oid,
        message,
        files_changed,
        insertions,
        deletions,
        github_url,
    })
}

fn validate_path(path: &str) -> Result<(), String> {
    // Git represents an untracked embedded repository as a directory ending
    // in '/', even with --untracked-files=all.
    let path = path.strip_suffix('/').unwrap_or(path);
    if path.is_empty()
        || path.contains('\0')
        || path.split('/').any(|part| matches!(part, "" | "." | ".."))
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Git 文件路径必须是仓库内的相对路径。".to_owned());
    }
    Ok(())
}

fn status_path(bytes: &[u8]) -> Result<String, String> {
    let path = std::str::from_utf8(bytes)
        .map_err(|_| "Git 文件名包含无法通过 UTF-8 传输的字符。".to_owned())?;
    validate_path(path)?;
    Ok(path.to_owned())
}

fn parse_branch(header: &[u8]) -> Result<Option<String>, String> {
    let header = std::str::from_utf8(header)
        .map_err(|_| "Git 分支名称包含无法通过 UTF-8 传输的字符。".to_owned())?;
    if header == "HEAD (no branch)" {
        return Ok(None);
    }
    let head = header
        .strip_prefix("No commits yet on ")
        .or_else(|| header.strip_prefix("Initial commit on "))
        .unwrap_or(header);
    // Branch names cannot contain '..' or ASCII spaces. Drop upstream and
    // ahead/behind information without splitting valid Unicode whitespace.
    let branch = head.split("...").next().unwrap_or(head);
    let branch = branch.split(' ').next().unwrap_or(branch);
    if branch.is_empty() {
        return Err("Git 返回了无效的分支名称。".to_owned());
    }
    Ok(Some(branch.to_owned()))
}

fn parse_status(bytes: &[u8], root: String) -> Result<GitStatus, String> {
    let mut result = GitStatus {
        root: Some(root),
        branch: None,
        staged: Vec::new(),
        unstaged: Vec::new(),
    };
    let mut records = bytes.split(|byte| *byte == 0);
    while let Some(record) = records.next() {
        if record.is_empty() {
            if records.next().is_some() {
                return Err("Git 返回了无效的状态记录。".to_owned());
            }
            break;
        }
        if record.len() < 4 || record[2] != b' ' || !bytes.ends_with(&[0]) {
            return Err("Git 返回了无效的状态记录。".to_owned());
        }
        if let Some(header) = record.strip_prefix(b"## ") {
            result.branch = parse_branch(header)?;
            continue;
        }
        let x = record[0];
        let y = record[1];
        let path = status_path(&record[3..])?;
        // With -z Git emits the destination first, then the original path.
        let original_path = if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            Some(status_path(
                records
                    .next()
                    .ok_or_else(|| "Git 重命名记录缺少原始路径。".to_owned())?,
            )?)
        } else {
            None
        };
        let conflicted = matches!(
            &record[..2],
            b"DD" | b"AU" | b"UD" | b"UA" | b"DU" | b"AA" | b"UU"
        );
        let change = |code: u8| GitFileChange {
            path: path.clone(),
            original_path: original_path.clone(),
            status: char::from(code).to_string(),
            conflicted,
        };
        if conflicted {
            result.unstaged.push(change(b'U'));
        } else if x == b'?' && y == b'?' {
            result.unstaged.push(change(b'?'));
        } else {
            if x != b' ' {
                result.staged.push(change(x));
            }
            if y != b' ' {
                result.unstaged.push(change(y));
            }
        }
    }
    Ok(result)
}

fn status_in(folder: &Path) -> Result<GitStatus, String> {
    let Some(root) = repository_root(folder)? else {
        return Ok(GitStatus {
            root: None,
            branch: None,
            staged: Vec::new(),
            unstaged: Vec::new(),
        });
    };
    let output = git_output(
        git_command(Path::new(&root)).args([
            "status",
            "--porcelain=v1",
            "--branch",
            "--no-ahead-behind",
            "-z",
            "--untracked-files=all",
            "--renames",
        ]),
        STATUS_LIMIT,
    )?;
    parse_status(&output.stdout, root)
}

#[cfg(windows)]
fn null_device() -> &'static str {
    "NUL"
}

#[cfg(not(windows))]
fn null_device() -> &'static str {
    "/dev/null"
}

fn diff_in(folder: &Path, area: GitArea, path: &str) -> Result<GitDiff, String> {
    validate_path(path)?;
    let status = status_in(folder)?;
    let root = status.root.ok_or("当前目录不是 Git 仓库。")?;
    let files = match area {
        GitArea::Staged => status.staged,
        GitArea::Unstaged => status.unstaged,
    };
    let selected = files
        .iter()
        .find(|file| file.path == path)
        .ok_or(STALE_SELECTION)?;
    let untracked = selected.status == "?";
    if untracked && path.ends_with('/') {
        return Err("这是未跟踪的嵌套 Git 仓库目录，请进入该目录查看文件差异。".to_owned());
    }
    let mut command = git_command(Path::new(&root));
    command.args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--find-renames",
    ]);
    if untracked {
        // In no-index mode a bare "-" means stdin, even after the option separator.
        let disk_path = if path == "-" { "./-" } else { path };
        command.args(["--no-index", "--", null_device(), disk_path]);
    } else {
        if matches!(area, GitArea::Staged) {
            command.arg("--cached");
        }
        command.arg("--");
        command.arg(path);
        if let Some(original_path) = &selected.original_path {
            command.arg(original_path);
        }
    }
    let output = run_git(&mut command, DIFF_LIMIT, true, DEADLINE)?;
    let no_index_difference = untracked
        && output.status.code() == Some(1)
        && !output
            .stderr
            .split(|byte| *byte == b'\n')
            .any(|line| line.starts_with(b"error:") || line.starts_with(b"fatal:"));
    if !output.truncated && !output.status.success() && !no_index_difference {
        return Err(command_error(&output));
    }
    // Git emits creation metadata even for an empty untracked file.
    if untracked
        && !output.truncated
        && std::fs::symlink_metadata(Path::new(&root).join(path))
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 0)
    {
        return Ok(GitDiff {
            text: String::new(),
            kind: GitDiffKind::Empty,
            truncated: false,
        });
    }
    let kind = if selected.conflicted {
        GitDiffKind::Conflict
    } else if output
        .stdout
        .split(|byte| *byte == b'\n')
        .any(|line| line.starts_with(b"Binary files ") || line == b"GIT binary patch")
    {
        GitDiffKind::Binary
    } else if output.stdout.is_empty() {
        GitDiffKind::Empty
    } else {
        GitDiffKind::Text
    };
    // Patch contents may contain legacy encodings. Filenames, unlike patch text,
    // are always checked strictly before being sent through the UTF-8 IPC contract.
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let truncated = output.truncated || text.len() > DIFF_LIMIT;
    if text.len() > DIFF_LIMIT {
        let mut end = DIFF_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok(GitDiff {
        text,
        kind,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempRepo(PathBuf);

    impl TempRepo {
        fn directory() -> Self {
            let path = std::env::temp_dir().join(format!("yiterm-git-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn new() -> Self {
            let repo = Self::directory();
            repo.git(&["init", "--template=", "--initial-branch=main"]);
            repo.git(&["config", "user.name", "YiTerm Tests"]);
            repo.git(&["config", "user.email", "tests@yiterm.invalid"]);
            repo.git(&["config", "commit.gpgsign", "false"]);
            repo.git(&["config", "core.hooksPath", null_device()]);
            repo
        }

        fn command(&self, args: &[&str]) -> Command {
            let mut command = git_command(&self.0);
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", null_device())
                .args(args);
            command
        }

        fn git(&self, args: &[&str]) -> ProcessOutput {
            let output = run_git(&mut self.command(args), STATUS_LIMIT, false, DEADLINE).unwrap();
            assert!(output.status.success(), "{}", command_error(&output));
            output
        }

        fn write(&self, path: &str, bytes: impl AsRef<[u8]>) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        fn commit(&self) {
            self.git(&["add", "--all"]);
            self.git(&["commit", "-m", "fixture"]);
        }

        fn oid(&self, revision: &str) -> String {
            String::from_utf8(self.git(&["rev-parse", "--verify", revision]).stdout)
                .unwrap()
                .trim_end_matches('\n')
                .to_owned()
        }

        fn commit_named(&self, subject: &str) -> String {
            self.git(&[
                "commit",
                "--allow-empty",
                "--date=1700000000 +0000",
                "-m",
                subject,
            ]);
            self.oid("HEAD")
        }

        fn history(&self, limit: usize) -> GitHistory {
            history_in(&self.0, limit).unwrap()
        }

        fn status(&self) -> GitStatus {
            status_in(&self.0).unwrap()
        }

        fn diff(&self, area: GitArea, path: &str) -> GitDiff {
            diff_in(&self.0, area, path).unwrap()
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn commit_details_cover_root_normal_empty_and_full_message_contract() {
        let repo = TempRepo::new();
        repo.write("file", "one\ntwo\n");
        repo.git(&["add", "file"]);
        let root = repo.commit_named("root");
        let details = commit_details_in(&repo.0, &root).unwrap();
        assert_eq!(
            (details.files_changed, details.insertions, details.deletions),
            (1, 2, 0)
        );
        assert!(details.github_url.is_none());

        repo.write("file", "two\nthree\n");
        repo.git(&["add", "file"]);
        let normal = repo.commit_named("normal");
        let details = commit_details_in(&repo.0, &normal).unwrap();
        assert_eq!(
            (details.files_changed, details.insertions, details.deletions),
            (1, 1, 1)
        );

        let message = "完整标题, subject\n\nFirst paragraph.\n\n第二段说明。\n";
        let empty = repo.commit_named(message);
        repo.git(&[
            "config",
            "remote.origin.url",
            "https://user:password@github.com/owner/repo.git?token=secret#fragment",
        ]);
        repo.write("nested/deeper/file", "untracked");
        let details = commit_details_in(&repo.0.join("nested/deeper"), &empty).unwrap();
        assert_eq!(
            serde_json::to_value(&details).unwrap(),
            serde_json::json!({
                "oid": empty,
                "message": message,
                "filesChanged": 0,
                "insertions": 0,
                "deletions": 0,
                "githubUrl": format!("https://github.com/owner/repo/commit/{empty}"),
            })
        );
    }

    #[test]
    fn commit_details_merge_stats_compare_with_the_first_parent() {
        let repo = TempRepo::new();
        repo.write("file", "base\n");
        repo.commit();
        repo.git(&["checkout", "-b", "feature"]);
        repo.write("feature", "added on feature\n");
        repo.commit();
        repo.git(&["checkout", "main"]);
        repo.write("file", "main\n");
        repo.commit();
        repo.git(&["merge", "--no-ff", "-m", "merge", "feature"]);
        let details = commit_details_in(&repo.0, &repo.oid("HEAD")).unwrap();
        assert_eq!(
            (details.files_changed, details.insertions, details.deletions),
            (1, 1, 0)
        );
    }

    #[test]
    fn commit_details_numstat_counts_renames_binary_and_special_paths() {
        let repo = TempRepo::new();
        #[cfg(not(windows))]
        let (old, new, added) = ("old\tname\n.txt", "-new\t中文\n.txt", "new\tfile\n.txt");
        #[cfg(windows)]
        let (old, new, added) = ("old-name.txt", "-new-中文.txt", "new-file.txt");
        repo.write(old, "unchanged content\n");
        repo.write("binary", b"\0\x01\xff");
        repo.commit();
        repo.git(&["mv", "--", old, new]);
        repo.write("binary", b"\0\x02\xff\x03");
        repo.write(added, "first\nsecond\n");
        repo.commit();
        let details = commit_details_in(&repo.0, &repo.oid("HEAD")).unwrap();
        assert_eq!(
            (details.files_changed, details.insertions, details.deletions),
            (3, 2, 0)
        );
    }

    #[test]
    fn commit_details_reject_revision_expressions_options_missing_objects_and_nonrepos() {
        let repo = TempRepo::new();
        repo.write("file", "contents\n");
        repo.commit();
        let oid = repo.oid("HEAD");
        for invalid in [
            "HEAD".to_owned(),
            "HEAD~1".to_owned(),
            "HEAD:file".to_owned(),
            "--all".to_owned(),
            "-p".to_owned(),
            oid[..8].to_owned(),
            format!("{oid}\n"),
            format!("{oid}/file"),
            "g".repeat(40),
        ] {
            assert!(
                commit_details_in(&repo.0, &invalid)
                    .unwrap_err()
                    .contains("对象 ID")
            );
        }
        for invalid_object in [
            "f".repeat(40),
            "f".repeat(64),
            repo.oid("HEAD:file"),
            repo.oid("HEAD^{tree}"),
        ] {
            assert!(commit_details_in(&repo.0, &invalid_object).is_err());
        }
        let plain = TempRepo::directory();
        assert_eq!(
            commit_details_in(&plain.0, &oid).unwrap_err(),
            "当前目录不是 Git 仓库。"
        );
        assert!(commit_details_in(&plain.0.join("missing"), &oid).is_err());
    }

    #[test]
    fn commit_details_github_urls_support_transports_and_discard_credentials_and_queries() {
        let oid = "a".repeat(40);
        let expected = format!("https://github.com/owner/repo/commit/{oid}");
        for remote in [
            "https://github.com/owner/repo.git",
            "https://github.com/owner/repo",
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "git://github.com/owner/repo.git",
            "https://username:password@github.com:443/owner/repo.git?token=secret#fragment",
            "ssh://username:password@github.com:22/owner/repo.git?token=secret#fragment",
            "git@github.com:owner/repo.git?token=secret#fragment",
        ] {
            assert_eq!(
                github_commit_url(remote, &oid).as_deref(),
                Some(expected.as_str()),
                "{remote}"
            );
        }
        for remote in [
            "",
            "/local/repository",
            "https://gitlab.com/owner/repo.git",
            "https://github.com.evil.example/owner/repo.git",
            "https://github.com@evil.example/owner/repo.git",
            "https://github.com/owner",
            "https://github.com/owner/repo/extra",
            "https://github.com/owner/.git",
            "https://github.com/owner/repo%2Fother.git",
            "file://github.com/owner/repo.git",
        ] {
            assert!(github_commit_url(remote, &oid).is_none(), "{remote}");
        }
    }

    #[test]
    fn commit_details_parsers_require_complete_records_and_valid_numbers() {
        assert_eq!(
            parse_commit_numstat(b"2\t1\tpath\twith\ntabs\0-\t-\tbinary\0").unwrap(),
            (2, 2, 1)
        );
        assert_eq!(
            parse_commit_numstat(b"0\t0\t\0old\tpath\0new\npath\0").unwrap(),
            (1, 0, 0)
        );
        assert_eq!(parse_commit_numstat(b"").unwrap(), (0, 0, 0));
        for invalid in [
            b"1\t2\tpath".as_slice(),
            b"1\t2\0",
            b"1\t2\tpath\0\0",
            b"0\t0\t\0old\0",
            b"0\t0\t\0\0new\0",
            b"0\t0\t\0old\0\0",
            b"-\t1\tpath\0",
            b"1\t-\tpath\0",
            b"invalid\t2\tpath\0",
            b"18446744073709551616\t0\tpath\0",
            b"18446744073709551615\t0\tfirst\0\x31\t0\tsecond\0",
        ] {
            assert!(parse_commit_numstat(invalid).is_err(), "{invalid:?}");
        }
        let oid = "a".repeat(40);
        let valid = format!("{oid}\0\0subject\n\nbody\n\0");
        assert_eq!(
            parse_commit_message(valid.as_bytes(), &oid).unwrap(),
            (Vec::new(), "subject\n\nbody\n".to_owned())
        );
        for invalid in [
            valid.as_bytes()[..valid.len() - 1].to_vec(),
            format!("{oid}\0subject\0").into_bytes(),
            format!("{oid}\0invalid-parent\0subject\0").into_bytes(),
            format!("{}\0\0subject\0", "b".repeat(40)).into_bytes(),
            [format!("{oid}\0\0").into_bytes(), b"invalid\xff\0".to_vec()].concat(),
        ] {
            assert!(parse_commit_message(&invalid, &oid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn history_linear_commits_preserve_metadata_and_camel_case_contract() {
        let repo = TempRepo::new();
        repo.git(&["config", "user.name", "作者, YiTerm"]);
        let first = repo.commit_named("初始, commit");
        let second = repo.commit_named("second");
        let third = repo.commit_named("third\n\nbody is not the subject");
        let history = repo.history(100);
        assert_eq!(
            history
                .commits
                .iter()
                .map(|commit| &commit.oid)
                .collect::<Vec<_>>(),
            [&third, &second, &first]
        );
        assert_eq!(history.commits[0].parents, [second.as_str()]);
        assert_eq!(history.commits[1].parents, [first.as_str()]);
        assert!(history.commits[2].parents.is_empty());
        assert_eq!(history.commits[0].subject, "third");
        assert_eq!(history.commits[2].subject, "初始, commit");
        assert_eq!(history.commits[2].author_name, "作者, YiTerm");
        assert_eq!(history.commits[2].author_time, 1_700_000_000);
        assert!(!history.has_more);
        let json = serde_json::to_value(&history).unwrap();
        assert_eq!(
            json["head"],
            serde_json::json!({ "oid": third, "refName": "refs/heads/main" })
        );
        assert_eq!(
            json["commits"][2],
            serde_json::json!({
                "oid": first,
                "parents": [],
                "subject": "初始, commit",
                "authorName": "作者, YiTerm",
                "authorTime": 1_700_000_000,
            })
        );
        assert_eq!(
            json["refs"][0],
            serde_json::json!({
                "name": "refs/heads/main",
                "shortName": "main",
                "kind": "branch",
                "oid": history.head.oid,
            })
        );
        assert_eq!(json["hasMore"], false);
        assert_eq!(json.as_object().unwrap().len(), 5);
    }

    #[test]
    fn history_merge_keeps_parent_order_and_topological_order() {
        let repo = TempRepo::new();
        let base = repo.commit_named("base");
        repo.git(&["checkout", "-b", "feature"]);
        let feature = repo.commit_named("feature");
        repo.git(&["checkout", "main"]);
        let main = repo.commit_named("main");
        repo.git(&["merge", "--no-ff", "-m", "merge feature", "feature"]);
        let merged = repo.oid("HEAD");
        let history = repo.history(100);
        assert_eq!(history.commits.len(), 4);
        assert_eq!(history.commits[0].oid, merged);
        assert_eq!(history.commits[0].parents, [main, feature]);
        assert_eq!(history.commits.last().unwrap().oid, base);
        for (index, commit) in history.commits.iter().enumerate() {
            for parent in &commit.parents {
                let parent_index = history
                    .commits
                    .iter()
                    .position(|item| &item.oid == parent)
                    .unwrap();
                assert!(parent_index > index);
            }
        }
    }

    #[test]
    fn history_includes_detached_head_without_any_ref_to_its_commit() {
        let repo = TempRepo::new();
        let base = repo.commit_named("base");
        repo.git(&["checkout", "--detach", "HEAD"]);
        let detached = repo.commit_named("unreferenced detached commit");
        let history = repo.history(100);
        assert_eq!(history.head.oid.as_deref(), Some(detached.as_str()));
        assert!(history.head.ref_name.is_none());
        assert_eq!(history.commits[0].oid, detached);
        assert_eq!(history.commits[0].parents, [base]);
        assert!(
            history
                .refs
                .iter()
                .all(|reference| reference.oid != detached)
        );
        assert!(serde_json::to_value(&history).unwrap()["head"]["refName"].is_null());
    }

    #[test]
    fn history_distinguishes_empty_unborn_nonrepo_and_nested_folders() {
        let repo = TempRepo::new();
        let empty = repo.history(100);
        assert!(empty.root.is_some());
        assert!(empty.commits.is_empty());
        assert!(empty.refs.is_empty());
        assert!(empty.head.oid.is_none());
        assert_eq!(empty.head.ref_name.as_deref(), Some("refs/heads/main"));
        assert!(!empty.has_more);

        let base = repo.commit_named("existing branch");
        repo.git(&["checkout", "--orphan", "pending/中文,branch"]);
        let unborn = repo.history(100);
        assert!(unborn.head.oid.is_none());
        assert_eq!(
            unborn.head.ref_name.as_deref(),
            Some("refs/heads/pending/中文,branch")
        );
        assert_eq!(unborn.commits[0].oid, base);
        assert_eq!(unborn.refs[0].name, "refs/heads/main");
        repo.write("嵌套/deeper/file", "untracked\n");
        assert_eq!(
            history_in(&repo.0.join("嵌套/deeper"), 100).unwrap(),
            unborn
        );

        let plain = TempRepo::directory();
        let nonrepo = plain.history(100);
        assert_eq!(
            serde_json::to_value(&nonrepo).unwrap(),
            serde_json::json!({
                "root": null,
                "commits": [],
                "refs": [],
                "head": { "oid": null, "refName": null },
                "hasMore": false,
            })
        );
        assert!(history_in(&plain.0.join("missing"), 100).is_err());
        plain.write("file", "");
        assert!(history_in(&plain.0.join("file"), 100).is_err());
    }

    #[test]
    fn history_refs_peel_tags_and_preserve_full_unicode_comma_names() {
        let repo = TempRepo::new();
        repo.write("file", "blob contents\n");
        repo.git(&["add", "file"]);
        let commit = repo.commit_named("tag target");
        repo.git(&["branch", "共同,名称"]);
        repo.git(&["tag", "共同,名称"]);
        repo.git(&["tag", "-a", "注释,标签", "-m", "annotated"]);
        repo.git(&["tag", "-a", "nested", "注释,标签", "-m", "nested annotated"]);
        repo.git(&["tag", "-a", "nested-again", "nested", "-m", "twice nested"]);
        let blob = repo.oid("HEAD:file");
        let tree = repo.oid("HEAD^{tree}");
        repo.git(&["tag", "blob", &blob]);
        repo.git(&["tag", "tree", &tree]);
        repo.git(&["tag", "-a", "annotated-blob", &blob, "-m", "blob"]);
        repo.git(&[
            "tag",
            "-a",
            "nested-blob",
            "annotated-blob",
            "-m",
            "nested blob",
        ]);
        repo.git(&["tag", "-a", "annotated-tree", &tree, "-m", "tree"]);
        repo.git(&["update-ref", "refs/remotes/origin/中文,分支", &commit]);
        repo.git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/中文,分支",
        ]);
        // A direct remote ref named HEAD is a real ref, rather than a symbolic alias.
        repo.git(&["update-ref", "refs/remotes/other/HEAD", &commit]);
        let history = repo.history(100);
        let names: Vec<_> = history
            .refs
            .iter()
            .map(|reference| reference.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "refs/heads/main",
                "refs/heads/共同,名称",
                "refs/remotes/origin/中文,分支",
                "refs/remotes/other/HEAD",
                "refs/tags/nested",
                "refs/tags/nested-again",
                "refs/tags/共同,名称",
                "refs/tags/注释,标签",
            ]
        );
        assert!(history.refs.iter().all(|reference| reference.oid == commit));
        assert_eq!(history.refs[1].short_name, "共同,名称");
        assert_eq!(history.refs[2].short_name, "origin/中文,分支");
        assert_eq!(history.refs[2].kind, GitRefKind::Remote);
        assert_eq!(history.refs[7].kind, GitRefKind::Tag);
        let json = serde_json::to_value(&history).unwrap();
        assert_eq!(json["refs"][2]["kind"], "remote");
        assert_eq!(json["refs"][7]["kind"], "tag");
    }

    #[test]
    fn history_limits_use_one_extra_commit_and_clamp_to_one_thousand() {
        use std::io::Write;

        let repo = TempRepo::new();
        repo.commit_named("first");
        assert!(!repo.history(1).has_more);
        repo.commit_named("second");
        assert!(repo.history(1).has_more);
        assert_eq!(repo.history(0), repo.history(1));
        assert!(!repo.history(2).has_more);
        assert!(!repo.history(3).has_more);
        assert_eq!(repo.history(1).commits[0].parents.len(), 1);

        // Import a real, long linear history in one Git process.
        let mut child = repo
            .command(&["fast-import", "--quiet"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            let mut input = child.stdin.take().unwrap();
            for index in 0..1001 {
                let subject = format!("imported {index}");
                write!(input, "commit refs/heads/imported\ncommitter YiTerm Tests <tests@yiterm.invalid> 1700000000 +0000\ndata {}\n{subject}\n\n", subject.len()).unwrap();
            }
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let capped = repo.history(usize::MAX);
        assert_eq!(capped, repo.history(1000));
        assert_eq!(capped.commits.len(), 1000);
        assert!(capped.has_more);
    }

    #[test]
    fn history_retries_head_changes_once_and_rejects_continuous_ref_changes() {
        let repo = TempRepo::new();
        repo.commit_named("base");
        let root = repository_root(&repo.0).unwrap().unwrap();
        let mut reads = 0;
        let history = history_at(root.clone(), 100, |path| {
            reads += 1;
            if reads == 2 {
                // OID and refs stay the same: changing just HEAD's symbolic name counts.
                repo.git(&["checkout", "--detach", "HEAD"]);
            }
            read_history_snapshot(path)
        })
        .unwrap();
        assert_eq!(reads, 4);
        assert!(history.head.ref_name.is_none());

        reads = 0;
        let error = history_at(root, 100, |path| {
            reads += 1;
            if reads == 2 || reads == 4 {
                repo.git(&[
                    "update-ref",
                    &format!("refs/yiterm-tests/moving-{reads}"),
                    "HEAD",
                ]);
            }
            read_history_snapshot(path)
        })
        .unwrap_err();
        assert_eq!(reads, 4);
        assert_eq!(error, HISTORY_CHANGED);
    }

    #[test]
    fn history_traverses_other_ref_namespaces_without_returning_them_as_labels() {
        let repo = TempRepo::new();
        repo.commit_named("base");
        repo.git(&["checkout", "--detach", "HEAD"]);
        let other = repo.commit_named("only reachable from another namespace");
        repo.git(&["checkout", "main"]);
        repo.git(&["update-ref", "refs/yiterm-tests/other", &other]);
        let history = repo.history(100);
        assert!(history.commits.iter().any(|commit| commit.oid == other));
        assert_eq!(history.refs.len(), 1);
        assert_eq!(history.refs[0].name, "refs/heads/main");
    }

    #[test]
    fn history_log_parser_rejects_incomplete_or_invalid_fields() {
        let oid = "a".repeat(40);
        let parent = "b".repeat(40);
        let record = format!("{oid}\0{parent}\0作者, name\01700000000\0subject, 中文\0");
        let parsed = parse_history_log(record.as_bytes()).unwrap();
        assert_eq!(parsed[0].parents, [parent.as_str()]);
        assert_eq!(parsed[0].author_time, 1_700_000_000);
        assert_eq!(parsed[0].subject, "subject, 中文");
        assert!(parse_history_log(b"").unwrap().is_empty());
        let empty = format!("{oid}\0\0\00\0\0");
        let empty = parse_history_log(empty.as_bytes()).unwrap();
        assert!(empty[0].parents.is_empty());
        assert!(empty[0].subject.is_empty());
        assert!(empty[0].author_name.is_empty());
        let sha256 = format!("{}\0\0author\0-1\0subject\0", "c".repeat(64));
        assert_eq!(
            parse_history_log(sha256.as_bytes()).unwrap()[0].author_time,
            -1
        );
        for invalid in [
            record.as_bytes()[..record.len() - 1].to_vec(),
            format!("{record}\0").into_bytes(),
            format!("{oid}\0{parent}\0author\01700000000\0").into_bytes(),
            b"short\0\0author\x000\0subject\0".to_vec(),
            format!("{}\0\0author\00\0subject\0", "g".repeat(40)).into_bytes(),
            format!("{oid}\0{parent} \0author\00\0subject\0").into_bytes(),
            format!("{oid}\0bad-parent\0author\00\0subject\0").into_bytes(),
            format!("{oid}\0\0author\0invalid\0subject\0").into_bytes(),
            format!("{oid}\0\0author\09223372036854775808\0subject\0").into_bytes(),
            [
                format!("{oid}\0\0author\00\0").into_bytes(),
                b"invalid\xff\0".to_vec(),
            ]
            .concat(),
        ] {
            assert!(parse_history_log(&invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn history_ref_parser_rejects_truncation_invalid_ids_and_non_utf8_names() {
        let oid = "a".repeat(40);
        let record = format!("refs/heads/中文,name\0{oid}\0commit\0\0\0\0\n");
        assert_eq!(
            parse_history_refs(record.as_bytes()).unwrap()[0].short_name,
            "中文,name"
        );
        assert!(parse_history_refs(b"").unwrap().is_empty());
        for invalid in [
            record.as_bytes()[..record.len() - 1].to_vec(),
            record.as_bytes()[..record.len() - 2].to_vec(),
            format!("{record}\n").into_bytes(),
            format!("refs/heads/main\0{oid}\0commit\0\0\0\n").into_bytes(),
            format!("refs/heads/\0{oid}\0commit\0\0\0\0\n").into_bytes(),
            format!("refs/heads/with space\0{oid}\0commit\0\0\0\0\n").into_bytes(),
            format!("heads/other/name\0{oid}\0commit\0\0\0\0\n").into_bytes(),
            b"refs/heads/name\0short\0commit\0\0\0\0\n".to_vec(),
            format!("refs/tags/tag\0{oid}\0tag\0bad\0commit\0\0\n").into_bytes(),
            format!("refs/tags/tag\0{oid}\0tag\0{oid}\0unknown\0\0\n").into_bytes(),
            format!("refs/tags/tag\0{oid}\0unknown\0\0\0\0\n").into_bytes(),
            [
                b"refs/heads/invalid\xff\0".to_vec(),
                format!("{oid}\0commit\0\0\0\0\n").into_bytes(),
            ]
            .concat(),
        ] {
            assert!(parse_history_refs(&invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn history_rejects_oversized_log_and_refs_instead_of_parsing_prefixes() {
        let repo = TempRepo::new();
        let mut message = "large subject ".repeat(HISTORY_OUTPUT_LIMIT / 10);
        message.push('\n');
        repo.write("message", message);
        repo.git(&["commit", "--quiet", "--allow-empty", "-F", "message"]);
        let error = history_in(&repo.0, 1).unwrap_err();
        assert_eq!(
            error,
            format!("Git 输出超过 {HISTORY_OUTPUT_LIMIT} 字节限制。")
        );

        let repo = TempRepo::new();
        let commit = repo.commit_named("base");
        // Packed refs provide a large real for-each-ref fixture without thousands of processes.
        let mut packed = String::from("# pack-refs with: sorted\n");
        for index in 0..30000 {
            packed.push_str(&format!(
                "{commit} refs/heads/{index:05}-{}\n",
                "x".repeat(96)
            ));
        }
        repo.write(".git/packed-refs", packed);
        assert_eq!(
            history_in(&repo.0, 1).unwrap_err(),
            format!("Git 输出超过 {HISTORY_OUTPUT_LIMIT} 字节限制。")
        );
    }

    #[test]
    fn branch_headers_preserve_names_without_tracking_metadata() {
        for (header, expected) in [
            ("main", Some("main")),
            ("main...origin/main [ahead 2, behind 1]", Some("main")),
            (
                "feature/review...origin/feature/review [gone]",
                Some("feature/review"),
            ),
            ("No commits yet on feature/new", Some("feature/new")),
            ("Initial commit on legacy", Some("legacy")),
            (
                "feature/中文\u{00a0}branch",
                Some("feature/中文\u{00a0}branch"),
            ),
            ("HEAD (no branch)", None),
        ] {
            let raw = format!("## {header}\0?? file.txt\0");
            let status = parse_status(raw.as_bytes(), "/repo".to_owned()).unwrap();
            assert_eq!(status.branch.as_deref(), expected, "{header}");
            assert_eq!(status.unstaged[0].path, "file.txt");
        }
        assert!(parse_branch(b"invalid\xff").is_err());
        assert!(parse_branch(b"No commits yet on ").is_err());
    }

    #[test]
    fn branch_status_tracks_unborn_tracking_and_detached_heads() {
        let repo = TempRepo::new();
        assert_eq!(repo.status().branch.as_deref(), Some("main"));
        repo.write("file.txt", "base\n");
        repo.commit();
        repo.git(&["remote", "add", "origin", "."]);
        repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        repo.git(&["branch", "--set-upstream-to=origin/main", "main"]);
        let tracking = repo.status();
        assert_eq!(tracking.branch.as_deref(), Some("main"));
        assert_eq!(serde_json::to_value(&tracking).unwrap()["branch"], "main");

        repo.git(&["checkout", "-b", "feature/branch-footer"]);
        assert_eq!(
            repo.status().branch.as_deref(),
            Some("feature/branch-footer")
        );
        repo.git(&["checkout", "--detach", "HEAD"]);
        let detached = repo.status();
        assert!(detached.root.is_some());
        assert!(detached.branch.is_none());
        assert!(serde_json::to_value(&detached).unwrap()["branch"].is_null());

        let plain = TempRepo::directory().status();
        assert!(plain.root.is_none());
        assert!(plain.branch.is_none());
    }

    #[test]
    fn staged_and_unstaged_diffs_use_their_own_versions() {
        let repo = TempRepo::new();
        repo.write("file.txt", "base\n");
        repo.commit();
        repo.write("file.txt", "index\n");
        repo.git(&["add", "--", "file.txt"]);
        repo.write("file.txt", "working\n");
        let status = repo.status();
        assert_eq!(status.staged[0].status, "M");
        assert_eq!(status.unstaged[0].status, "M");
        let staged = repo.diff(GitArea::Staged, "file.txt");
        assert_eq!(staged.kind, GitDiffKind::Text);
        assert!(staged.text.contains("-base\n+index\n"));
        assert!(!staged.text.contains("working"));
        let unstaged = repo.diff(GitArea::Unstaged, "file.txt");
        assert!(unstaged.text.contains("-index\n+working\n"));
        assert!(!unstaged.truncated);
    }

    #[test]
    fn untracked_nested_repository_does_not_hide_other_changes() {
        let repo = TempRepo::new();
        repo.git(&["init", "--template=", "nested"]);
        repo.write("nested/inside.txt", "nested file\n");
        repo.write("regular.txt", "regular file\n");
        let status = repo.status();
        assert!(status.unstaged.iter().any(|file| file.path == "nested/"));
        assert!(
            status
                .unstaged
                .iter()
                .any(|file| file.path == "regular.txt")
        );
        assert!(
            diff_in(&repo.0, GitArea::Unstaged, "nested/")
                .unwrap_err()
                .contains("嵌套")
        );
    }

    #[test]
    fn unborn_adds_work_without_head() {
        let repo = TempRepo::new();
        repo.write("new.txt", "first\n");
        repo.git(&["add", "--", "new.txt"]);
        let status = repo.status();
        assert_eq!(status.staged[0].status, "A");
        assert!(status.unstaged.is_empty());
        let diff = repo.diff(GitArea::Staged, "new.txt");
        assert!(diff.text.contains("new file mode"));
        assert!(diff.text.contains("+first\n"));
        let json = serde_json::to_value(&status).unwrap();
        assert!(json["staged"][0]["originalPath"].is_null());
        assert_eq!(
            serde_json::to_string(&GitArea::Unstaged).unwrap(),
            "\"unstaged\""
        );
        assert_eq!(serde_json::to_value(&diff).unwrap()["kind"], "text");
    }

    #[test]
    fn rename_has_destination_then_original_and_both_paths_in_diff() {
        let repo = TempRepo::new();
        repo.write("old name.txt", "same\n".repeat(20));
        repo.commit();
        repo.git(&["mv", "--", "old name.txt", "new name.txt"]);
        repo.write("new name.txt", format!("{}extra\n", "same\n".repeat(20)));
        let status = repo.status();
        assert_eq!(status.staged[0].path, "new name.txt");
        assert_eq!(
            status.staged[0].original_path.as_deref(),
            Some("old name.txt")
        );
        assert_eq!(status.staged[0].status, "R");
        assert_eq!(status.unstaged[0].path, "new name.txt");
        let staged = repo.diff(GitArea::Staged, "new name.txt");
        assert!(staged.text.contains("rename from old name.txt"));
        assert!(staged.text.contains("rename to new name.txt"));
        let unstaged = repo.diff(GitArea::Unstaged, "new name.txt");
        assert!(unstaged.text.contains("+extra\n"));
    }

    #[test]
    fn literal_unicode_spaces_newlines_and_leading_dashes() {
        let repo = TempRepo::new();
        #[cfg(not(windows))]
        let names = [
            "-option",
            "中文 空格.txt",
            "line\nbreak.txt",
            "[ab]*?.txt",
            ":(glob)*",
        ];
        #[cfg(windows)]
        let names = ["-option", "中文 空格.txt", "[ab].txt"];
        for name in names {
            repo.write(name, "before\n");
        }
        repo.commit();
        for (index, name) in names.iter().enumerate() {
            repo.write(name, format!("after {index}\n"));
        }
        let status = repo.status();
        assert_eq!(status.unstaged.len(), names.len());
        for (index, name) in names.iter().enumerate() {
            assert!(status.unstaged.iter().any(|change| change.path == *name));
            let diff = repo.diff(GitArea::Unstaged, name);
            assert!(diff.text.contains(&format!("+after {index}\n")));
            assert_eq!(diff.text.matches("diff --git ").count(), 1);
        }
        #[cfg(not(windows))]
        let untracked_name = "untracked\n[字]*";
        #[cfg(windows)]
        let untracked_name = "untracked[字]";
        repo.write(untracked_name, "untracked\n");
        let diff = repo.diff(GitArea::Unstaged, untracked_name);
        assert!(diff.text.contains("+untracked\n"));
        repo.write("-", "dash filename\n");
        assert!(
            repo.diff(GitArea::Unstaged, "-")
                .text
                .contains("+dash filename\n")
        );
    }

    #[test]
    fn untracked_text_empty_and_binary_and_tracked_binary() {
        let repo = TempRepo::new();
        repo.write("text", "hello\n");
        repo.write("empty", "");
        repo.write("binary", b"\0\x01\xff");
        assert_eq!(repo.status().unstaged.len(), 3);
        assert!(repo.status().unstaged.iter().all(|file| file.status == "?"));
        assert_eq!(repo.diff(GitArea::Unstaged, "text").kind, GitDiffKind::Text);
        let empty = repo.diff(GitArea::Unstaged, "empty");
        assert_eq!(empty.kind, GitDiffKind::Empty);
        assert!(empty.text.is_empty());
        assert_eq!(
            repo.diff(GitArea::Unstaged, "binary").kind,
            GitDiffKind::Binary
        );
        repo.commit();
        repo.write("binary", b"\0changed");
        assert_eq!(
            repo.diff(GitArea::Unstaged, "binary").kind,
            GitDiffKind::Binary
        );
    }

    #[test]
    fn staged_and_unstaged_deletions() {
        let repo = TempRepo::new();
        repo.write("gone", "deleted content\n");
        repo.commit();
        std::fs::remove_file(repo.0.join("gone")).unwrap();
        assert_eq!(repo.status().unstaged[0].status, "D");
        assert!(
            repo.diff(GitArea::Unstaged, "gone")
                .text
                .contains("-deleted content\n")
        );
        repo.git(&["add", "--", "gone"]);
        assert_eq!(repo.status().staged[0].status, "D");
        assert!(
            repo.diff(GitArea::Staged, "gone")
                .text
                .contains("deleted file mode")
        );
    }

    #[test]
    fn conflicts_are_only_unstaged_and_have_conflict_kind() {
        let repo = TempRepo::new();
        repo.write("conflict", "base\n");
        repo.commit();
        repo.git(&["checkout", "-b", "other"]);
        repo.write("conflict", "other\n");
        repo.commit();
        repo.git(&["checkout", "main"]);
        repo.write("conflict", "main\n");
        repo.commit();
        let merge = run_git(
            &mut repo.command(&["merge", "other"]),
            STATUS_LIMIT,
            false,
            DEADLINE,
        )
        .unwrap();
        assert_eq!(merge.status.code(), Some(1));
        let status = repo.status();
        assert!(status.staged.is_empty());
        assert_eq!(status.unstaged.len(), 1);
        assert!(status.unstaged[0].conflicted);
        assert_eq!(status.unstaged[0].status, "U");
        assert_eq!(
            repo.diff(GitArea::Unstaged, "conflict").kind,
            GitDiffKind::Conflict
        );
        for pair in ["DD", "AU", "UD", "UA", "DU", "AA", "UU"] {
            let parsed =
                parse_status(format!("{pair} conflict\0").as_bytes(), "/repo".to_owned()).unwrap();
            assert!(parsed.staged.is_empty());
            assert_eq!(parsed.unstaged[0].status, "U");
            assert!(parsed.unstaged[0].conflicted);
        }
    }

    #[test]
    fn discovers_nested_root_and_distinguishes_nonrepo_and_errors() {
        let repo = TempRepo::new();
        #[cfg(not(windows))]
        let repo = {
            let mut repo = repo;
            let renamed = repo.0.with_extension("中文 空格\n");
            std::fs::rename(&repo.0, &renamed).unwrap();
            repo.0 = renamed;
            repo
        };
        repo.write("nested/deeper/file", "new\n");
        let root = repo.status().root.unwrap();
        let nested = status_in(&repo.0.join("nested/deeper")).unwrap();
        assert_eq!(nested.root.as_deref(), Some(root.as_str()));
        assert_eq!(nested.unstaged[0].path, "nested/deeper/file");
        let plain = TempRepo::directory();
        assert!(status_in(&plain.0).unwrap().root.is_none());
        assert!(status_in(&plain.0.join("missing")).is_err());
        plain.write("file", "");
        assert!(status_in(&plain.0.join("file")).is_err());
        let mut missing_git = Command::new(plain.0.join("missing-git"));
        assert!(
            run_git(&mut missing_git, 1024, false, DEADLINE)
                .unwrap_err()
                .contains("未找到 Git")
        );
        std::fs::write(repo.0.join(".git/config"), "[invalid config\n").unwrap();
        assert!(status_in(&repo.0).is_err());
    }

    #[test]
    fn rejects_stale_wrong_area_and_nonrelative_selections() {
        let repo = TempRepo::new();
        repo.write("new", "new\n");
        assert_eq!(
            diff_in(&repo.0, GitArea::Staged, "new").unwrap_err(),
            STALE_SELECTION
        );
        repo.git(&["add", "--", "new"]);
        assert_eq!(
            diff_in(&repo.0, GitArea::Unstaged, "new").unwrap_err(),
            STALE_SELECTION
        );
        repo.commit();
        assert_eq!(
            diff_in(&repo.0, GitArea::Staged, "new").unwrap_err(),
            STALE_SELECTION
        );
        for path in [
            "",
            "/absolute",
            "../outside",
            "a/../outside",
            "./new",
            "a//b",
            "a\0b",
        ] {
            assert!(
                diff_in(&repo.0, GitArea::Unstaged, path).is_err(),
                "{path:?}"
            );
        }
    }

    #[test]
    fn parses_copy_records_and_rejects_non_utf8_or_incomplete_records() {
        let status = parse_status(b"C  copied\0original\0", "/repo".to_owned()).unwrap();
        assert_eq!(status.staged[0].status, "C");
        assert_eq!(status.staged[0].original_path.as_deref(), Some("original"));
        for bytes in [
            b"?? invalid\xff\0".as_slice(),
            b"R  new\0",
            b"?? incomplete",
            b"\0?? extra\0",
        ] {
            assert!(parse_status(bytes, "/repo".to_owned()).is_err());
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt;
            let repo = TempRepo::new();
            std::fs::write(
                repo.0
                    .join(std::ffi::OsString::from_vec(b"invalid\xff".to_vec())),
                "",
            )
            .unwrap();
            assert!(status_in(&repo.0).unwrap_err().contains("UTF-8"));
        }
    }

    #[test]
    fn bounded_diff_and_status_overflow() {
        let repo = TempRepo::new();
        repo.write(
            "large",
            "large line of diff content\n".repeat(DIFF_LIMIT / 10),
        );
        let diff = repo.diff(GitArea::Unstaged, "large");
        assert_eq!(diff.kind, GitDiffKind::Text);
        assert!(diff.truncated);
        assert!(diff.text.len() <= DIFF_LIMIT);
        assert!(diff.text.starts_with("diff --git "));
        let mut status = repo.command(&["status", "--porcelain=v1", "-z"]);
        assert_eq!(
            run_git(&mut status, 2, false, DEADLINE).unwrap_err(),
            "Git 输出超过 2 字节限制。"
        );
    }

    #[test]
    fn bounded_stderr_and_deadline_reap_the_process() {
        let repo = TempRepo::new();
        let mut error = repo.command(&["rev-parse", "--verify", "missing"]);
        // A small stdout bound does not affect the independently bounded stderr.
        let output = run_git(&mut error, 1, false, DEADLINE).unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
        let mut error = repo.command(&[
            "-c",
            "alias.yiterm-stderr=!printf '%040000d' 0 >&2",
            "yiterm-stderr",
        ]);
        let result = run_git(&mut error, 1, false, DEADLINE);
        assert!(result.unwrap_err().contains("错误输出"));
        let mut command = repo.command(&["--version"]);
        assert!(
            run_git(&mut command, 1024, false, Duration::ZERO)
                .unwrap_err()
                .contains("超时")
        );
        #[cfg(unix)]
        {
            let mut command = repo.command(&["-c", "alias.yiterm-wait=!sleep 5", "yiterm-wait"]);
            let started = Instant::now();
            assert!(
                run_git(&mut command, 1024, false, Duration::from_millis(50))
                    .unwrap_err()
                    .contains("超时")
            );
            assert!(started.elapsed() < Duration::from_secs(2));
        }
    }
}
