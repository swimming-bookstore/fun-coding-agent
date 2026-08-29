use super::{Buffer, Rect, Style};

#[derive(Clone, Debug)]
pub struct Block {
    pub border: Style,
    pub title: Option<(String, Style)>,
}

impl Block {
    pub fn new() -> Self {
        Self {
            border: Style::new(),
            title: None,
        }
    }

    pub fn border(mut self, style: Style) -> Self {
        self.border = style;
        self
    }

    pub fn title(mut self, text: impl Into<String>, style: Style) -> Self {
        self.title = Some((text.into(), style));
        self
    }

    pub fn inner(area: Rect) -> Rect {
        if area.width < 2 || area.height < 2 {
            return Rect::new(area.x, area.y, 0, 0);
        }
        Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(1),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let x0 = area.left();
        let y0 = area.top();
        let x1 = area.right().saturating_sub(1);
        let y1 = area.bottom().saturating_sub(1);
        buf.put(x0, y0, '╭', self.border);
        buf.put(x1, y0, '╮', self.border);
        if area.height > 1 {
            buf.put(x0, y1, '╰', self.border);
            buf.put(x1, y1, '╯', self.border);
        }
        for x in x0.saturating_add(1)..x1 {
            buf.put(x, y0, '─', self.border);
            if area.height > 1 {
                buf.put(x, y1, '─', self.border);
            }
        }
        for y in y0.saturating_add(1)..y1 {
            buf.put(x0, y, '│', self.border);
            buf.put(x1, y, '│', self.border);
        }
        if let Some((title, style)) = &self.title
            && area.width > 4
        {
            let label = format!(" {title} ");
            buf.write(area, x0.saturating_add(2), y0, &label, *style);
        }
    }
}
