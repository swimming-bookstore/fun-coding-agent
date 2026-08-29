//! Optional `~/.config/fun-coding-agent/config.json`.
//! Override with `FUN_CODING_AGENT_CONFIG`.
//! Invalid JSON keeps the default palette.
//!
//! ```json
//! {
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
//!   }
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

pub fn load() -> Palette {
    config_path()
        .and_then(|p| load_from(&p))
        .unwrap_or_default()
}

fn load_from(path: &Path) -> Result<Palette> {
    if !path.exists() {
        return Ok(Palette::default());
    }
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let file: File = serde_json::from_str(&raw)
        .with_context(|| format!("parse {}", path.display()))?;
    file.colors.into_palette()
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
        let pal = load_from(&path).unwrap_or_else(|_| Palette::default());
        assert_eq!(rgb_of(pal.user), Some((247, 168, 120)));
        let _ = fs::remove_dir_all(&dir);
        let pal = load_from(Path::new("/no/such/config.json"));
        assert!(pal.is_ok(), "missing file");
        if let Ok(pal) = pal {
            assert_eq!(rgb_of(pal.user), Some((247, 168, 120)));
        }
    }
}
