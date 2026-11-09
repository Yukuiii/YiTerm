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
const STDERR_LIMIT: usize = 32 * 1024;
const DEADLINE: Duration = Duration::from_secs(10);
const STALE_SELECTION: &str = "文件状态已变化，请刷新 Git 状态后重试。";

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
        return Err("Git 状态输出超过 4 MiB 限制。".to_owned());
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
    let path = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    let path = std::str::from_utf8(path)
        .map_err(|_| "Git 仓库路径包含无法通过 UTF-8 传输的字符。".to_owned())?;
    if path.is_empty() || !Path::new(path).is_absolute() {
        return Err("Git 返回了无效的仓库根目录。".to_owned());
    }
    Ok(Some(path.to_owned()))
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
    let output = run_git(
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
        false,
        DEADLINE,
    )?;
    if !output.status.success() {
        return Err(command_error(&output));
    }
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
        assert!(
            run_git(&mut status, 2, false, DEADLINE)
                .unwrap_err()
                .contains("限制")
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
