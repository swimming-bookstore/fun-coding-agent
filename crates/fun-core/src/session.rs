use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub use crate::prune::{
    PruneTurn, PruneView, parse_drop_ids, prune_inspect, prune_inspect_ids, prune_inspect_note,
    prune_restored, record_turn, sanitize_hidden, should_prune,
};

pub fn empty_args() -> Value {
    Value::Object(serde_json::Map::default())
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Call {
    pub id: String,
    pub name: String,
    #[serde(default = "empty_args")]
    pub args: Value,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Entry {
    User {
        text: String,
    },
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
    Ok(xdg_data_home()?.join("fun"))
}

fn xdg_data_home() -> Result<PathBuf> {
    if let Ok(dir) = env::var("XDG_DATA_HOME")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let home = env::var("HOME").context("HOME is not set")?;
    if home.is_empty() {
        bail!("HOME is not set");
    }
    Ok(PathBuf::from(home).join(".local/share"))
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
    let name = format!("--{safe}-{}--", path_digest(&cwd));
    Ok(data_dir()?.join("sessions").join(name))
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
pub struct Usage {
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
        self.add_inner(input, output, cached, reasoning, true);
    }

    /// Count prune-turn tokens without treating that payload as the live context size.
    pub fn add_prune(&mut self, input: u64, output: u64, cached: u64, reasoning: u64) {
        self.add_inner(input, output, cached, reasoning, false);
    }

    fn add_inner(
        &mut self,
        input: u64,
        output: u64,
        cached: u64,
        reasoning: u64,
        set_last_input: bool,
    ) {
        if input > 0 {
            self.input_tokens += input;
            if set_last_input {
                self.last_input_tokens = input;
            }
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
struct DiskPrune {
    #[serde(rename = "type")]
    kind: String,
    hidden: Vec<usize>,
    /// Newly hidden this prune (0 on older files — recovered from the hidden-set delta).
    #[serde(default)]
    dropped: usize,
    /// Ids hidden this prune (empty on older files — recovered from the hidden-set delta).
    #[serde(default)]
    added: Vec<usize>,
    /// Ids still sent after this prune (empty on older files — recovered as complement of hidden).
    #[serde(default)]
    keep: Vec<usize>,
    /// Hidden write/edit ids currently restored as compact stubs.
    #[serde(default)]
    restored: Vec<usize>,
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

#[derive(Clone)]
pub struct Session {
    pub entries: Vec<Entry>,
    pub path: PathBuf,
    pub usage: Usage,
    pub hidden: BTreeSet<usize>,
    /// Hidden write/edit ids sent as compact stubs. Kept across later prunes.
    pub restored: BTreeSet<usize>,
    /// Drop notes: `(entries.len() when pruned, newly hidden ids)`.
    pub prune_notes: Vec<(usize, Vec<usize>)>,
    /// Per-turn keep/drop. The next send is last keep plus later messages, minus later drop.
    pub ledger: Vec<PruneTurn>,
}

#[derive(Clone)]
pub struct SessionInfo {
    pub path: PathBuf,
    pub title: String,
    pub n: usize,
    pub workspace: PathBuf,
}

#[derive(Clone)]
pub struct WorkspaceGroup {
    pub workspace: PathBuf,
    pub chats: Vec<SessionInfo>,
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
            hidden: BTreeSet::new(),
            restored: BTreeSet::new(),
            prune_notes: Vec::new(),
            ledger: Vec::new(),
        })
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read session {}", path.display()))?;
        let mut entries = Vec::new();
        let mut usage = Usage::default();
        let mut hidden = BTreeSet::new();
        let mut restored = BTreeSet::new();
        let mut prune_notes = Vec::new();
        let mut ledger = Vec::new();
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
                Some("compact") => {}
                Some("prune") => {
                    if let Ok(p) = serde_json::from_value::<DiskPrune>(v) {
                        let next: BTreeSet<usize> = p.hidden.into_iter().collect();
                        let added: Vec<usize> = if !p.added.is_empty() {
                            p.added
                        } else {
                            next.difference(&hidden).copied().collect()
                        };
                        if !added.is_empty() {
                            prune_notes.push((entries.len(), added.clone()));
                        }
                        restored = p.restored.into_iter().collect();
                        let keep = if p.keep.is_empty() {
                            (0..entries.len()).filter(|i| !next.contains(i)).collect()
                        } else {
                            p.keep
                        };
                        ledger.push(PruneTurn {
                            at: entries.len(),
                            keep,
                            drop: added,
                            restored: restored.iter().copied().collect(),
                        });
                        hidden = next;
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
        hidden.retain(|i| *i < entries.len());
        hidden = sanitize_hidden(&entries, &hidden);
        if restored.is_empty() {
            restored = prune_restored(&entries, &hidden).into_keys().collect();
        } else {
            restored.retain(|i| hidden.contains(i));
        }
        Ok(Self {
            entries,
            path,
            usage,
            hidden,
            restored,
            prune_notes,
            ledger,
        })
    }

    pub fn open_latest(workspace: &Path) -> Result<Option<Self>> {
        jsonl_files(&sessions_dir(workspace)?)
            .pop()
            .map(Self::load)
            .transpose()
    }

    pub fn latest_file(workspace: &Path) -> Result<Option<PathBuf>> {
        Ok(jsonl_files(&sessions_dir(workspace)?).pop())
    }

    pub fn summaries(workspace: &Path) -> Result<Vec<SessionInfo>> {
        let mut out = Vec::new();
        for path in jsonl_files(&sessions_dir(workspace)?).into_iter().rev() {
            let mut title = String::new();
            let mut n = 0usize;
            if let Ok(text) = fs::read_to_string(&path) {
                for line in text.lines() {
                    if let Ok(e) = serde_json::from_str::<Entry>(line) {
                        n += 1;
                        if let Entry::User { text } = e {
                            title = text.chars().take(72).collect();
                        }
                    }
                }
            }
            if title.is_empty() {
                title = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("session")
                    .into();
            }
            out.push(SessionInfo {
                path,
                title,
                n,
                workspace: workspace.to_path_buf(),
            });
        }
        Ok(out)
    }

    pub fn all_groups() -> Result<Vec<WorkspaceGroup>> {
        let root = data_dir()?.join("sessions");
        let mut groups = Vec::new();
        let Ok(rd) = fs::read_dir(&root) else {
            return Ok(groups);
        };
        let mut dirs: Vec<_> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let mut chats = Vec::new();
            let mut workspace = PathBuf::new();
            for path in jsonl_files(&dir).into_iter().rev() {
                let mut title = String::new();
                let mut n = 0usize;
                if let Ok(text) = fs::read_to_string(&path) {
                    for line in text.lines() {
                        if workspace.as_os_str().is_empty() {
                            if let Ok(h) = serde_json::from_str::<DiskHeader>(line)
                                && (h.kind == "header" || h.kind.is_empty())
                                && !h.cwd.is_empty()
                            {
                                workspace = PathBuf::from(&h.cwd);
                            }
                        }
                        if let Ok(e) = serde_json::from_str::<Entry>(line) {
                            n += 1;
                            if let Entry::User { text } = e {
                                title = text.chars().take(72).collect();
                            }
                        }
                    }
                }
                if title.trim().is_empty() {
                    title = "New chat".into();
                }
                chats.push(SessionInfo {
                    path,
                    title,
                    n,
                    workspace: workspace.clone(),
                });
            }
            if workspace.as_os_str().is_empty() {
                workspace = PathBuf::from(dir.file_name().unwrap_or_default());
            }
            if !chats.is_empty() {
                groups.push(WorkspaceGroup { workspace, chats });
            }
        }
        groups.sort_by(|a, b| {
            repo_name(&a.workspace)
                .to_lowercase()
                .cmp(&repo_name(&b.workspace).to_lowercase())
        });
        Ok(groups)
    }

    pub fn groups_for(workspaces: &[PathBuf]) -> Result<Vec<WorkspaceGroup>> {
        let all = Self::all_groups()?;
        let mut out = Vec::new();
        for ws in workspaces {
            let canon = ws.canonicalize().unwrap_or_else(|_| ws.clone());
            if let Some(mut g) = all
                .iter()
                .find(|g| {
                    g.workspace
                        .canonicalize()
                        .unwrap_or_else(|_| g.workspace.clone())
                        == canon
                        || g.workspace == *ws
                })
                .cloned()
            {
                g.workspace = ws.clone();
                out.push(g);
            } else {
                out.push(WorkspaceGroup {
                    workspace: ws.clone(),
                    chats: Vec::new(),
                });
            }
        }
        Ok(out)
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

    pub fn save_hidden(&mut self, hidden: BTreeSet<usize>) -> Result<(Vec<usize>, Vec<usize>)> {
        let hidden = sanitize_hidden(&self.entries, &hidden);
        let added: Vec<usize> = hidden.difference(&self.hidden).copied().collect();
        let dropped = added.len();
        let restored: BTreeSet<usize> =
            prune_restored(&self.entries, &hidden).into_keys().collect();
        let mut newly: Vec<usize> = restored.difference(&self.restored).copied().collect();
        newly.sort_unstable();
        append_json(
            &self.path,
            &DiskPrune {
                kind: "prune".into(),
                hidden: hidden.iter().copied().collect(),
                dropped,
                added: added.clone(),
                keep: (0..self.entries.len())
                    .filter(|i| !hidden.contains(i))
                    .collect(),
                restored: restored.iter().copied().collect(),
            },
        )?;
        if dropped > 0 {
            self.prune_notes.push((self.entries.len(), added.clone()));
        }
        self.ledger.push(record_turn(
            self.entries.len(),
            &hidden,
            added.clone(),
            prune_restored(&self.entries, &hidden),
        ));
        self.hidden = hidden;
        self.restored = restored;
        Ok((added, newly))
    }

    /// Entries sent to the model: last keep plus later messages, minus later drop.
    pub fn model_entries(&self) -> Vec<Entry> {
        crate::prune::model_entries(&self.entries, &self.ledger)
    }

    /// Live / Keep / Candidate / Hidden, with previous keep/drop for the prune model.
    pub fn prune_view(&self) -> PruneView<'_> {
        PruneView::with_ledger(&self.entries, &self.hidden, &self.ledger)
    }

    pub fn should_prune(&self) -> Option<usize> {
        should_prune(&self.entries, &self.hidden, self.usage.last_input_tokens)
    }
}

fn repo_name(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.to_str().unwrap_or("folder"))
        .to_string()
}

pub fn list_sessions(workspace: &Path) -> Result<()> {
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
            hidden: BTreeSet::new(),
            restored: BTreeSet::new(),
            prune_notes: Vec::new(),
            ledger: Vec::new(),
        };
        assert!(s.add(Entry::User { text: "hi".into() }).is_ok(), "add user");
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
        let path = std::env::temp_dir().join(format!(
            "fun-missing-{}.jsonl",
            uuid::Uuid::new_v4().simple()
        ));
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
        u.add_prune(40_000, 20, 0, 0);
        assert_eq!(u.input_tokens, 40_020);
        assert_eq!(u.last_input_tokens, 8);
    }

    fn user(s: &str) -> Entry {
        Entry::User { text: s.into() }
    }

    fn assistant_call(id: &str, name: &str) -> Entry {
        Entry::Assistant {
            text: String::new(),
            thinking: String::new(),
            calls: vec![Call {
                id: id.into(),
                name: name.into(),
                args: empty_args(),
            }],
        }
    }

    fn tool_named(id: &str, name: &str, content: &str) -> Entry {
        Entry::Tool(ToolResult {
            id: id.into(),
            name: name.into(),
            content: content.into(),
            is_error: false,
        })
    }

    fn assistant_write(id: &str, path: &str, content: &str) -> Entry {
        Entry::Assistant {
            text: String::new(),
            thinking: String::new(),
            calls: vec![Call {
                id: id.into(),
                name: "write".into(),
                args: serde_json::json!({"path": path, "content": content}),
            }],
        }
    }

    fn tool(id: &str, content: &str) -> Entry {
        Entry::Tool(ToolResult {
            id: id.into(),
            name: "read".into(),
            content: content.into(),
            is_error: false,
        })
    }

    #[test]
    fn save_and_reload_hidden() {
        let dir = tmp();
        let path = dir.join("c.jsonl");
        let mut s = Session {
            entries: Vec::new(),
            path: path.clone(),
            usage: Usage::default(),
            hidden: BTreeSet::new(),
            restored: BTreeSet::new(),
            prune_notes: Vec::new(),
            ledger: Vec::new(),
        };
        assert!(s.add(user("one")).is_ok());
        assert!(s.add(assistant_call("c1", "bash")).is_ok());
        assert!(s.add(tool("c1", "noise")).is_ok());
        assert!(s.save_hidden(BTreeSet::from([1, 2])).is_ok());
        let loaded = Session::load(path);
        assert!(loaded.is_ok());
        if let Ok(loaded) = loaded {
            assert_eq!(loaded.entries.len(), 3);
            assert!(loaded.hidden.contains(&1));
            assert!(loaded.hidden.contains(&2));
            assert!(!loaded.hidden.contains(&0));
            assert_eq!(loaded.prune_notes, vec![(3, vec![1, 2])]);
            assert!(loaded.restored.is_empty());
            assert_eq!(loaded.ledger.len(), 1);
            assert_eq!(loaded.ledger[0].drop, vec![1, 2]);
            assert_eq!(loaded.ledger[0].keep, vec![0]);
            assert_eq!(loaded.ledger[0].at, 3);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_persists_across_later_prunes() {
        let dir = tmp();
        let path = dir.join("r.jsonl");
        let mut s = Session {
            entries: Vec::new(),
            path: path.clone(),
            usage: Usage::default(),
            hidden: BTreeSet::new(),
            restored: BTreeSet::new(),
            prune_notes: Vec::new(),
            ledger: Vec::new(),
        };
        assert!(s.add(user("make okta")).is_ok());
        assert!(s.add(assistant_write("w1", "src/login.rs", "okta")).is_ok());
        assert!(s.add(tool_named("w1", "write", "wrote okta")).is_ok());
        assert!(s.add(assistant_call("c1", "bash")).is_ok());
        assert!(s.add(tool("c1", "noise")).is_ok());
        assert!(s.add(user("open")).is_ok());
        let (added, newly) = s
            .save_hidden(BTreeSet::from([1, 2, 3, 4]))
            .expect("prune 1");
        assert_eq!(added, vec![1, 2, 3, 4]);
        assert_eq!(newly, vec![1, 2]);
        assert_eq!(s.restored, BTreeSet::from([1, 2]));
        assert!(s.add(assistant_call("c2", "bash")).is_ok());
        assert!(s.add(tool("c2", "more noise")).is_ok());
        assert!(s.add(user("open again")).is_ok());
        let (added, newly) = s
            .save_hidden(BTreeSet::from([1, 2, 3, 4, 6, 7]))
            .expect("prune 2");
        assert_eq!(added, vec![6, 7]);
        assert!(newly.is_empty(), "already-restored writes stay restored");
        assert_eq!(s.restored, BTreeSet::from([1, 2]));
        let out = s.model_entries();
        assert!(out.iter().any(|e| matches!(e, Entry::Assistant { calls, .. } if calls.iter().any(|c| c.name == "write"))));
        assert!(!out.iter().any(|e| matches!(e, Entry::Assistant { calls, .. } if calls.iter().any(|c| c.name == "bash"))));
        let loaded = Session::load(path).expect("reload");
        assert_eq!(loaded.restored, BTreeSet::from([1, 2]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_recovers_prune_note_without_dropped_field() {
        let dir = tmp();
        let path = dir.join("old-prune.jsonl");
        let header = r#"{"type":"header","version":1,"id":"x","cwd":"/tmp","createdAt":1}"#;
        let user = r#"{"kind":"user","text":"hi"}"#;
        let asst = r#"{"kind":"assistant","text":"yo","thinking":"","calls":[]}"#;
        let prune = r#"{"type":"prune","hidden":[0]}"#;
        fs::write(&path, format!("{header}\n{user}\n{asst}\n{prune}\n")).unwrap();
        let loaded = Session::load(path).expect("load");
        assert_eq!(loaded.prune_notes, vec![(2, vec![0])]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_keeps_each_prune_added_ids() {
        let dir = tmp();
        let path = dir.join("two-prune.jsonl");
        let header = r#"{"type":"header","version":1,"id":"x","cwd":"/tmp","createdAt":1}"#;
        let a = r#"{"kind":"user","text":"a"}"#;
        let b = r#"{"kind":"assistant","text":"b","thinking":"","calls":[]}"#;
        let c = r#"{"kind":"user","text":"c"}"#;
        let p1 = r#"{"type":"prune","hidden":[0,1],"dropped":2,"added":[0,1]}"#;
        let p2 = r#"{"type":"prune","hidden":[0,1,2],"dropped":1,"added":[2]}"#;
        fs::write(&path, format!("{header}\n{a}\n{b}\n{p1}\n{c}\n{p2}\n")).unwrap();
        let loaded = Session::load(path).expect("load");
        assert_eq!(loaded.prune_notes, vec![(2, vec![0, 1]), (3, vec![2])]);
        assert_eq!(
            prune_inspect_ids(&loaded.entries, loaded.prune_notes[0].1.iter().copied()).len(),
            2
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
