//! Optional `~/.config/fun-coding-agent/config.json`.
//! Override with `FUN_CODING_AGENT_CONFIG`.
//! Invalid JSON keeps the defaults.
//!
//! `{home}` is the trunk branch (`master` / `main` / `dev`).
//! `{branch}` is the current checkout, or `{home}` before `git init`.
//! Set `"home"` to choose which name to use when git has none yet.
//! `"when": "git"` hides a chip until the workspace is a git repo.
//! `"when": "origin"` also requires a remote named `origin`.
//! `"ask"` opens a box for extra input (origin URL) instead of sending.
//!
//! ```json
//! {
//!   "home": "master",
//!   "colors": {
//!     "text": "#e2e6f1",
//!     "muted": "#7a829a",
//!     "accent": "#7dcfef",
//!     "user": "#f7a878",
//!     "agent": "#c4a7f7",
//!     "tool": "#e8c468",
//!     "ok": "#9ece6a",
//!     "error": "#f38ba8",
//!     "code": "#89dcd2",
//!     "select": "#34436e",
//!     "queue": "#2a4056",
//!     "think": "#948ca8",
//!     "think_border": "#9a82c4"
//!   },
//!   "actions": [
//!     {
//!       "label": "[ checkout and pull {home} ]",
//!       "prompt": "checkout and pull {home}",
//!       "color": "ok",
//!       "when": "git"
//!     },
//!     {
//!       "label": "[ commit to {branch} and push ]",
//!       "prompt": "commit to {branch} and push",
//!       "color": "accent",
//!       "ask": "init git on {home} if needed, add origin "
//!     }
//!   ]
//! }
//! ```

use crate::ui::Color;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
pub struct Palette {
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub user: Color,
    pub agent: Color,
    pub tool: Color,
    pub ok: Color,
    pub error: Color,
    pub code: Color,
    pub select: Color,
    pub queue: Color,
    pub think: Color,
    pub think_border: Color,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub label: String,
    pub prompt: String,
    pub color: ActionColor,
    pub when: ActionWhen,
    pub ask: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActionWhen {
    #[default]
    Always,
    Git,
    Origin,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActionColor {
    #[default]
    Accent,
    Ok,
    User,
    Agent,
    Tool,
    Muted,
    Text,
    Error,
}

impl Action {
    pub fn defaults() -> Vec<Self> {
        vec![
            Self {
                label: "[ checkout and pull {home} ]".into(),
                prompt: "checkout and pull {home}".into(),
                color: ActionColor::Ok,
                when: ActionWhen::Git,
                ask: None,
            },
            Self {
                label: "[ commit to {branch} and push ]".into(),
                prompt: "commit to {branch} and push".into(),
                color: ActionColor::Accent,
                when: ActionWhen::Always,
                ask: Some("init git on {home} if needed, add origin ".into()),
            },
        ]
    }
}

fn named(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// `{home}` is trunk. `{branch}` is the current checkout, or `{home}` before git exists.
pub fn fill_action(s: &str, home: Option<&str>, branch: Option<&str>) -> Option<String> {
    if s.trim().is_empty() {
        return None;
    }
    let mut out = s.to_string();
    if out.contains("{home}") {
        out = out.replace("{home}", named(home)?);
    }
    if out.contains("{branch}") {
        out = out.replace("{branch}", named(branch).or_else(|| named(home))?);
    }
    Some(out)
}

#[derive(Clone)]
pub struct Config {
    pub palette: Palette,
    pub actions: Vec<Action>,
    pub home: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            palette: Palette::default(),
            actions: Action::defaults(),
            home: "master".into(),
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            text: rgb(226, 230, 241),
            muted: rgb(122, 130, 154),
            accent: rgb(125, 207, 239),
            user: rgb(247, 168, 120),
            agent: rgb(196, 167, 247),
            tool: rgb(232, 196, 104),
            ok: rgb(158, 206, 106),
            error: rgb(243, 139, 168),
            code: rgb(137, 220, 210),
            select: rgb(52, 67, 110),
            queue: rgb(42, 64, 86),
            think: rgb(148, 140, 168),
            think_border: rgb(154, 130, 196),
        }
    }
}

#[derive(Default, Deserialize)]
struct File {
    #[serde(default)]
    colors: ColorFile,
    actions: Option<Vec<ActionFile>>,
    home: Option<String>,
}

#[derive(Default, Deserialize)]
struct ActionFile {
    label: Option<String>,
    prompt: Option<String>,
    color: Option<String>,
    when: Option<String>,
    ask: Option<String>,
}

#[derive(Default, Deserialize)]
struct ColorFile {
    text: Option<String>,
    muted: Option<String>,
    accent: Option<String>,
    user: Option<String>,
    agent: Option<String>,
    tool: Option<String>,
    ok: Option<String>,
    error: Option<String>,
    code: Option<String>,
    select: Option<String>,
    queue: Option<String>,
    think: Option<String>,
    think_border: Option<String>,
}

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb { r, g, b }
}

fn config_path() -> Result<PathBuf> {
    if let Ok(path) = env::var("FUN_CODING_AGENT_CONFIG")
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    Ok(config_dir()?.join("config.json"))
}

fn config_dir() -> Result<PathBuf> {
    if let Ok(dir) = env::var("XDG_CONFIG_HOME")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir).join("fun-coding-agent"));
    }
    let home = env::var("HOME").context("HOME is not set")?;
    if home.is_empty() {
        bail!("HOME is not set");
    }
    Ok(PathBuf::from(home).join(".config/fun-coding-agent"))
}

pub fn load() -> Config {
    config_path()
        .and_then(|p| load_from(&p))
        .unwrap_or_default()
}

fn load_from(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let file: File = serde_json::from_str(&raw)
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(Config {
        palette: file.colors.into_palette()?,
        actions: match file.actions {
            None => Action::defaults(),
            Some(items) => items
                .into_iter()
                .filter_map(|item| item.into_action())
                .collect(),
        },
        home: parse_home(file.home),
    })
}

impl ActionFile {
    fn into_action(self) -> Option<Action> {
        let label = self.label?.trim().to_string();
        let prompt = self.prompt?.trim().to_string();
        if label.is_empty() || prompt.is_empty() {
            return None;
        }
        Some(Action {
            label,
            prompt,
            color: parse_action_color(self.color.as_deref()),
            when: parse_action_when(self.when.as_deref()),
            ask: self
                .ask
                .map(|s| s.trim_start().to_string())
                .filter(|s| !s.trim().is_empty()),
        })
    }
}

fn parse_home(value: Option<String>) -> String {
    match value.as_deref().map(str::trim).unwrap_or("") {
        "main" => "main".into(),
        "dev" => "dev".into(),
        _ => "master".into(),
    }
}

fn parse_action_when(name: Option<&str>) -> ActionWhen {
    match name.map(str::trim).unwrap_or("") {
        "origin" => ActionWhen::Origin,
        "git" => ActionWhen::Git,
        _ => ActionWhen::Always,
    }
}

fn parse_action_color(name: Option<&str>) -> ActionColor {
    match name.map(str::trim).unwrap_or("") {
        "ok" => ActionColor::Ok,
        "user" => ActionColor::User,
        "agent" => ActionColor::Agent,
        "tool" => ActionColor::Tool,
        "muted" => ActionColor::Muted,
        "text" => ActionColor::Text,
        "error" => ActionColor::Error,
        _ => ActionColor::Accent,
    }
}

impl ColorFile {
    fn into_palette(self) -> Result<Palette> {
        let mut pal = Palette::default();
        overlay(&mut pal.text, self.text, "colors.text")?;
        overlay(&mut pal.muted, self.muted, "colors.muted")?;
        overlay(&mut pal.accent, self.accent, "colors.accent")?;
        overlay(&mut pal.user, self.user, "colors.user")?;
        overlay(&mut pal.agent, self.agent, "colors.agent")?;
        overlay(&mut pal.tool, self.tool, "colors.tool")?;
        overlay(&mut pal.ok, self.ok, "colors.ok")?;
        overlay(&mut pal.error, self.error, "colors.error")?;
        overlay(&mut pal.code, self.code, "colors.code")?;
        overlay(&mut pal.select, self.select, "colors.select")?;
        overlay(&mut pal.queue, self.queue, "colors.queue")?;
        overlay(&mut pal.think, self.think, "colors.think")?;
        overlay(&mut pal.think_border, self.think_border, "colors.think_border")?;
        Ok(pal)
    }
}

fn overlay(slot: &mut Color, value: Option<String>, key: &str) -> Result<()> {
    if let Some(s) = value {
        *slot = parse_color(&s).with_context(|| format!("{key}: {s}"))?;
    }
    Ok(())
}

fn parse_color(s: &str) -> Result<Color> {
    let s = s.trim();
    let hex = s.strip_prefix('#').unwrap_or(s);
    let n = hex.len();
    if n != 3 && n != 6 {
        bail!("expected #rgb or #rrggbb");
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("expected #rgb or #rrggbb");
    }
    let (r, g, b) = if n == 3 {
        let r = nibble(hex.as_bytes()[0])?;
        let g = nibble(hex.as_bytes()[1])?;
        let b = nibble(hex.as_bytes()[2])?;
        (r * 17, g * 17, b * 17)
    } else {
        (
            u8::from_str_radix(&hex[0..2], 16)?,
            u8::from_str_radix(&hex[2..4], 16)?,
            u8::from_str_radix(&hex[4..6], 16)?,
        )
    };
    Ok(rgb(r, g, b))
}

fn nibble(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => bail!("expected #rgb or #rrggbb"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rgb_of(c: Color) -> Option<(u8, u8, u8)> {
        match c {
            Color::Rgb { r, g, b } => Some((r, g, b)),
            _ => None,
        }
    }

    #[test]
    fn parse_hex() {
        assert_eq!(parse_color("#e2e6f1").ok().and_then(rgb_of), Some((226, 230, 241)));
        assert_eq!(parse_color("7dcfe7").ok().and_then(rgb_of), Some((125, 207, 231)));
        assert_eq!(parse_color("#fff").ok().and_then(rgb_of), Some((255, 255, 255)));
        assert_eq!(parse_color("#f80").ok().and_then(rgb_of), Some((255, 136, 0)));
        assert!(parse_color("red").is_err());
        assert!(parse_color("#gg0000").is_err());
    }

    #[test]
    fn overlay_keeps_defaults() {
        let pal = ColorFile::default().into_palette();
        assert!(pal.is_ok(), "default palette");
        if let Ok(pal) = pal {
            assert_eq!(rgb_of(pal.user), Some((247, 168, 120)));
        }
        let pal = ColorFile {
            user: Some("#ffcc88".into()),
            error: Some("#f00".into()),
            ..ColorFile::default()
        }
        .into_palette();
        assert!(pal.is_ok(), "overlay palette");
        if let Ok(pal) = pal {
            assert_eq!(rgb_of(pal.user), Some((255, 204, 136)));
            assert_eq!(rgb_of(pal.error), Some((255, 0, 0)));
            assert_eq!(rgb_of(pal.accent), Some((125, 207, 239)));
        }
    }

    #[test]
    fn file_shape() {
        let file = serde_json::from_value::<File>(json!({
            "colors": { "user": "#abc" }
        }));
        assert!(file.is_ok(), "parse file");
        let pal = file.ok().map(|f| f.colors.into_palette());
        assert!(pal.as_ref().is_some_and(Result::is_ok), "palette");
        if let Some(Ok(pal)) = pal {
            assert_eq!(rgb_of(pal.user), Some((170, 187, 204)));
        }
    }

    #[test]
    fn bad_file_keeps_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "fun-cfg-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        assert!(fs::write(&path, "{not json").is_ok(), "write bad config");
        let cfg = load_from(&path).unwrap_or_default();
        assert_eq!(rgb_of(cfg.palette.user), Some((247, 168, 120)));
        let _ = fs::remove_dir_all(&dir);
        let cfg = load_from(Path::new("/no/such/config.json"));
        assert!(cfg.is_ok(), "missing file");
        if let Ok(cfg) = cfg {
            assert_eq!(rgb_of(cfg.palette.user), Some((247, 168, 120)));
            assert_eq!(cfg.actions, Action::defaults());
            assert_eq!(cfg.home, "master");
        }
    }

    #[test]
    fn actions_from_file() {
        let file = serde_json::from_value::<File>(json!({
            "home": "main",
            "actions": [
                { "label": "[ checkout and pull {home} ]", "prompt": "checkout and pull {home}", "color": "ok", "when": "git" },
                { "label": "  ", "prompt": "skip" },
                { "label": "[ custom ]", "prompt": "do the thing", "color": "user", "ask": "add origin " }
            ]
        }))
        .expect("parse");
        assert_eq!(parse_home(file.home.clone()), "main");
        let actions: Vec<Action> = file
            .actions
            .unwrap_or_default()
            .into_iter()
            .filter_map(ActionFile::into_action)
            .collect();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].color, ActionColor::Ok);
        assert_eq!(actions[0].when, ActionWhen::Git);
        assert_eq!(actions[1].label, "[ custom ]");
        assert_eq!(actions[1].color, ActionColor::User);
        assert_eq!(actions[1].when, ActionWhen::Always);
        assert_eq!(actions[1].ask.as_deref(), Some("add origin "));
        assert_eq!(
            fill_action("checkout and pull {home}", Some("master"), None).as_deref(),
            Some("checkout and pull master")
        );
        assert_eq!(fill_action("checkout and pull {home}", None, None), None);
        assert_eq!(
            fill_action("[ commit to {home} and push ]", Some("dev"), None).as_deref(),
            Some("[ commit to dev and push ]")
        );
        assert_eq!(
            fill_action("commit to {branch} and push", Some("master"), Some("master")).as_deref(),
            Some("commit to master and push")
        );
        assert_eq!(
            fill_action(
                "[ commit to {branch} and push ]",
                Some("master"),
                Some("feat")
            )
            .as_deref(),
            Some("[ commit to feat and push ]")
        );
        assert_eq!(
            fill_action("commit to {branch} and push", Some("master"), None).as_deref(),
            Some("commit to master and push")
        );
        assert_eq!(fill_action("commit to {branch} and push", None, None), None);
        assert_eq!(
            fill_action("checkout and pull {home}", Some("master"), Some("feat")).as_deref(),
            Some("checkout and pull master")
        );
        assert_eq!(parse_home(Some("dev".into())), "dev");
        assert_eq!(parse_home(Some("trunk".into())), "master");
    }
}
