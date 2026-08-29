use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::Notify;

#[derive(Debug)]
pub(crate) struct Aborted;

impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("aborted")
    }
}

impl std::error::Error for Aborted {}

pub(crate) fn aborted() -> anyhow::Error {
    anyhow::Error::new(Aborted)
}

pub(crate) fn is_abort(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Aborted>().is_some()
}

const MAX_READ_BYTES: usize = 50 * 1024;
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_BASH_BYTES: usize = 20_000;

pub(crate) fn clip_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn cap_utf8(text: &mut String, max: usize) {
    if text.len() > max {
        let n = clip_utf8(text, max).len();
        text.truncate(n);
        text.push_str("\n... (truncated)");
    }
}

pub(crate) struct Abort {
    flag: AtomicBool,
    notify: Notify,
}

impl Abort {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            flag: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    pub fn abort(&self) {
        self.flag.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }

    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    pub fn clear(&self) {
        self.flag.store(false, Ordering::Relaxed);
    }

    pub async fn wait(&self) {
        loop {
            if self.is_set() {
                return;
            }
            let notified = self.notify.notified();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }
}

pub(crate) struct Property {
    pub name: &'static str,
    pub r#type: &'static str,
    pub description: &'static str,
    pub required: bool,
}

pub(crate) struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub properties: &'static [Property],
    pub execute: Option<fn(&Path, &Value) -> Result<String>>,
}

fn resolve_path(workspace: &Path, path: &str) -> Result<PathBuf> {
    if path.is_empty() {
        bail!("empty path");
    }
    let root = workspace.canonicalize().unwrap_or_else(|_| clean(workspace));
    let requested = Path::new(path);
    let candidate = if requested.is_absolute() {
        clean(requested)
    } else {
        clean(&root.join(requested))
    };
    if let Ok(real) = candidate.canonicalize() {
        return Ok(real);
    }
    if let Some(parent) = candidate.parent()
        && let Ok(real_parent) = parent.canonicalize()
    {
        if let Some(name) = candidate.file_name() {
            return Ok(real_parent.join(name));
        }
    }
    Ok(candidate)
}

fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(c);
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn tool_bash() -> Tool {
    Tool {
        name: "bash",
        description: "Run a shell command in the workspace. Combined stdout+stderr, capped at 20KB. The command itself is not sandboxed.",
        properties: &[Property {
            name: "cmd",
            r#type: "string",
            description: "Shell command",
            required: true,
        }],
        execute: None,
    }
}

fn format_bash_output(stdout: &[u8], stderr: &[u8], status: std::process::ExitStatus) -> Result<String> {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(stderr));
    cap_utf8(&mut text, MAX_BASH_BYTES);
    if text.is_empty() {
        text = "(no output)".into();
    }
    if status.success() {
        Ok(text)
    } else {
        Err(anyhow!("exit {status}: {text}"))
    }
}

async fn bash_execute_async(workspace: &Path, raw: &Value, abort: &Abort) -> Result<String> {
    #[derive(Deserialize)]
    struct Args {
        cmd: String,
    }
    let a: Args = Args::deserialize(raw).context("bash args")?;
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&a.cmd)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn bash")?;
    let mut stdout = child.stdout.take().context("bash stdout")?;
    let mut stderr = child.stderr.take().context("bash stderr")?;
    let out_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).await?;
        Ok::<_, std::io::Error>(buf)
    });
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        stderr.read_to_end(&mut buf).await?;
        Ok::<_, std::io::Error>(buf)
    });
    let status = tokio::select! {
        status = child.wait() => status.context("wait bash")?,
        _ = abort.wait() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(aborted());
        }
    };
    let stdout = out_task.await.context("bash stdout task")?.context("read bash stdout")?;
    let stderr = err_task.await.context("bash stderr task")?.context("read bash stderr")?;
    format_bash_output(&stdout, &stderr, status)
}

pub(crate) async fn execute_tool(
    tool: &Tool,
    workspace: &Path,
    args: &Value,
    abort: &Abort,
) -> Result<String> {
    if abort.is_set() {
        return Err(aborted());
    }
    if tool.name == "bash" {
        return bash_execute_async(workspace, args, abort).await;
    }
    let Some(exec) = tool.execute else {
        bail!("unknown tool: {}", tool.name);
    };
    let workspace = workspace.to_path_buf();
    let args = args.clone();
    // spawn_blocking cannot be cancelled; we stop waiting on abort/timeout.
    let handle = tokio::task::spawn_blocking(move || exec(&workspace, &args));
    tokio::select! {
        r = handle => match r {
            Ok(Ok(s)) => Ok(s),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(anyhow!("tool task: {e}")),
        },
        _ = tokio::time::sleep(TOOL_TIMEOUT) => bail!("tool timed out after {}s", TOOL_TIMEOUT.as_secs()),
        _ = abort.wait() => Err(aborted()),
    }
}

fn tool_read() -> Tool {
    Tool {
        name: "read",
        description: "Read a file. Absolute paths and paths outside the workspace are allowed. A directory lists names. Lines are numbered for display only — never copy those numbers into edit/write. Binary files (including PNG) return type/size instead of failing UTF-8.",
        properties: &[
            Property {
                name: "path",
                r#type: "string",
                description: "File path",
                required: true,
            },
            Property {
                name: "offset",
                r#type: "integer",
                description: "0-based start line",
                required: false,
            },
            Property {
                name: "limit",
                r#type: "integer",
                description: "Max lines (default 500)",
                required: false,
            },
        ],
        execute: Some(read_execute),
    }
}

fn read_execute(workspace: &Path, raw: &Value) -> Result<String> {
    #[derive(Deserialize)]
    struct Args {
        path: String,
        offset: Option<u32>,
        limit: Option<u32>,
    }
    let a: Args = Args::deserialize(raw).context("read args")?;
    let path = resolve_path(workspace, &a.path)?;
    if path.is_dir() {
        return list_dir(&path);
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    if let Some(desc) = binary_read_summary(&path, &bytes) {
        return Ok(desc);
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow!("{}: stream did not contain valid UTF-8", path.display()))?;
    let start = a.offset.unwrap_or(0) as usize;
    let want = a.limit.unwrap_or(500) as usize;
    let width = (start + want).max(1).to_string().len();
    let mut out = String::new();
    let mut seen = 0usize;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i < start {
            continue;
        }
        if seen >= want {
            break;
        }
        let line = line.trim_end_matches(['\n', '\r']);
        let row = format!("{:>width$}\t{line}\n", i + 1, width = width);
        if out.len() + row.len() > MAX_READ_BYTES {
            out.push_str(&format!("... (truncated at 50KB; offset={i})\n"));
            break;
        }
        out.push_str(&row);
        seen += 1;
    }
    if out.is_empty() {
        Ok("(empty)".into())
    } else {
        Ok(out)
    }
}

fn binary_read_summary(path: &Path, bytes: &[u8]) -> Option<String> {
    if let Some((w, h)) = png_size(bytes) {
        return Some(format!(
            "(binary image: PNG {w}x{h}, {} bytes)",
            bytes.len()
        ));
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some(format!("(binary image: JPEG, {} bytes)", bytes.len()));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(format!("(binary image: GIF, {} bytes)", bytes.len()));
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Some(format!("(binary image: WebP, {} bytes)", bytes.len()));
    }
    if bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        return Some(format!("(binary file: {name}, {} bytes)", bytes.len()));
    }
    None
}

fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    if &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

fn tool_write() -> Tool {
    Tool {
        name: "write",
        description: "Create or overwrite a file. Absolute paths and paths outside the workspace are allowed.",
        properties: &[
            Property {
                name: "path",
                r#type: "string",
                description: "File path",
                required: true,
            },
            Property {
                name: "content",
                r#type: "string",
                description: "Full contents",
                required: true,
            },
        ],
        execute: Some(write_execute),
    }
}

fn list_dir(path: &Path) -> Result<String> {
    let mut names = Vec::new();
    for ent in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let ent = ent.with_context(|| format!("read {}", path.display()))?;
        let mut name = ent.file_name().to_string_lossy().into_owned();
        if ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            name.push('/');
        }
        names.push(name);
    }
    names.sort();
    if names.is_empty() {
        Ok("(empty directory)".into())
    } else {
        Ok(names.join("\n"))
    }
}

fn write_execute(workspace: &Path, raw: &Value) -> Result<String> {
    #[derive(Deserialize)]
    struct Args {
        path: String,
        content: String,
    }
    let a: Args = Args::deserialize(raw).context("write args")?;
    let path = resolve_path(workspace, &a.path)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    }
    fs::write(&path, &a.content).with_context(|| format!("write {}", path.display()))?;
    Ok(format!(
        "wrote {} bytes to {}",
        a.content.len(),
        path.display()
    ))
}

fn tool_edit() -> Tool {
    Tool {
        name: "edit",
        description: "Replace `old` with `new`. `old` must appear exactly once. Absolute paths and paths outside the workspace are allowed.",
        properties: &[
            Property {
                name: "path",
                r#type: "string",
                description: "File path",
                required: true,
            },
            Property {
                name: "old",
                r#type: "string",
                description: "Exact text to find",
                required: true,
            },
            Property {
                name: "new",
                r#type: "string",
                description: "Replacement",
                required: true,
            },
        ],
        execute: Some(edit_execute),
    }
}

fn edit_execute(workspace: &Path, raw: &Value) -> Result<String> {
    #[derive(Deserialize)]
    struct Args {
        path: String,
        old: String,
        new: String,
    }
    let a: Args = Args::deserialize(raw).context("edit args")?;
    if a.old.is_empty() {
        bail!("`old` must not be empty");
    }
    let path = resolve_path(workspace, &a.path)?;
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let n = text.matches(&a.old).count();
    if n != 1 {
        bail!("`old` must appear exactly once (found {n})");
    }
    fs::write(&path, text.replacen(&a.old, &a.new, 1))
        .with_context(|| format!("write {}", path.display()))?;
    Ok(format!("edited {}", path.display()))
}

pub(crate) fn get_tools() -> Vec<Tool> {
    vec![tool_read(), tool_write(), tool_edit(), tool_bash()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fun-tool-{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&dir);
        dir.canonicalize().unwrap_or(dir)
    }

    #[test]
    fn clip_utf8_keeps_char_boundary() {
        assert_eq!(clip_utf8("hello", 5), "hello");
        assert_eq!(clip_utf8("hello", 3), "hel");
        assert_eq!(clip_utf8("éé", 1), "");
        assert_eq!(clip_utf8("éé", 2), "é");
    }

    #[test]
    fn resolve_relative_and_outside() {
        let root = workspace();
        assert!(
            fs::write(root.join("a.txt"), "ok").is_ok(),
            "write fixture"
        );
        let resolved = resolve_path(&root, "a.txt");
        assert!(resolved.as_ref().is_ok_and(|p| p.ends_with("a.txt")));
        assert!(resolve_path(&root, "../secret").is_ok());
        assert!(resolve_path(&root, "").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_outside_workspace_and_directory() {
        let root = workspace();
        let outside = root.parent().unwrap().join(format!(
            "fun-tool-out-{}",
            uuid::Uuid::new_v4().simple()
        ));
        assert!(fs::create_dir_all(&outside).is_ok());
        assert!(fs::write(outside.join("secret.txt"), "peek").is_ok());
        let out = read_execute(
            &root,
            &json!({"path": outside.join("secret.txt").to_string_lossy()}),
        );
        assert!(out.as_ref().is_ok_and(|s| s.contains("peek")), "{out:?}");
        let listing = read_execute(
            &root,
            &json!({"path": outside.to_string_lossy()}),
        );
        assert!(
            listing.as_ref().is_ok_and(|s| s.contains("secret.txt")),
            "{listing:?}"
        );
        let wrote = write_execute(
            &root,
            &json!({"path": outside.join("nope.txt").to_string_lossy(), "content": "x"}),
        );
        assert!(wrote.is_ok(), "{wrote:?}");
        let edited = edit_execute(
            &root,
            &json!({
                "path": outside.join("secret.txt").to_string_lossy(),
                "old": "peek",
                "new": "seen"
            }),
        );
        assert!(edited.is_ok(), "{edited:?}");
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "seen"
        );
        let _ = fs::remove_dir_all(&outside);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_png_reports_size_instead_of_utf8_error() {
        let root = workspace();
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&1u32.to_be_bytes());
        png.extend_from_slice(&2u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0]);
        png.extend_from_slice(&[0; 4]);
        let path = root.join("t24.png");
        assert!(fs::write(&path, &png).is_ok(), "write png");
        let out = read_execute(&root, &json!({"path": "t24.png"}));
        assert!(
            out.as_ref()
                .is_ok_and(|s| s.contains("PNG 1x2") && s.contains("bytes")),
            "{out:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn write_edit_read_roundtrip() {
        let root = workspace();
        assert!(
            write_execute(&root, &json!({"path": "n.txt", "content": "hello world"})).is_ok(),
            "write"
        );
        assert!(
            edit_execute(
                &root,
                &json!({"path": "n.txt", "old": "world", "new": "there"}),
            )
            .is_ok(),
            "edit"
        );
        let out = read_execute(&root, &json!({"path": "n.txt"}));
        assert!(out.as_ref().is_ok_and(|s| s.contains("hello there")));
        assert!(edit_execute(
            &root,
            &json!({"path": "n.txt", "old": "missing", "new": "x"})
        )
        .is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_drops_dotdot_inside_root() {
        let p = clean(Path::new("/ws/a/../b/./c"));
        assert_eq!(p, PathBuf::from("/ws/b/c"));
    }
}
