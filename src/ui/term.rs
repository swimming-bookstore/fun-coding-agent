use super::{Buffer, Rect, Style};
use anyhow::Result;
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste};
use crossterm::style::{
    Attribute, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, size as term_size, Clear, ClearType, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{queue, ExecutableCommand};
use std::io::{self, Write};

pub struct Terminal {
    prev: Buffer,
    pub size: Rect,
    cursor: Option<(u16, u16)>,
}

impl Terminal {
    pub fn start() -> Result<Self> {
        enable_raw_mode()?;
        let started = (|| -> Result<Self> {
            let mut out = io::stdout();
            out.execute(EnterAlternateScreen)?;
            out.execute(EnableBracketedPaste)?;
            out.execute(Hide)?;
            // 1000+1002+1006: wheel + drag. 1003 (all-motion) is enabled only while selecting.
            write!(out, "\x1b[?7l\x1b[?1000h\x1b[?1002h\x1b[?1006h")?;
            let (cols, rows) = term_size().unwrap_or((80, 24));
            let size = Rect::new(0, 0, cols.max(1), rows.max(1));
            queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
            out.flush()?;
            Ok(Self {
                prev: Buffer::new(size.width, size.height),
                size,
                cursor: None,
            })
        })();
        if started.is_err() {
            restore_terminal();
        }
        started
    }

    pub fn set_mouse_motion(&mut self, on: bool) -> Result<()> {
        let mut out = io::stdout();
        if on {
            write!(out, "\x1b[?1003h")?;
        } else {
            write!(out, "\x1b[?1003l")?;
        }
        out.flush()?;
        Ok(())
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.size = Rect::new(0, 0, cols.max(1), rows.max(1));
        self.prev = Buffer::new(self.size.width, self.size.height);
        self.cursor = None;
        let mut out = io::stdout();
        queue!(out, MoveTo(0, 0), Clear(ClearType::All))?;
        out.flush()?;
        Ok(())
    }

    pub fn draw_with_cursor(
        &mut self,
        paint: impl FnOnce(&mut Buffer, Rect) -> (u16, u16),
    ) -> Result<()> {
        let mut buf = Buffer::new(self.size.width, self.size.height);
        let area = buf.area();
        let cursor = paint(&mut buf, area);
        self.flush(&buf, Some(cursor))?;
        self.prev = buf;
        Ok(())
    }

    fn flush(&mut self, next: &Buffer, cursor: Option<(u16, u16)>) -> Result<()> {
        let mut out = io::stdout();
        let mut last = Style::default();
        let mut last_pos: Option<(u16, u16)> = None;
        let mut wrote = false;
        let mut wrote_cursor_cell = false;
        let want = cursor.map(|(x, y)| {
            (
                x.min(self.size.width.saturating_sub(1)),
                y.min(self.size.height.saturating_sub(1)),
            )
        });
        for y in 0..next.height {
            let mut x = 0u16;
            while x < next.width {
                let cell = next.get(x, y);
                let prev = self.prev.get(x, y);
                if cell.width == 0 {
                    x = x.saturating_add(1);
                    continue;
                }
                let w = cell.width.max(1) as u16;
                if cell != prev {
                    if last_pos != Some((x, y)) {
                        queue!(out, MoveTo(x, y))?;
                    }
                    if cell.style != last {
                        write_style(&mut out, cell.style)?;
                        last = cell.style;
                    }
                    write!(out, "{}", cell.ch)?;
                    last_pos = Some((x.saturating_add(w), y));
                    wrote = true;
                    if want == Some((x, y)) {
                        wrote_cursor_cell = true;
                    }
                }
                x = x.saturating_add(w);
            }
        }
        if wrote {
            queue!(out, ResetColor, SetAttribute(Attribute::Reset))?;
        }
        if let Some(pos) = want {
            let moved = wrote || wrote_cursor_cell || self.cursor != Some(pos);
            if moved {
                queue!(out, MoveTo(pos.0, pos.1))?;
            }
            if self.cursor.is_none() {
                queue!(out, Show)?;
                wrote = true;
            }
            self.cursor = Some(pos);
            if moved {
                wrote = true;
            }
        }
        if wrote {
            out.flush()?;
        }
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let mut out = io::stdout();
    let _ = write!(out, "\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?7h");
    let _ = out.execute(DisableMouseCapture);
    let _ = out.execute(DisableBracketedPaste);
    let _ = out.execute(Show);
    let _ = out.execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
    let _ = out.flush();
}

fn write_style(out: &mut io::Stdout, style: Style) -> Result<()> {
    queue!(out, ResetColor, SetAttribute(Attribute::Reset))?;
    if style.dim {
        queue!(out, SetAttribute(Attribute::Dim))?;
    }
    if style.bold {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    if style.italic {
        queue!(out, SetAttribute(Attribute::Italic))?;
    }
    if style.underline {
        queue!(out, SetAttribute(Attribute::Underlined))?;
    }
    if style.strike {
        queue!(out, SetAttribute(Attribute::CrossedOut))?;
    }
    if let Some(c) = style.fg {
        queue!(out, SetForegroundColor(c))?;
    }
    if let Some(c) = style.bg {
        queue!(out, SetBackgroundColor(c))?;
    }
    Ok(())
}
