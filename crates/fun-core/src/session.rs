use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::time::{SystemTime, UNIX_EPOCH};

pub use crate::prune::{
    PruneTurn, PruneView, parse_drop_ids, prune_inspect, prune_inspect_ids, prune_inspect_note,
    prune_restored, record_turn, sanitize_hidden, should_prune,
};

/// Largest image Fun will attach to a user message (decoded bytes).
pub const MAX_IMAGE_BYTES: usize = 7 * 1024 * 1024;
/// Largest PDF or plain-text file Fun will attach (decoded bytes).
pub const MAX_DOCUMENT_BYTES: usize = 50 * 1024 * 1024;
/// Largest audio or video file Fun will attach (decoded bytes).
pub const MAX_MEDIA_BYTES: usize = 20 * 1024 * 1024;

pub fn empty_args() -> Value {
    Value::Object(serde_json::Map::default())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Call {
    pub id: String,
    pub name: String,
    #[serde(default = "empty_args")]
    pub args: Value,
    /// Gemini thought signature. Required on later turns when the model used tools.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thought_signature: String,
    /// Prune put this write/edit back as a path-only note. Not a call to replay.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restored: bool,
}

impl Call {
    /// Context-prune reminder. Successful writes/edits are omitted on purpose:
    /// a path stub is not proof the file still exists, and saying "applied" makes
    /// a later review keep or delete work it has not read. Only a failed or
    /// missing result is worth a note, so the next turn does not treat a miss
    /// as already on disk.
    pub fn restored_note(calls: &[&Call], results: &[ToolResult]) -> String {
        let mut failed = Vec::new();
        let mut unknown = Vec::new();
        for c in calls {
            let label = c
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|p| format!("{} {p}", c.name))
                .unwrap_or_else(|| c.name.clone());
            match tool_outcome(c, results) {
                ToolOutcome::Ok => {}
                ToolOutcome::Failed => failed.push(label),
                ToolOutcome::Unknown => {
                    if let Some(err) = c
                        .args
                        .get("error")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                    {
                        failed.push(format!("{label} ({err})"));
                    } else {
                        unknown.push(label);
                    }
                }
            }
        }
        let mut parts = Vec::new();
        if !failed.is_empty() {
            parts.push(format!(
                "{} failed — not on disk; read the file and retry",
                failed.join(", ")
            ));
        }
        if !unknown.is_empty() {
            parts.push(format!(
                "{} outcome unknown — read the file before repeating",
                unknown.join(", ")
            ));
        }
        if parts.is_empty() {
            return String::new();
        }
        format!(
            "(earlier tool call compacted from context: {}.)",
            parts.join("; ")
        )
    }
}

enum ToolOutcome {
    Ok,
    Failed,
    Unknown,
}

fn tool_outcome(c: &Call, results: &[ToolResult]) -> ToolOutcome {
    if c.id.is_empty() {
        return ToolOutcome::Unknown;
    }
    match results.iter().find(|t| t.id == c.id && t.name == c.name) {
        Some(t) if t.is_error => ToolOutcome::Failed,
        Some(_) => ToolOutcome::Ok,
        None => ToolOutcome::Unknown,
    }
}

impl Default for Call {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            args: empty_args(),
            thought_signature: String::new(),
            restored: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
    /// Compact prune stub, not a functionResponse the model is waiting on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restored: bool,
}

/// One file attached to a user message. Bytes are base64 on disk.
/// Older sessions stored pictures only; `kind` defaults to `image`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Image {
    /// Short type: `png`, `pdf`, `mp3`, `mp4`, …
    pub media: String,
    pub data: String,
    /// Basename shown in the transcript. Empty on older files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// `image`, `audio`, `video`, `pdf`, or `text`.
    #[serde(default = "default_media_kind", skip_serializing_if = "is_image_kind")]
    pub kind: String,
}

fn default_media_kind() -> String {
    "image".into()
}

fn is_image_kind(kind: &str) -> bool {
    kind.is_empty() || kind == "image"
}

impl Image {
    pub fn kind(&self) -> &str {
        if self.kind.is_empty() {
            "image"
        } else {
            &self.kind
        }
    }

    pub fn media_type(&self) -> String {
        match self.kind() {
            "audio" => match self.media.as_str() {
                "aac" => "audio/x-aac".into(),
                "mp3" => "audio/mp3".into(),
                "m4a" | "mp4" => "audio/mp4".into(),
                "mpeg" | "mpga" => "audio/mpeg".into(),
                "ogg" => "audio/ogg".into(),
                "pcm" => "audio/pcm".into(),
                "wav" => "audio/wav".into(),
                "webm" => "audio/webm".into(),
                "flac" => "audio/flac".into(),
                other => format!("audio/{other}"),
            },
            "video" => match self.media.as_str() {
                "mov" => "video/quicktime".into(),
                "mpeg" | "mpg" | "mpegs" => "video/mpeg".into(),
                "flv" => "video/x-flv".into(),
                "wmv" => "video/wmv".into(),
                "3gp" | "3gpp" => "video/3gpp".into(),
                "webm" => "video/webm".into(),
                "mp4" => "video/mp4".into(),
                other => format!("video/{other}"),
            },
            "pdf" => "application/pdf".into(),
            "text" => "text/plain".into(),
            "image" | "" => match self.media.as_str() {
                "jpg" | "jpeg" => "image/jpeg".into(),
                "png" => "image/png".into(),
                "gif" => "image/gif".into(),
                "webp" => "image/webp".into(),
                "heic" => "image/heic".into(),
                "heif" => "image/heif".into(),
                other => format!("image/{other}"),
            },
            "heic" => "image/heic".into(),
            "heif" => "image/heif".into(),
            other => format!("image/{other}"),
        }
    }

    pub fn chip_label(&self) -> String {
        let name = if self.name.is_empty() {
            self.media.as_str()
        } else {
            self.name.as_str()
        };
        format!("{} {name}", self.kind())
    }

    pub fn decode_utf8(&self) -> Option<String> {
        let bytes = base64_decode(&self.data)?;
        String::from_utf8(bytes).ok()
    }
}

pub fn user_display(text: &str, images: &[Image]) -> String {
    let mut out = text.trim().to_string();
    for image in images {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push('[');
        out.push_str(&image.chip_label());
        out.push(']');
    }
    out
}

/// Display name for a transcript chip. Path basename, or the kind.
pub fn image_label(path: &str) -> String {
    media_label(path, "image")
}

pub fn media_label(path: &str, kind: &str) -> String {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(kind);
    format!("{kind} {name}")
}

fn image_media(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpeg"),
        "gif" => Some("gif"),
        "webp" => Some("webp"),
        _ => None,
    }
}

fn sniff_media(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

/// Load a local image for a user message. Rejects unknown types and files over 4MB.
pub fn load_image(path: &Path) -> Result<Image> {
    let bytes = fs::read(path).with_context(|| format!("read image {}", path.display()))?;
    if bytes.is_empty() {
        bail!("empty image {}", path.display());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        bail!(
            "image {} is {} bytes (max {MAX_IMAGE_BYTES})",
            path.display(),
            bytes.len()
        );
    }
    let hinted = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(image_media);
    let media = sniff_media(&bytes).or(hinted).ok_or_else(|| {
        anyhow::anyhow!("not a png, jpeg, gif, or webp image: {}", path.display())
    })?;
    Ok(Image {
        media: media.into(),
        data: base64_encode(&bytes),
        name: String::new(),
        kind: "image".into(),
    })
}

/// Load a local file Gemini 4 Argon can take: image, audio, video, PDF, or plain text.
pub fn load_media(path: &Path) -> Result<Image> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.is_empty() {
        bail!("empty file {}", path.display());
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (kind, media) = classify_media(&ext, &bytes).ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported file {} — want image, audio, video, pdf, or txt",
            path.display()
        )
    })?;
    let max = match kind {
        "image" => MAX_IMAGE_BYTES,
        "pdf" | "text" => MAX_DOCUMENT_BYTES,
        _ => MAX_MEDIA_BYTES,
    };
    if bytes.len() > max {
        bail!(
            "{} {} is {} bytes (max {max})",
            kind,
            path.display(),
            bytes.len()
        );
    }
    Ok(Image {
        media: media.into(),
        data: base64_encode(&bytes),
        name: String::new(),
        kind: kind.into(),
    })
}

fn classify_media(ext: &str, bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if let Some(media) = sniff_media(bytes) {
        return Some(("image", media));
    }
    if bytes.starts_with(b"%PDF-") {
        return Some(("pdf", "pdf"));
    }
    match ext {
        "png" => Some(("image", "png")),
        "jpg" | "jpeg" => Some(("image", "jpeg")),
        "gif" => Some(("image", "gif")),
        "webp" => Some(("image", "webp")),
        "heic" => Some(("image", "heic")),
        "heif" => Some(("image", "heif")),
        "pdf" => Some(("pdf", "pdf")),
        "txt" | "text" | "md" => Some(("text", "txt")),
        "mp3" => Some(("audio", "mp3")),
        "wav" => Some(("audio", "wav")),
        "flac" => Some(("audio", "flac")),
        "aac" => Some(("audio", "aac")),
        "ogg" => Some(("audio", "ogg")),
        "m4a" => Some(("audio", "m4a")),
        "mpga" => Some(("audio", "mpga")),
        "pcm" => Some(("audio", "pcm")),
        "mp4" => Some(("video", "mp4")),
        "mov" => Some(("video", "mov")),
        "webm" => Some(("video", "webm")),
        "mpeg" | "mpg" | "mpegs" => Some(("video", "mpeg")),
        "flv" => Some(("video", "flv")),
        "wmv" => Some(("video", "wmv")),
        "3gp" | "3gpp" => Some(("video", "3gpp")),
        _ => None,
    }
}

/// True when a dropped path is a supported attachment.
pub fn looks_like_media_path(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    classify_media(&ext, &[]).is_some()
}

fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n =
            (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8) | u32::from(bytes[i + 2]);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    match bytes.len() - i {
        1 => {
            let n = u32::from(bytes[i]) << 16;
            out.push(T[((n >> 18) & 63) as usize] as char);
            out.push(T[((n >> 12) & 63) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8);
            out.push(T[((n >> 18) & 63) as usize] as char);
            out.push(T[((n >> 12) & 63) as usize] as char);
            out.push(T[((n >> 6) & 63) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

pub(crate) fn base64_decode(data: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = data.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let (a, b, c, d) = (bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]);
        let av = val(a)?;
        let bv = val(b)?;
        out.push((av << 2) | (bv >> 4));
        if c != b'=' {
            let cv = val(c)?;
            out.push((bv << 4) | (cv >> 2));
            if d != b'=' {
                let dv = val(d)?;
                out.push((cv << 6) | dv);
            }
        }
        i += 4;
    }
    Some(out)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Entry {
    User {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
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
        }
        let prompt = input.saturating_add(cached);
        if set_last_input && prompt > 0 {
            self.last_input_tokens = prompt;
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
            images: Vec::new(),
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
    let mut opts = fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    opts.mode(0o600);
    opts.open(path)
        .and_then(|mut f| {
            restrict_private(&f)?;
            f.write_all(line.as_bytes())
        })
        .with_context(|| format!("write session {}", path.display()))
}

/// `OpenOptions::mode` only applies when the file is created. Restrict an
/// existing session too, so a file left at 0644 by an older build is not
/// left readable after the next append.
fn restrict_private(file: &fs::File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = file;
    }
    Ok(())
}

fn write_header(path: &Path, workspace: &Path, id: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("create session dir")?;
    }
    let header = DiskHeader {
        kind: "header".into(),
        version: 1,
        id: id.to_string(),
        cwd: workspace.display().to_string(),
        created_at: now_ms(),
    };
    let body = serde_json::to_string(&header).context("encode session header")? + "\n";
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    opts.open(path)
        .and_then(|mut f| {
            restrict_private(&f)?;
            f.write_all(body.as_bytes())
        })
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
    /// Workspace for a session that has not been written yet.
    pending_cwd: Option<PathBuf>,
    /// Workspace this session belongs to, kept after the header is written.
    workspace: PathBuf,
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
    /// New chat. The file is written on the first entry, not at startup.
    pub fn create(workspace: &Path) -> Result<Self> {
        let dir = sessions_dir(workspace)?;
        let id = hex_id();
        let path = dir.join(format!("{}_{}.jsonl", now_ms(), &id[..8]));
        Ok(Self {
            entries: Vec::new(),
            path,
            usage: Usage::default(),
            hidden: BTreeSet::new(),
            restored: BTreeSet::new(),
            prune_notes: Vec::new(),
            ledger: Vec::new(),
            pending_cwd: Some(workspace.to_path_buf()),
            workspace: workspace.to_path_buf(),
        })
    }

    fn ensure_file(&mut self) -> Result<()> {
        let Some(cwd) = self.pending_cwd.take() else {
            return Ok(());
        };
        let id = self
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.split('_').nth(1))
            .unwrap_or("session");
        if let Err(err) = write_header(&self.path, &cwd, id) {
            self.pending_cwd = Some(cwd);
            return Err(err);
        }
        Ok(())
    }

    /// True when nothing was said. A header-only file still counts as empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Start a new chat in the same workspace. The current file is already on
    /// disk once it has entries, so this only swaps the in-memory transcript.
    /// An empty current session is discarded instead of leaving a blank file.
    pub fn fork(&mut self) -> Result<PathBuf> {
        let previous = self.path.clone();
        self.discard_if_empty()?;
        let fresh = Self::create(&self.workspace)?;
        *self = fresh;
        Ok(previous)
    }

    /// Drop a session that never got a message, including one created before this rule.
    ///
    /// A missing file is already discarded. Only a non-empty transcript is kept.
    pub fn discard_if_empty(&self) -> Result<bool> {
        if !self.is_empty() {
            return Ok(false);
        }
        if !self.path.exists() {
            return Ok(true);
        }
        let text = fs::read_to_string(&self.path)
            .with_context(|| format!("read session {}", self.path.display()))?;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                return Ok(false);
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("header") | None => {}
                _ => return Ok(false),
            }
            if v.get("role").is_some() || v.get("text").is_some() {
                return Ok(false);
            }
        }
        fs::remove_file(&self.path)
            .with_context(|| format!("remove empty session {}", self.path.display()))?;
        Ok(true)
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
        let mut workspace = PathBuf::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("header") => {
                    if let Ok(h) = serde_json::from_value::<DiskHeader>(v)
                        && !h.cwd.is_empty()
                    {
                        workspace = PathBuf::from(h.cwd);
                    }
                }
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
            pending_cwd: None,
            workspace,
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
                        if let Entry::User { text, images } = e {
                            title = user_preview(&text, &images);
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
                        if workspace.as_os_str().is_empty()
                            && let Ok(h) = serde_json::from_str::<DiskHeader>(line)
                            && (h.kind == "header" || h.kind.is_empty())
                            && !h.cwd.is_empty()
                        {
                            workspace = PathBuf::from(&h.cwd);
                        }
                        if let Ok(e) = serde_json::from_str::<Entry>(line) {
                            n += 1;
                            if let Entry::User { text, images } = e {
                                title = user_preview(&text, &images);
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
        self.ensure_file()?;
        append_json(&self.path, &e)?;
        self.entries.push(e);
        Ok(())
    }

    pub fn save_usage(&mut self, usage: Usage) -> Result<()> {
        self.usage = usage;
        if self.pending_cwd.is_some() {
            return Ok(());
        }
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
        if self.pending_cwd.is_none() {
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
        }
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

fn user_preview(text: &str, images: &[Image]) -> String {
    let text = text.trim();
    if !text.is_empty() {
        return text.chars().take(72).collect();
    }
    match images.len() {
        0 => String::new(),
        1 => "image".into(),
        n => format!("{n} images"),
    }
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
                        && let Entry::User { text, images } = e
                    {
                        preview = user_preview(&text, &images).chars().take(60).collect();
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
    fn restored_note_does_not_claim_a_failed_edit_applied() {
        let ok = Call {
            id: "1".into(),
            name: "edit".into(),
            args: serde_json::json!({"path": "/tmp/ok.rs"}),
            thought_signature: String::new(),
            restored: true,
        };
        let bad = Call {
            id: "2".into(),
            name: "edit".into(),
            args: serde_json::json!({"path": "/tmp/missing.rs"}),
            thought_signature: String::new(),
            restored: true,
        };
        let unknown = Call {
            id: "3".into(),
            name: "write".into(),
            args: serde_json::json!({"path": "/tmp/gone.rs"}),
            thought_signature: String::new(),
            restored: true,
        };
        let results = vec![
            ToolResult {
                id: "1".into(),
                name: "edit".into(),
                content: "applied".into(),
                is_error: false,
                restored: true,
            },
            ToolResult {
                id: "2".into(),
                name: "edit".into(),
                content: "old string not found".into(),
                is_error: true,
                restored: true,
            },
        ];
        let note = Call::restored_note(&[&ok, &bad, &unknown], &results);
        assert!(!note.contains("/tmp/ok.rs"));
        assert!(!note.contains("applied"));
        assert!(!note.contains("do not repeat"));
        assert!(note.contains("edit /tmp/missing.rs failed — not on disk; read the file and retry"));
        assert!(note.contains("write /tmp/gone.rs outcome unknown — read the file before repeating"));
        assert!(!note.contains("Do not repeat a call that applied"));
        assert!(!note.contains("already applied"));
        assert!(!note.contains("file is on disk"));
        let buried = Call {
            id: "4".into(),
            name: "edit".into(),
            args: serde_json::json!({"path": "/tmp/ui.rs", "error": "`old` must appear exactly once"}),
            thought_signature: String::new(),
            restored: true,
        };
        let buried_note = Call::restored_note(&[&buried], &[]);
        assert!(buried_note.contains("edit /tmp/ui.rs (`old` must appear exactly once) failed — not on disk"));
        assert!(!buried_note.contains("do not repeat"));
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
            pending_cwd: None,
            workspace: PathBuf::new(),
        };
        assert!(
            s.add(Entry::User {
                text: "hi".into(),
                images: Vec::new(),
            })
            .is_ok(),
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
                matches!(&loaded.entries[0], Entry::User { text, images } if text == "hi" && images.is_empty()),
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
        assert_eq!(u.cached_tokens, 4);
        assert_eq!(u.last_input_tokens, 16);
        u.add(8, 1, 0, 0);
        assert_eq!(u.input_tokens, 20);
        assert_eq!(u.last_input_tokens, 8);
        u.add(0, 1, 10, 0);
        assert_eq!(u.input_tokens, 20);
        assert_eq!(u.cached_tokens, 14);
        assert_eq!(u.last_input_tokens, 10);
        u.add_prune(40_000, 20, 0, 0);
        assert_eq!(u.input_tokens, 40_020);
        assert_eq!(u.last_input_tokens, 10);
    }

    fn user(s: &str) -> Entry {
        Entry::User {
            text: s.into(),
            images: Vec::new(),
        }
    }

    #[test]
    fn user_image_roundtrip_and_load() {
        let dir = tmp();
        let png = dir.join("dot.png");
        fs::write(
            &png,
            [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x01],
        )
        .unwrap();
        let image = load_image(&png).expect("png");
        assert_eq!(image.media, "png");
        assert_eq!(image.kind(), "image");
        assert_eq!(
            user_display("see", std::slice::from_ref(&image)),
            "see\n[image png]"
        );
        let path = dir.join("s.jsonl");
        let mut s = Session {
            entries: Vec::new(),
            path: path.clone(),
            usage: Usage::default(),
            hidden: BTreeSet::new(),
            restored: BTreeSet::new(),
            prune_notes: Vec::new(),
            ledger: Vec::new(),
            pending_cwd: None,
            workspace: PathBuf::new(),
        };
        s.add(Entry::User {
            text: "see this".into(),
            images: vec![image.clone()],
        })
        .unwrap();
        let loaded = Session::load(path).unwrap();
        match &loaded.entries[0] {
            Entry::User { text, images } => {
                assert_eq!(text, "see this");
                assert_eq!(images, &vec![image]);
            }
            other => panic!("expected user, got {other:?}"),
        }
        let old = r#"{"kind":"user","text":"plain"}"#;
        let parsed: Entry = serde_json::from_str(old).unwrap();
        assert!(matches!(parsed, Entry::User { images, .. } if images.is_empty()));
        assert!(load_image(&dir.join("missing.png")).is_err());
        let pdf = dir.join("note.pdf");
        fs::write(&pdf, b"%PDF-1.4 hello").unwrap();
        let doc = load_media(&pdf).expect("pdf");
        assert_eq!(doc.kind(), "pdf");
        assert_eq!(doc.media_type(), "application/pdf");
        let mp3 = dir.join("clip.mp3");
        fs::write(&mp3, b"ID3fake").unwrap();
        assert_eq!(load_media(&mp3).unwrap().kind(), "audio");
        let mp4 = dir.join("clip.mp4");
        fs::write(&mp4, b"....ftyp").unwrap();
        assert_eq!(load_media(&mp4).unwrap().media_type(), "video/mp4");
        assert!(looks_like_media_path("notes.txt"));
        assert!(!looks_like_media_path("main.rs"));
        let _ = fs::remove_dir_all(&dir);
    }

    fn assistant_call(id: &str, name: &str) -> Entry {
        Entry::Assistant {
            text: String::new(),
            thinking: String::new(),
            calls: vec![Call {
                id: id.into(),
                name: name.into(),
                args: empty_args(),
                ..Call::default()
            }],
        }
    }

    fn tool_named(id: &str, name: &str, content: &str) -> Entry {
        Entry::Tool(ToolResult {
            id: id.into(),
            name: name.into(),
            content: content.into(),
            is_error: false,
            restored: false,
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
                ..Call::default()
            }],
        }
    }

    fn tool(id: &str, content: &str) -> Entry {
        Entry::Tool(ToolResult {
            id: id.into(),
            name: "read".into(),
            content: content.into(),
            is_error: false,
            restored: false,
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
            pending_cwd: None,
            workspace: PathBuf::new(),
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
            pending_cwd: None,
            workspace: PathBuf::new(),
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

    #[test]
    fn fork_keeps_the_previous_file_and_starts_empty() {
        let dir = tmp();
        let mut s = Session::create(&dir).expect("create");
        s.add(Entry::User {
            text: "old task".into(),
            images: Vec::new(),
        })
        .expect("add");
        let previous = s.path.clone();
        let saved = s.fork().expect("fork");
        assert_eq!(saved, previous);
        assert!(previous.exists(), "previous session stays on disk");
        assert!(s.entries.is_empty());
        assert_ne!(s.path, previous);
        assert!(!s.path.exists(), "new chat is not written until the next message");
        s.add(Entry::User {
            text: "new task".into(),
            images: Vec::new(),
        })
        .expect("add new");
        let old = Session::load(previous).expect("reload old");
        assert_eq!(old.entries.len(), 1);
        assert!(s.path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_session_is_not_written_until_first_message() {
        let dir = tmp();
        let s = Session::create(&dir).expect("create");
        assert!(s.is_empty());
        assert!(!s.path.exists(), "empty session must not hit disk");
        assert!(s.discard_if_empty().unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exit_removes_header_only_session() {
        let dir = tmp();
        let mut s = Session::create(&dir).expect("create");
        s.ensure_file().expect("header");
        assert!(s.path.exists());
        assert!(s.discard_if_empty().unwrap());
        assert!(!s.path.exists());
        assert!(s.discard_if_empty().unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_message_writes_session() {
        let dir = tmp();
        let mut s = Session::create(&dir).expect("create");
        s.add(Entry::User {
            text: "hi".into(),
            images: Vec::new(),
        })
        .expect("add");
        assert!(s.path.exists());
        assert!(!s.discard_if_empty().unwrap());
        let loaded = Session::load(s.path.clone()).expect("reload");
        assert_eq!(loaded.entries.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
