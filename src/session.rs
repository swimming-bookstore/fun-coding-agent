use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn empty_args() -> Value {
    Value::Object(serde_json::Map::default())
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Call {
    pub id: String,
    pub name: String,
    #[serde(default = "empty_args")]
    pub args: Value,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ToolResult {
    pub id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum Entry {
    User { text: String },
    Assistant {
        text: String,
        #[serde(default)]
        thinking: String,
        calls: Vec<Call>,
    },
    Tool(ToolResult),
}

fn hex_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn data_dir() -> Result<PathBuf> {
    if let Ok(dir) = env::var("XDG_DATA_HOME")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir).join("fun-coding-agent"));
    }
    let home = env::var("HOME").context("HOME is not set")?;
    if home.is_empty() {
        bail!("HOME is not set");
    }
    Ok(PathBuf::from(home).join(".local/share/fun-coding-agent"))
}

fn path_digest(cwd: &str) -> String {
    let mut h = 0u64;
    for b in cwd.bytes() {
        h = h.wrapping_mul(16_777_619) ^ u64::from(b);
    }
    format!("{:08x}", h as u32)
}

fn sessions_dir(workspace: &Path) -> Result<PathBuf> {
    let cwd = workspace.to_string_lossy();
    let safe: String = cwd
        .trim_start_matches(['/', '\\'])
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') || c.is_whitespace() {
                '-'
            } else {
                c
            }
        })
        .collect();
    Ok(data_dir()?
        .join("sessions")
        .join(format!("--{safe}-{}--", path_digest(&cwd))))
}

fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
                files.push(p);
            }
        }
    }
    files.sort_by_key(|p| {
        p.metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    files
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub last_input_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, input: u64, output: u64, cached: u64, reasoning: u64) {
        if input > 0 {
            self.input_tokens += input;
            self.last_input_tokens = input;
        }
        if output > 0 {
            self.output_tokens += output;
        }
        if cached > 0 {
            self.cached_tokens += cached;
        }
        if reasoning > 0 {
            self.reasoning_tokens += reasoning;
        }
    }
}

#[derive(Serialize, Deserialize)]
struct DiskUsage {
    #[serde(rename = "type")]
    kind: String,
    #[serde(flatten)]
    usage: Usage,
}

#[derive(Serialize, Deserialize)]
struct DiskHeader {
    #[serde(rename = "type")]
    kind: String,
    version: u32,
    id: String,
    cwd: String,
    #[serde(rename = "createdAt")]
    created_at: u64,
}

#[derive(Deserialize)]
struct OldEntry {
    #[serde(rename = "type")]
    kind: String,
    role: String,
    content: Value,
}

fn entry_from_old(role: &str, content: Value) -> Option<Entry> {
    match role {
        "user" => Some(Entry::User {
            text: content.as_str().unwrap_or("").into(),
        }),
        "assistant" => Some(Entry::Assistant {
            text: content
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
            thinking: content
                .get("thinking")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
            calls: content
                .get("calls")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
        }),
        "tool_result" => serde_json::from_value(content).ok().map(Entry::Tool),
        _ => None,
    }
}

fn append_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut line = serde_json::to_string(value).context("encode session")?;
    line.push('\n');
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("session dir")?;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .with_context(|| format!("write session {}", path.display()))
}

pub(crate) struct Session {
    pub entries: Vec<Entry>,
    pub path: PathBuf,
    pub usage: Usage,
}

impl Session {
    pub fn create(workspace: &Path) -> Result<Self> {
        let dir = sessions_dir(workspace)?;
        fs::create_dir_all(&dir).context("create session dir")?;
        let id = hex_id();
        let path = dir.join(format!("{}_{}.jsonl", now_ms(), &id[..8]));
        let header = DiskHeader {
            kind: "header".into(),
            version: 1,
            id,
            cwd: workspace.display().to_string(),
            created_at: now_ms(),
        };
        fs::write(
            &path,
            serde_json::to_string(&header).context("encode session header")? + "\n",
        )
        .with_context(|| format!("write session {}", path.display()))?;
        Ok(Self {
            entries: Vec::new(),
            path,
            usage: Usage::default(),
        })
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read session {}", path.display()))?;
        let mut entries = Vec::new();
        let mut usage = Usage::default();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("header") => {}
                Some("usage") => {
                    if let Ok(u) = serde_json::from_value::<DiskUsage>(v) {
                        usage = u.usage;
                    }
                }
                Some("entry") => {
                    if let Ok(rec) = serde_json::from_value::<OldEntry>(v)
                        && rec.kind == "entry"
                        && let Some(e) = entry_from_old(&rec.role, rec.content)
                    {
                        entries.push(e);
                    }
                }
                _ => {
                    if let Ok(e) = serde_json::from_value::<Entry>(v) {
                        entries.push(e);
                    }
                }
            }
        }
        Ok(Self {
            entries,
            path,
            usage,
        })
    }

    pub fn open_latest(workspace: &Path) -> Result<Option<Self>> {
        jsonl_files(&sessions_dir(workspace)?)
            .pop()
            .map(Self::load)
            .transpose()
    }

    pub fn add(&mut self, e: Entry) -> Result<()> {
        append_json(&self.path, &e)?;
        self.entries.push(e);
        Ok(())
    }

    pub fn save_usage(&mut self, usage: Usage) -> Result<()> {
        self.usage = usage;
        append_json(
            &self.path,
            &DiskUsage {
                kind: "usage".into(),
                usage,
            },
        )
    }
}

pub(crate) fn list_sessions(workspace: &Path) -> Result<()> {
    let files = jsonl_files(&sessions_dir(workspace)?);
    if files.is_empty() {
        println!("no sessions yet");
        return Ok(());
    }
    for path in files.iter().rev() {
        let mut created = String::new();
        let mut n = 0usize;
        let mut preview = String::new();
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if let Ok(h) = serde_json::from_str::<DiskHeader>(line)
                    && h.kind == "header"
                {
                    created = h.created_at.to_string();
                }
                if let Ok(e) = serde_json::from_str::<Entry>(line) {
                    n += 1;
                    if preview.is_empty()
                        && let Entry::User { text } = e
                    {
                        preview = text.chars().take(60).collect();
                    }
                } else if let Ok(e) = serde_json::from_str::<OldEntry>(line)
                    && e.kind == "entry"
                {
                    n += 1;
                    if preview.is_empty() && e.role == "user" {
                        preview = e.content.as_str().unwrap_or("").chars().take(60).collect();
                    }
                }
            }
        }
        println!(
            "{}  {created}  {n} entries  {preview:?}",
            path.file_stem().and_then(|s| s.to_str()).unwrap_or("?")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fun-sess-{}", uuid::Uuid::new_v4().simple()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn add_and_reload_entries() {
        let dir = tmp();
        let path = dir.join("s.jsonl");
        let mut s = Session {
            entries: Vec::new(),
            path: path.clone(),
            usage: Usage::default(),
        };
        assert!(
            s.add(Entry::User { text: "hi".into() }).is_ok(),
            "add user"
        );
        assert!(
            s.add(Entry::Assistant {
                text: "yo".into(),
                thinking: String::new(),
                calls: Vec::new(),
            })
            .is_ok(),
            "add assistant"
        );
        let loaded = Session::load(path);
        assert!(loaded.is_ok(), "reload");
        if let Ok(loaded) = loaded {
            assert_eq!(loaded.entries.len(), 2);
            assert!(
                matches!(&loaded.entries[0], Entry::User { text } if text == "hi"),
                "expected user"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_old_entry_shape() {
        let dir = tmp();
        let path = dir.join("old.jsonl");
        assert!(
            fs::write(
                &path,
                r#"{"type":"header","version":1,"id":"x","cwd":"/tmp","createdAt":1}
{"type":"entry","id":"1","parentId":null,"role":"user","content":"hello"}
{"type":"usage","input_tokens":10,"output_tokens":2,"cached_tokens":0,"reasoning_tokens":0,"last_input_tokens":10}
"#,
            )
            .is_ok(),
            "write old session"
        );
        let s = Session::load(path);
        assert!(s.is_ok(), "load old");
        if let Ok(s) = s {
            assert_eq!(s.entries.len(), 1);
            assert_eq!(s.usage.input_tokens, 10);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_errors() {
        let path = std::env::temp_dir().join(format!("fun-missing-{}.jsonl", uuid::Uuid::new_v4().simple()));
        assert!(Session::load(path).is_err());
    }

    #[test]
    fn usage_add_sets_last_input() {
        let mut u = Usage::default();
        u.add(12, 3, 4, 5);
        assert_eq!(u.input_tokens, 12);
        assert_eq!(u.last_input_tokens, 12);
        u.add(8, 1, 0, 0);
        assert_eq!(u.input_tokens, 20);
        assert_eq!(u.last_input_tokens, 8);
    }
}
