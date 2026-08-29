#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn left(self) -> u16 {
        self.x
    }

    pub fn top(self) -> u16 {
        self.y
    }

    pub fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    pub fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }

    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn contains(self, x: u16, y: u16) -> bool {
        !self.is_empty()
            && x >= self.left()
            && x < self.right()
            && y >= self.top()
            && y < self.bottom()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constraint {
    Length(u16),
    Fill,
}

pub fn split(area: Rect, constraints: &[Constraint]) -> Vec<Rect> {
    if constraints.is_empty() {
        return Vec::new();
    }
    let mut sizes = vec![0u16; constraints.len()];
    let mut leftover = area.height;
    let mut fills = 0u16;
    for (i, c) in constraints.iter().enumerate() {
        match *c {
            Constraint::Length(n) => {
                let n = n.min(leftover);
                sizes[i] = n;
                leftover -= n;
            }
            Constraint::Fill => fills += 1,
        }
    }
    if let Some(each) = leftover.checked_div(fills) {
        let mut rem = leftover % fills;
        for (i, c) in constraints.iter().enumerate() {
            if matches!(c, Constraint::Fill) {
                let extra = if rem > 0 {
                    rem -= 1;
                    1
                } else {
                    0
                };
                sizes[i] = each.saturating_add(extra);
            }
        }
    }
    let mut out = Vec::with_capacity(constraints.len());
    let mut y = area.y;
    for sz in sizes {
        out.push(Rect {
            x: area.x,
            y,
            width: area.width,
            height: sz,
        });
        y = y.saturating_add(sz);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_fill_and_lengths() {
        let area = Rect::new(0, 0, 10, 10);
        let parts = split(area, &[Constraint::Fill, Constraint::Length(3), Constraint::Length(1)]);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].height, 6);
        assert_eq!(parts[1].height, 3);
        assert_eq!(parts[2].height, 1);
        assert_eq!(parts[1].y, 6);
        assert_eq!(parts[2].y, 9);
    }

    #[test]
    fn split_two_fills_share_remainder() {
        let area = Rect::new(0, 0, 8, 5);
        let parts = split(area, &[Constraint::Fill, Constraint::Fill]);
        assert_eq!(parts[0].height + parts[1].height, 5);
        assert_eq!(parts[0].height.abs_diff(parts[1].height), 1);
    }

    #[test]
    fn rect_edges() {
        let r = Rect::new(2, 3, 4, 5);
        assert_eq!(r.left(), 2);
        assert_eq!(r.top(), 3);
        assert_eq!(r.right(), 6);
        assert_eq!(r.bottom(), 8);
        assert!(!r.is_empty());
        assert!(r.contains(2, 3));
        assert!(r.contains(5, 7));
        assert!(!r.contains(6, 7));
        assert!(!r.contains(5, 8));
    }
}
