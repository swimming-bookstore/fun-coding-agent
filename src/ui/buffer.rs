use super::{Rect, Style};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub width: u8,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            width: 1,
            style: Style::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Buffer {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<Cell>,
}

impl Buffer {
    pub fn new(width: u16, height: u16) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        Self {
            width,
            height,
            cells: vec![Cell::default(); width as usize * height as usize],
        }
    }

    pub fn area(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    pub fn get(&self, x: u16, y: u16) -> Cell {
        self.index(x, y)
            .and_then(|i| self.cells.get(i).copied())
            .unwrap_or_default()
    }

    fn index(&self, x: u16, y: u16) -> Option<usize> {
        if x < self.width && y < self.height {
            Some(y as usize * self.width as usize + x as usize)
        } else {
            None
        }
    }

    pub fn put(&mut self, x: u16, y: u16, ch: char, style: Style) {
        let Some(w) = ch.width() else {
            return;
        };
        if w == 0 {
            return;
        }
        if u32::from(x) + w as u32 > u32::from(self.width) || y >= self.height {
            return;
        }
        let Some(i) = self.index(x, y) else {
            return;
        };
        if x > 0
            && let Some(p) = self.index(x - 1, y)
            && self.cells[p].width == 2
        {
            self.cells[p] = Cell::default();
        }
        self.cells[i] = Cell {
            ch,
            width: w as u8,
            style,
        };
        if w == 2 && let Some(n) = self.index(x + 1, y) {
            self.cells[n] = Cell {
                ch: ' ',
                width: 0,
                style,
            };
        }
    }

    pub fn write(&mut self, area: Rect, mut x: u16, y: u16, s: &str, style: Style) -> u16 {
        if y < area.top() || y >= area.bottom() {
            return x;
        }
        let max = area.right().min(self.width);
        let min = area.left();
        for ch in s.chars() {
            let w = ch.width().unwrap_or(0) as u16;
            if w == 0 {
                continue;
            }
            if x < min {
                x = x.saturating_add(w);
                continue;
            }
            if x.saturating_add(w) > max {
                break;
            }
            self.put(x, y, ch, style);
            x = x.saturating_add(w);
        }
        x
    }
}
