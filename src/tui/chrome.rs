use super::*;
use crate::ui::{Block, Buffer, Rect, Style};
use fun_core::agent::{ToolRun, tool_counts, tool_summary};
use fun_core::config::{ActionColor, ActionWhen};
use unicode_width::UnicodeWidthStr;

pub(super) const COMPOSER_MAX: u16 = 12;

pub(super) fn chrome_heights(total: u16, status_wanted: u16, composer_wanted: u16) -> (u16, u16) {
    if total == 0 {
        return (0, 0);
    }
    if total == 1 {
        return (0, 1);
    }
    if total <= 4 {
        return (total.saturating_sub(1).min(3), 1);
    }
    let status_h = status_wanted.min(total.saturating_sub(3)).max(1);
    let rest = total.saturating_sub(status_h);
    let cap = rest.saturating_sub(1).clamp(3, COMPOSER_MAX);
    let composer_h = composer_wanted.clamp(3, cap);
    (composer_h, status_h)
}

pub(super) fn composer_text_width(area_width: u16) -> usize {
    area_width.saturating_sub(6).max(1) as usize
}

pub(super) fn composer_wanted_height(atoms: &[ComposerAtom], width: u16) -> u16 {
    let lines = composer_view(atoms, 0, composer_text_width(width))
        .lines
        .len()
        .max(1) as u16;
    lines.saturating_add(2).max(3)
}

pub(super) fn status_rows(
    bar: &Bar,
    working: bool,
    spinner: usize,
    scroll: usize,
    width: u16,
) -> Vec<Vec<(String, Style)>> {
    let inner = width.saturating_sub(2).max(1) as usize;
    let mut ident: Vec<(String, Style)> = Vec::new();
    let mut stats: Vec<(String, Style)> = Vec::new();
    let mut right: Vec<(String, Style)> = Vec::new();
    let push = |out: &mut Vec<(String, Style)>, text: &str, style: Style| {
        if !text.is_empty() {
            out.push((text.to_string(), style));
        }
    };
    let push_item = |out: &mut Vec<(String, Style)>, text: &str, style: Style| {
        if text.is_empty() {
            return;
        }
        if !out.is_empty() {
            push(out, "  ", muted());
        }
        push(out, text, style);
    };
    if working {
        push_item(
            &mut ident,
            &format!("{} working", SPINNER[spinner % SPINNER.len()]),
            tool_col(),
        );
    }
    push_item(&mut ident, &bar.workspace, accent().bold());
    if let Some(branch) = &bar.branch {
        push_item(&mut ident, branch, user_col());
    }
    let add_stat =
        |out: &mut Vec<(String, Style)>, label: &str, value: &str, color: fun_core::config::Rgb| {
            if value.is_empty() {
                return;
            }
            push_item(out, &format!("{label} {value}"), pal_col(color));
        };
    add_stat(&mut stats, "input tokens", &bar.input, pal().text);
    add_stat(&mut stats, "output tokens", &bar.output, pal().ok);
    add_stat(&mut stats, "reasoning tokens", &bar.reasoning, pal().agent);
    add_stat(&mut stats, "cache hit", &bar.cache_hit, pal().tool);
    push_item(&mut stats, &bar.cost, user_col());
    push_item(&mut stats, &bar.context, muted());

    push_item(&mut right, bar.model.trim(), agent_col());
    push_item(&mut right, bar.effort.trim(), muted());
    if scroll > 0 {
        push_item(&mut right, &format!("+{scroll}"), accent());
    }

    let ident_w = segs_width(&ident);
    let stats_w = segs_width(&stats);
    let right_w = segs_width(&right);
    let gap = |a: usize, b: usize| -> usize { if a > 0 && b > 0 { 2 } else { 0 } };
    let one = ident_w + gap(ident_w, stats_w) + stats_w + gap(ident_w + stats_w, right_w) + right_w;
    if one <= inner {
        let mut line = ident;
        if !line.is_empty() && !stats.is_empty() {
            push(&mut line, "  ", muted());
        }
        line.extend(stats);
        return vec![align_right(line, right, inner)];
    }
    let mut left = ident;
    if !left.is_empty() && !stats.is_empty() {
        push(&mut left, "  ", muted());
    }
    left.extend(stats);
    wrap_status(left, right, inner)
}

pub(super) fn segs_width(segs: &[(String, Style)]) -> usize {
    segs.iter().map(|(t, _)| width(t)).sum()
}

pub(super) fn align_right(
    mut left: Vec<(String, Style)>,
    right: Vec<(String, Style)>,
    inner: usize,
) -> Vec<(String, Style)> {
    let left_w = segs_width(&left);
    let right_w = segs_width(&right);
    if right_w == 0 {
        return left;
    }
    let gap = inner.saturating_sub(left_w.saturating_add(right_w));
    if gap > 0 {
        left.push((" ".repeat(gap), muted()));
    }
    left.extend(right);
    left
}

pub(super) fn wrap_status(
    left: Vec<(String, Style)>,
    right: Vec<(String, Style)>,
    inner: usize,
) -> Vec<Vec<(String, Style)>> {
    let mut rows: Vec<Vec<(String, Style)>> = Vec::new();
    let mut cur: Vec<(String, Style)> = Vec::new();
    let mut used = 0usize;
    let push_seg = |rows: &mut Vec<Vec<(String, Style)>>,
                    cur: &mut Vec<(String, Style)>,
                    used: &mut usize,
                    text: String,
                    style: Style| {
        let w = width(&text);
        if w == 0 || inner == 0 {
            return;
        }
        if *used == 0 && text.trim().is_empty() {
            return;
        }
        if *used > 0 && used.saturating_add(w) > inner {
            rows.push(std::mem::take(cur));
            *used = 0;
            if text.trim().is_empty() {
                return;
            }
        }
        if w > inner && cur.is_empty() {
            cur.push((clip_width(&text, inner), style));
            rows.push(std::mem::take(cur));
            *used = 0;
            return;
        }
        cur.push((text, style));
        *used = used.saturating_add(w);
    };
    for (text, style) in left {
        push_seg(&mut rows, &mut cur, &mut used, text, style);
    }
    let right_w = segs_width(&right);
    if !right.is_empty() {
        if !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
            used = 0;
        }
        if right_w <= inner {
            if right_w < inner {
                cur.push((" ".repeat(inner - right_w), muted()));
            }
            cur.extend(right);
        } else {
            for (text, style) in right {
                push_seg(&mut rows, &mut cur, &mut used, text, style);
            }
            if used > 0 && used < inner {
                cur.insert(0, (" ".repeat(inner - used), muted()));
            }
        }
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}

pub(super) fn status(
    buf: &mut Buffer,
    area: Rect,
    bar: &Bar,
    working: bool,
    spinner: usize,
    scroll: usize,
) {
    if area.is_empty() {
        return;
    }
    let rows = status_rows(bar, working, spinner, scroll, area.width);
    let x0 = area.x.saturating_add(1);
    for (i, row) in rows.into_iter().take(area.height as usize).enumerate() {
        let y = area.y.saturating_add(i as u16);
        if y >= area.bottom() {
            break;
        }
        let mut x = x0;
        for (text, style) in row {
            x = buf.write(area, x, y, &text, style);
        }
    }
}

pub(super) fn prev_char(s: &str, mut i: usize) -> usize {
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub(super) fn next_char(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    i += 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

pub(super) fn fill_rect(buf: &mut Buffer, area: Rect, style: Style) {
    let mut y = area.top();
    while y < area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            buf.put(x, y, ' ', style);
            x = x.saturating_add(1);
        }
        y = y.saturating_add(1);
    }
}

pub(super) fn paint_ask(buf: &mut Buffer, screen: Rect, ask: &AskDialog) -> (u16, u16) {
    if screen.is_empty() {
        return (screen.x, screen.y);
    }
    fill_rect(buf, screen, Style::new());
    let title = if ask.title.trim().is_empty() {
        "git origin URL"
    } else {
        ask.title.trim()
    };
    let hint = if ask.hint.trim().is_empty() {
        "Enter send · Esc cancel"
    } else {
        ask.hint.trim()
    };
    let inner_w = (width(title) + 8)
        .max(width(hint) + 2)
        .max(width(&ask.placeholder) + 2)
        .max(44)
        .min(screen.width.saturating_sub(4) as usize)
        .max(16);
    let w = (inner_w as u16).saturating_add(4).min(screen.width).max(10);
    let h = 8u16.min(screen.height).max(5);
    let x = screen.x.saturating_add(screen.width.saturating_sub(w) / 2);
    let y = screen.y.saturating_add(screen.height.saturating_sub(h) / 2);
    let box_area = Rect::new(x, y, w, h);
    fill_rect(buf, box_area, Style::new());
    Block::new()
        .border(accent())
        .title(title, accent().bold())
        .render(buf, box_area);
    let inner = Block::inner(box_area);
    if inner.is_empty() {
        return (box_area.x.saturating_add(1), box_area.y.saturating_add(1));
    }
    let field_y = inner
        .y
        .saturating_add(1)
        .min(inner.bottom().saturating_sub(1));
    let field = Rect::new(
        inner.x.saturating_add(1),
        field_y,
        inner.width.saturating_sub(2),
        1,
    );
    let max = field.width.saturating_sub(1).max(1) as usize;
    let cursor = if ask.value.is_empty() && !ask.placeholder.is_empty() {
        buf.write(
            field,
            field.x,
            field.y,
            &clip_width(&ask.placeholder, max),
            muted().italic(),
        );
        (field.x, field.y)
    } else {
        let (shown, cur) = visible_input(&ask.value, ask.cursor, max);
        buf.write(field, field.x, field.y, &shown, pal_col(pal().text));
        (field.x.saturating_add(cur as u16), field.y)
    };
    if inner.height > 3 {
        buf.write(
            inner,
            inner.x.saturating_add(1),
            inner.bottom().saturating_sub(1),
            hint,
            muted(),
        );
    }
    cursor
}

pub(super) fn order_sel(start: (u16, usize), end: (u16, usize)) -> ((u16, usize), (u16, usize)) {
    if (start.1, start.0) <= (end.1, end.0) {
        (start, end)
    } else {
        (end, start)
    }
}

pub(super) fn action_items(
    home: Option<&str>,
    branch: Option<&str>,
) -> Vec<(String, ActionHit, ActionColor)> {
    actions()
        .into_iter()
        .enumerate()
        .filter_map(|(index, spec)| {
            match spec.when {
                ActionWhen::Git if !git_present() => return None,
                ActionWhen::Origin if !has_origin() => return None,
                _ => {}
            }
            spec.filled_prompt(home, branch, None)?;
            let label = spec.filled_label(home, branch)?;
            Some((label, ActionHit { index }, spec.color))
        })
        .collect()
}

pub(super) fn action_style(color: ActionColor) -> Style {
    match color {
        ActionColor::Ok => ok_col().bold(),
        ActionColor::User => user_col().bold(),
        ActionColor::Agent => agent_col().bold(),
        ActionColor::Tool => tool_col().bold(),
        ActionColor::Muted => muted().bold(),
        ActionColor::Text => pal_col(pal().text).bold(),
        ActionColor::Error => err_col().bold(),
        ActionColor::Accent => accent().bold(),
    }
}

pub(super) fn action_bar_height(
    width: u16,
    home: Option<&str>,
    branch: Option<&str>,
    room: u16,
) -> u16 {
    if room == 0 || width < 8 || action_items(home, branch).is_empty() {
        0
    } else {
        1
    }
}

pub(super) fn paint_action_bar(
    buf: &mut Buffer,
    area: Rect,
    home: Option<&str>,
    branch: Option<&str>,
) -> Vec<(Rect, ActionHit)> {
    if area.is_empty() {
        return Vec::new();
    }
    let items = action_items(home, branch);
    if items.is_empty() {
        return Vec::new();
    }
    let y = area.y;
    let gap = 1u16;
    let mut chips: Vec<(String, ActionHit, ActionColor, u16)> = Vec::new();
    let mut total = 0u16;
    for (label, hit, color) in items {
        let w = (width(&label) as u16).min(area.width).max(1);
        if total > 0 {
            total = total.saturating_add(gap);
        }
        if total.saturating_add(w) > area.width {
            break;
        }
        total = total.saturating_add(w);
        chips.push((label, hit, color, w));
    }
    if chips.is_empty() {
        return Vec::new();
    }
    let mut x = area.right().saturating_sub(total).max(area.x);
    let mut out = Vec::new();
    for (i, (label, hit, color, w)) in chips.into_iter().enumerate() {
        if i > 0 {
            x = x.saturating_add(gap);
        }
        let toast = Rect::new(x, y, w, 1);
        buf.write(area, x, y, &label, action_style(color));
        out.push((toast, hit));
        x = x.saturating_add(w);
    }
    out
}

pub(super) fn paint_copied(buf: &mut Buffer, screen: Rect, composer: Rect) {
    if screen.is_empty() {
        return;
    }
    let label = "copied";
    let inner = width(label) as u16;
    let boxed = screen.width >= inner.saturating_add(4) && screen.height >= 3;
    let (w, h) = if boxed {
        (inner.saturating_add(4), 3)
    } else {
        (inner.min(screen.width).max(1), 1)
    };
    if w > screen.width || h > screen.height {
        return;
    }
    let anchor = if composer.is_empty() {
        screen
    } else {
        composer
    };
    let x = anchor
        .x
        .saturating_add(anchor.width.saturating_sub(w) / 2)
        .clamp(screen.x, screen.right().saturating_sub(w));
    let y = if !composer.is_empty() && composer.y >= screen.y.saturating_add(h) {
        composer.y.saturating_sub(h)
    } else if !composer.is_empty() && composer.height > 1 {
        composer.y
    } else {
        screen
            .bottom()
            .saturating_sub(h.saturating_add(1))
            .max(screen.y)
    };
    let toast = Rect::new(x, y, w, h);
    let mut ty = toast.top();
    while ty < toast.bottom() {
        let mut tx = toast.left();
        while tx < toast.right() {
            buf.put(tx, ty, ' ', Style::new());
            tx = tx.saturating_add(1);
        }
        ty = ty.saturating_add(1);
    }
    if boxed {
        Block::new().border(ok_col()).render(buf, toast);
        let inner_area = Block::inner(toast);
        if !inner_area.is_empty() {
            buf.write(
                inner_area,
                inner_area.x.saturating_add(1),
                inner_area.y,
                label,
                ok_col().bold(),
            );
        }
    } else {
        buf.write(toast, toast.x, toast.y, label, ok_col().bold());
    }
}

pub(super) fn next_prune_scroll(scroll: usize, delta: i32, max: usize) -> usize {
    if delta > 0 {
        scroll.saturating_add(delta as usize).min(max)
    } else {
        scroll.saturating_sub(delta.unsigned_abs() as usize)
    }
}

pub(super) fn prune_max_scroll(n: usize, screen: Rect) -> usize {
    if screen.is_empty() || n == 0 {
        return 0;
    }
    let h = ((n as u16).saturating_add(4))
        .min(screen.height.saturating_sub(2).max(6))
        .max(6)
        .min(screen.height)
        .max(1);
    let inner_h = h.saturating_sub(2);
    let body_h = inner_h.saturating_sub(1).max(1) as usize;
    n.saturating_sub(body_h)
}

pub(super) fn paint_prune(buf: &mut Buffer, screen: Rect, lines: &[String], scroll: usize) {
    if screen.is_empty() {
        return;
    }
    let title = if lines.is_empty() {
        "pruned from context — none"
    } else {
        "pruned from context"
    };
    let hint = "Esc close · ↑↓ PgUp/PgDn scroll";
    let inner_w = (width(title) + 8)
        .max(width(hint) + 2)
        .max(lines.iter().map(|s| width(s)).max().unwrap_or(24) + 2)
        .min(screen.width.saturating_sub(4) as usize)
        .max(28);
    let w = (inner_w as u16).saturating_add(4).min(screen.width).max(20);
    let h = ((lines.len() as u16).saturating_add(4))
        .min(screen.height.saturating_sub(2).max(6))
        .max(6);
    let x = screen.x.saturating_add(screen.width.saturating_sub(w) / 2);
    let y = screen.y.saturating_add(screen.height.saturating_sub(h) / 2);
    let box_area = Rect::new(x, y, w, h);
    fill_rect(buf, box_area, Style::new());
    Block::new()
        .border(tool_col())
        .title(title, tool_col().bold())
        .render(buf, box_area);
    let inner = Block::inner(box_area);
    if inner.is_empty() {
        return;
    }
    let body_h = inner.height.saturating_sub(1).max(1) as usize;
    let max_scroll = prune_max_scroll(lines.len(), screen);
    let start = scroll.min(max_scroll);
    let clip = inner.width.saturating_sub(1).max(1) as usize;
    if lines.is_empty() {
        buf.write(
            inner,
            inner.x,
            inner.y,
            &clip_width("nothing hidden from the model yet", clip),
            muted(),
        );
    } else {
        for (row, line) in lines.iter().skip(start).take(body_h).enumerate() {
            buf.write(
                inner,
                inner.x,
                inner.y.saturating_add(row as u16),
                &clip_width(line, clip),
                pal_col(pal().text),
            );
        }
    }
    if inner.height > 1 {
        buf.write(
            inner,
            inner.x,
            inner.bottom().saturating_sub(1),
            &clip_width(hint, clip),
            muted(),
        );
    }
}

pub(super) fn overlay_above(screen: Rect, composer: Rect, height: u16) -> Rect {
    if screen.is_empty() || height == 0 {
        return Rect::new(0, 0, 0, 0);
    }
    let h = height.min(screen.height);
    let y = if !composer.is_empty() && composer.y >= screen.y.saturating_add(h) {
        composer.y.saturating_sub(h)
    } else if !composer.is_empty() && composer.y > screen.y {
        screen.y
    } else {
        screen
            .bottom()
            .saturating_sub(h.saturating_add(composer.height.max(1)))
            .max(screen.y)
    };
    Rect::new(screen.x, y, screen.width, h)
}

pub(super) fn paint_select(
    buf: &mut Buffer,
    area: Rect,
    select: Option<((u16, usize), (u16, usize))>,
    body_start: usize,
) {
    let Some(select) = select else {
        return;
    };
    let (a, b) = order_sel(select.0, select.1);
    if a == b || area.is_empty() {
        return;
    }
    let vis_top = body_start;
    let vis_bot = body_start.saturating_add(area.height.saturating_sub(1) as usize);
    let row0 = a.1.max(vis_top);
    let row1 = b.1.min(vis_bot);
    if row0 > row1 {
        return;
    }
    for row in row0..=row1 {
        let y = area.y.saturating_add((row - vis_top) as u16);
        if y >= area.bottom() {
            break;
        }
        let mut x = area.left();
        while x < area.right() {
            let cell = buf.get(x, y);
            let w = cell.width.max(1) as u16;
            let on_first = row == a.1;
            let on_last = row == b.1;
            let hit = if on_first && on_last {
                x >= a.0 && x <= b.0
            } else if on_first {
                x >= a.0
            } else if on_last {
                x <= b.0
            } else {
                true
            };
            if hit {
                buf.put(x, y, cell.ch, cell.style.bg(rgb_color(pal().select)));
            }
            x = x.saturating_add(w.max(1));
        }
    }
}

pub(super) fn is_prune_note(text: &str) -> bool {
    let t = text.trim();
    (t.contains("dropped ") && t.contains("from context")) || t.contains("pruned from context")
}

pub(super) fn is_status(item: &Item) -> bool {
    matches!(item, Item::Note { .. } | Item::Tools { .. })
}

pub(super) fn gap_before(prev: &Item, item: &Item) -> usize {
    if matches!((prev, item), (Item::Md(_), Item::Md(_))) || is_status(item) || is_status(prev) {
        1
    } else {
        2
    }
}

pub(super) fn indent_lines(lines: Vec<Line>, pad: &str) -> Vec<Line> {
    let extra = width(pad);
    lines
        .into_iter()
        .map(|line| {
            if line.spans.is_empty() {
                line
            } else {
                let hang = line.hang.saturating_add(extra);
                let mut spans = Vec::with_capacity(line.spans.len() + 1);
                spans.push(Span::new(pad, Style::new()));
                spans.extend(line.spans);
                Line { spans, hang }
            }
        })
        .collect()
}

pub(super) fn layout_items(
    items: &[Item],
    width: usize,
) -> (Vec<Line>, Vec<(usize, usize, usize)>) {
    let mut rows = Vec::new();
    let mut map = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let prev = if i == 0 { None } else { Some(&items[i - 1]) };
        if let Some(prev) = prev {
            for _ in 0..gap_before(prev, item) {
                rows.push(Line::empty());
            }
        }
        let start = rows.len();
        let (summary, body) = layout_item(item, width);
        rows.extend(body);
        map.push((start, start.saturating_add(summary), rows.len()));
    }
    (rows, map)
}

pub(super) fn layout_item(item: &Item, width: usize) -> (usize, Vec<Line>) {
    let body_w = width.saturating_sub(2).max(1);
    match item {
        Item::User(text) => {
            let body: Vec<Line> = text
                .split('\n')
                .map(|chunk| Line::plain(chunk.to_string(), user_col()))
                .collect();
            let lines = wrap_lines(&body, width.max(1));
            (lines.len(), lines)
        }
        Item::Md(text) => {
            let lines = indent_lines(wrap_lines(&render_md(text, body_w), body_w), "  ");
            (lines.len(), lines)
        }
        Item::Note { text, inspect } => {
            let style = if inspect.as_ref().is_some_and(|l| !l.is_empty()) || is_prune_note(text) {
                muted().underline()
            } else {
                muted()
            };
            let lines = indent_lines(
                wrap_lines(&[Line::plain(text.clone(), style)], body_w),
                "  ",
            );
            (lines.len(), lines)
        }
        Item::Tools { runs, open } => tool_block(runs, *open, width.max(1)),
    }
}

pub(super) fn tool_summary_spans(ok: usize, fail: usize) -> Vec<Span> {
    match (ok, fail) {
        (0, 0) => Vec::new(),
        (n, 0) => vec![Span::new(tool_summary(n, 0), ok_col())],
        (0, _) => vec![Span::new(tool_summary(0, fail), err_col())],
        (n, _) => {
            let ok_part = tool_summary(n, 0);
            let fail_part = tool_summary(0, fail);
            vec![
                Span::new(ok_part, ok_col()),
                Span::new("  ".to_string(), muted()),
                Span::new(fail_part, err_col()),
            ]
        }
    }
}

pub(super) fn tool_block(runs: &[ToolRun], open: bool, width: usize) -> (usize, Vec<Line>) {
    let mut lines = Vec::new();
    let (ok, fail) = tool_counts(runs);
    let summary = tool_summary_spans(ok, fail);
    if !summary.is_empty() {
        let inner = width.saturating_sub(2).max(1);
        lines.extend(indent_lines(
            wrap_lines(&[Line::spans(summary)], inner),
            "  ",
        ));
    }
    let summary_len = lines.len();
    let show: Vec<&ToolRun> = if open {
        runs.iter().collect()
    } else {
        runs.iter().filter(|r| r.is_error).collect()
    };
    if !lines.is_empty() && !show.is_empty() {
        lines.push(Line::empty());
    }
    let indent = "  ";
    let mark = "⏺ ";
    let call_hang = UnicodeWidthStr::width(indent) + UnicodeWidthStr::width(mark);
    let detail_pad = format!("{indent}  ");
    let detail_hang = UnicodeWidthStr::width(detail_pad.as_str());
    for (i, r) in show.iter().enumerate() {
        if open && i > 0 {
            lines.push(Line::empty());
        }
        let style = if r.is_error { err_col() } else { ok_col() };
        let call = format!("{indent}{mark}{}", r.call_line());
        lines.extend(wrap_lines(
            &[Line::plain(call, style).with_hang(call_hang)],
            width,
        ));
        if r.is_error {
            for line in r.detail.lines() {
                lines.extend(wrap_lines(
                    &[
                        Line::plain(format!("{detail_pad}{line}"), err_col())
                            .with_hang(detail_hang),
                    ],
                    width,
                ));
            }
        }
    }
    (summary_len, lines)
}

pub(super) fn think_lines(text: &str, width: usize) -> Vec<Line> {
    let body: Vec<Line> = text
        .split('\n')
        .map(|chunk| Line::plain(chunk.to_string(), think_col()))
        .collect();
    wrap_lines(&body, width.max(1))
}

pub(super) fn think_height(think: &str, width: u16, room: u16) -> u16 {
    if think.trim().is_empty() {
        return 0;
    }
    let inner_w = width.saturating_sub(4).max(1) as usize;
    let content = think_lines(think, inner_w).len().max(1) as u16;
    if room < 3 {
        return 0;
    }
    let max_content = (room / 3).clamp(1, 6);
    content.min(max_content).saturating_add(2).min(room).max(3)
}

pub(super) fn think_panel(buf: &mut Buffer, area: Rect, think: &str) {
    if area.is_empty() {
        return;
    }
    Block::new()
        .border(pal_col(pal().think_border))
        .title("thinking", think_col().bold())
        .render(buf, area);
    let inner = Block::inner(area);
    if inner.is_empty() {
        return;
    }
    let inner = Rect {
        x: inner.x.saturating_add(1),
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    if inner.is_empty() {
        return;
    }
    let width = inner.width.max(1) as usize;
    let lines = think_lines(think, width);
    let h = inner.height as usize;
    let start = lines.len().saturating_sub(h);
    for (i, line) in lines[start..].iter().take(h).enumerate() {
        let y = inner.y.saturating_add(i as u16);
        let mut x = inner.x;
        for span in &line.spans {
            x = buf.write(inner, x, y, &span.text, span.style);
        }
    }
}

pub(super) fn queue_height(queue: &[Queued], room: u16) -> u16 {
    if queue.is_empty() {
        return 0;
    }
    if room < 3 {
        return 0;
    }
    let items = (queue.len() as u16).min(4);
    let extra = if queue.len() as u16 > items { 1 } else { 0 };
    items
        .saturating_add(extra)
        .saturating_add(2)
        .min(room)
        .max(3)
}

pub(super) fn queue_visible(queue: &[Queued], height: u16) -> usize {
    if queue.is_empty() || height == 0 {
        return 0;
    }
    let room = height as usize;
    let hidden = queue.len().saturating_sub(room);
    if hidden > 0 && room > 0 {
        (room - 1).min(queue.len())
    } else {
        room.min(queue.len())
    }
}

pub(super) fn queue_inner(area: Rect) -> Rect {
    let inner = Block::inner(area);
    if inner.is_empty() {
        return inner;
    }
    Rect {
        x: inner.x.saturating_add(1),
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    }
}

pub(super) fn queue_ctrl_hit(kind: QueueCtrl, row: usize) -> QueueHit {
    match kind {
        QueueCtrl::Steer => QueueHit::Steer(row),
        QueueCtrl::Earlier => QueueHit::Up(row),
        QueueCtrl::Later => QueueHit::Down(row),
        QueueCtrl::Edit => QueueHit::Edit(row),
        QueueCtrl::Remove => QueueHit::Drop(row),
    }
}

pub(super) fn queue_ctrl_width() -> u16 {
    let mut w = 0usize;
    for (i, (label, _)) in QUEUE_CTRLS.iter().enumerate() {
        w = w.saturating_add(if i == 0 { 1 } else { 2 });
        w = w.saturating_add(width(label));
    }
    w as u16
}

pub(super) fn queue_ctrl_origin(inner: Rect) -> u16 {
    inner.right().saturating_sub(queue_ctrl_width())
}

pub(super) fn queue_hit(area: Rect, queue: &[Queued], x: u16, y: u16) -> Option<QueueHit> {
    let inner = queue_inner(area);
    if !inner.contains(x, y) || queue.is_empty() {
        return None;
    }
    let end = queue_visible(queue, inner.height);
    let row = y.saturating_sub(inner.y) as usize;
    if row >= end {
        return None;
    }
    let mut cx = queue_ctrl_origin(inner).saturating_add(1);
    for (i, (label, kind)) in QUEUE_CTRLS.iter().enumerate() {
        if i > 0 {
            cx = cx.saturating_add(2);
        }
        let w = width(label) as u16;
        if x >= cx && x < cx.saturating_add(w) {
            return Some(queue_ctrl_hit(*kind, row));
        }
        cx = cx.saturating_add(w);
    }
    Some(QueueHit::Drag(row))
}

pub(super) fn notice_strip(
    buf: &mut Buffer,
    area: Rect,
    label: &str,
    label_style: Style,
    items: &[String],
) {
    if area.is_empty() || items.is_empty() {
        return;
    }
    let y = area.y;
    let mut x = area.x.saturating_add(1);
    x = buf.write(area, x, y, &format!("{label}  "), label_style);
    write_notice_line(buf, area, x, y, items, label_style);
}

pub(super) fn notice_height(items: &[String], room: u16) -> u16 {
    if items.is_empty() || room < 3 {
        0
    } else {
        3.min(room)
    }
}

pub(super) fn write_notice_line(
    buf: &mut Buffer,
    area: Rect,
    x: u16,
    y: u16,
    items: &[String],
    count_style: Style,
) {
    let preview = items[0].replace('\n', " ");
    let n = items.len();
    let count = if n > 1 {
        format!("  ×{n}")
    } else {
        String::new()
    };
    let count_w = width(&count);
    let max = area
        .right()
        .saturating_sub(x)
        .saturating_sub(count_w as u16)
        .saturating_sub(1) as usize;
    buf.write(area, x, y, &clip_width(&preview, max), pal_col(pal().text));
    if !count.is_empty() {
        let cx = area
            .right()
            .saturating_sub(count_w as u16)
            .saturating_sub(1);
        if cx > x {
            buf.write(area, cx, y, &count, count_style);
        }
    }
}

pub(super) fn notice_panel(
    buf: &mut Buffer,
    area: Rect,
    title: &str,
    border: Style,
    title_style: Style,
    items: &[String],
) {
    if area.is_empty() || items.is_empty() {
        return;
    }
    Block::new()
        .border(border)
        .title(title, title_style)
        .render(buf, area);
    let inner = Block::inner(area);
    if inner.is_empty() {
        return;
    }
    let inner = Rect {
        x: inner.x.saturating_add(1),
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    if inner.is_empty() {
        return;
    }
    write_notice_line(buf, inner, inner.x, inner.y, items, title_style);
}

pub(super) fn queue_panel(
    buf: &mut Buffer,
    area: Rect,
    queue: &[Queued],
    highlight: Option<usize>,
    editing: Option<(usize, &str)>,
) {
    if area.is_empty() || queue.is_empty() {
        return;
    }
    Block::new()
        .border(muted())
        .title("queue", accent().bold())
        .render(buf, area);
    let inner = queue_inner(area);
    if inner.is_empty() {
        return;
    }
    let end = queue_visible(queue, inner.height);
    let n = queue.len();
    for (i, item) in queue.iter().enumerate().take(end) {
        let y = inner.y.saturating_add(i as u16);
        if y >= inner.bottom() {
            break;
        }
        let editing_here = editing.is_some_and(|(j, _)| j == i);
        let hot = highlight == Some(i) || editing_here;
        if hot {
            let mut x = inner.x;
            while x < inner.right() {
                buf.put(x, y, ' ', Style::new().bg(rgb_color(pal().queue)));
                x = x.saturating_add(1);
            }
        }
        let num = if editing_here {
            user_col().bold().bg(rgb_color(pal().queue))
        } else if hot {
            tool_col().bold().bg(rgb_color(pal().queue))
        } else {
            muted()
        };
        let text = if hot {
            pal_col(pal().text).bold().bg(rgb_color(pal().queue))
        } else {
            pal_col(pal().text)
        };
        let mut x = inner.x;
        x = buf.write(inner, x, y, &format!("{}. ", i + 1), num);
        let ctrl_w = queue_ctrl_width();
        let max = inner.right().saturating_sub(x).saturating_sub(ctrl_w) as usize;
        let live = editing
            .filter(|(j, _)| *j == i)
            .map(|(_, t)| t)
            .filter(|t| !t.trim().is_empty());
        let preview = live.unwrap_or(&item.text).replace('\n', " ");
        buf.write(inner, x, y, &clip_width(&preview, max), text);
        let mut cx = queue_ctrl_origin(inner);
        let earlier_style = if i == 0 { muted() } else { tool_col() };
        let later_style = if i + 1 == n { muted() } else { tool_col() };
        for (k, (label, kind)) in QUEUE_CTRLS.iter().enumerate() {
            let gap = if k == 0 { " " } else { "  " };
            let gap_style = if hot {
                muted().bg(rgb_color(pal().queue))
            } else {
                muted()
            };
            cx = buf.write(inner, cx, y, gap, gap_style);
            let style = match kind {
                QueueCtrl::Steer => accent(),
                QueueCtrl::Earlier => earlier_style,
                QueueCtrl::Later => later_style,
                QueueCtrl::Edit => user_col(),
                QueueCtrl::Remove => err_col(),
            };
            let style = if hot {
                style.bold().bg(rgb_color(pal().queue))
            } else {
                style
            };
            cx = buf.write(inner, cx, y, label, style);
        }
    }
    if n > end {
        let y = inner.y.saturating_add(end as u16);
        if y < inner.bottom() {
            buf.write(
                inner,
                inner.x,
                y,
                &format!("+{} more", n - end),
                muted().italic(),
            );
        }
    }
}

pub(super) fn link_at(
    rows: &[Line],
    partial: &str,
    working: bool,
    area: Rect,
    x: u16,
    y: u16,
    body_start: usize,
) -> Option<String> {
    if !area.contains(x, y) {
        return None;
    }
    let wrap_w = area.width.saturating_sub(2).max(1) as usize;
    let lines = wrap_body(rows, partial, working, wrap_w);
    let row = body_start + y.saturating_sub(area.top()) as usize;
    let line = lines.get(row)?;
    let origin = area.x.saturating_add(1);
    if x < origin {
        return None;
    }
    let mut col = origin;
    for span in &line.spans {
        let w = width(&span.text) as u16;
        let end = col.saturating_add(w);
        if x >= col && (x < end || w == 0 && x == col) {
            return span.link.as_deref().and_then(http_url).map(str::to_string);
        }
        col = end;
    }
    None
}

pub(super) fn wrap_body(rows: &[Line], partial: &str, working: bool, width: usize) -> Vec<Line> {
    let mut wrapped = rows.to_vec();
    if !partial.is_empty() {
        if !wrapped.is_empty() {
            wrapped.push(Line::empty());
        }
        let body_w = width.saturating_sub(2).max(1);
        let mut plain = render_plain(partial);
        if working {
            if let Some(last) = plain.last_mut() {
                last.spans.push(Span::new("▍", agent_col().dim()));
            } else {
                plain.push(Line::plain("▍", agent_col().dim()));
            }
        }
        wrapped.extend(indent_lines(wrap_lines(&plain, body_w), "  "));
    } else if wrapped.is_empty() {
        wrapped.push(Line::plain(
            "ask anything about this workspace",
            muted().italic(),
        ));
    }
    wrapped
}

pub(super) fn selected_from_lines(
    lines: &[Line],
    select: ((u16, usize), (u16, usize)),
    origin_x: u16,
    right: u16,
) -> String {
    let (a, b) = order_sel(select.0, select.1);
    if a == b {
        return String::new();
    }
    let mut out = String::new();
    for row in a.1..=b.1 {
        if row >= lines.len() {
            break;
        }
        if row > a.1 {
            out.push('\n');
        }
        let x0 = if row == a.1 { a.0 } else { origin_x };
        let x1 = if row == b.1 {
            b.0
        } else {
            right.saturating_sub(1)
        };
        out.push_str(slice_line(&lines[row], origin_x, x0, x1).trim_end());
    }
    out.trim_end().to_string()
}

pub(super) fn slice_line(line: &Line, origin_x: u16, x0: u16, x1: u16) -> String {
    let mut out = String::new();
    let mut x = origin_x;
    for span in &line.spans {
        for c in span.text.chars() {
            let w = c.width().unwrap_or(0) as u16;
            if w == 0 {
                continue;
            }
            if x >= x0 && x <= x1 {
                out.push(c);
            }
            x = x.saturating_add(w);
            if x > x1 {
                break;
            }
        }
    }
    out
}

pub(super) fn body_window(len: usize, height: usize, follow: bool, start: usize) -> (usize, usize) {
    let max_start = len.saturating_sub(height);
    let start = if follow {
        max_start
    } else {
        start.min(max_start)
    };
    (max_start.saturating_sub(start), start)
}

pub(super) fn body(
    buf: &mut Buffer,
    area: Rect,
    rows: &[Line],
    partial: &str,
    working: bool,
    follow: bool,
    start: usize,
) -> (usize, usize) {
    if area.is_empty() {
        return (0, 0);
    }
    let width = area.width.saturating_sub(2).max(1) as usize;
    let wrapped = wrap_body(rows, partial, working, width);
    let h = area.height as usize;
    let (scroll, start) = body_window(wrapped.len(), h, follow, start);
    let x = area.x.saturating_add(1);
    for (i, line) in wrapped[start..].iter().take(h).enumerate() {
        let y = area.y.saturating_add(i as u16);
        if y >= area.bottom() {
            break;
        }
        let mut cx = x;
        for span in &line.spans {
            cx = buf.write(area, cx, y, &span.text, span.style);
        }
    }
    (scroll, start)
}

pub(super) struct ComposerPiece {
    pub(super) text: String,
    pub(super) style: Style,
    pub(super) chip: Option<usize>,
}

pub(super) struct ComposerLine {
    pub(super) pieces: Vec<ComposerPiece>,
    pub(super) width: usize,
}

pub(super) struct ComposerView {
    pub(super) lines: Vec<ComposerLine>,
    pub(super) cursor_line: usize,
    pub(super) cursor_col: usize,
}

pub(super) fn chip_text(label: &str, max: usize) -> String {
    let max = max.max(5);
    let budget = max.saturating_sub(4);
    format!("[{} ×]", clip_width(label, budget))
}

pub(super) fn composer_view(atoms: &[ComposerAtom], cursor: usize, max: usize) -> ComposerView {
    let max = max.max(1);
    let cursor = cursor.min(atoms.len());
    let mut lines = vec![ComposerLine {
        pieces: Vec::new(),
        width: 0,
    }];
    let mut cursor_line = 0usize;
    let mut cursor_col = 0usize;

    let newline = |lines: &mut Vec<ComposerLine>| {
        lines.push(ComposerLine {
            pieces: Vec::new(),
            width: 0,
        });
    };
    let mark = |lines: &[ComposerLine], cursor_line: &mut usize, cursor_col: &mut usize| {
        *cursor_line = lines.len().saturating_sub(1);
        *cursor_col = lines.last().map(|l| l.width).unwrap_or(0);
    };

    if cursor == 0 {
        cursor_line = 0;
        cursor_col = 0;
    }

    for (i, atom) in atoms.iter().enumerate() {
        if cursor == i {
            mark(&lines, &mut cursor_line, &mut cursor_col);
        }
        match atom {
            ComposerAtom::Char('\n') => newline(&mut lines),
            ComposerAtom::Char(c) => {
                let cw = c.width().unwrap_or(0);
                if cw == 0 {
                    continue;
                }
                if lines
                    .last()
                    .is_some_and(|l| l.width + cw > max && l.width > 0)
                {
                    newline(&mut lines);
                }
                let Some(line) = lines.last_mut() else {
                    continue;
                };
                let ch = c.to_string();
                if let Some(last) = line.pieces.last_mut()
                    && last.chip.is_none()
                    && last.style == Style::new()
                {
                    last.text.push(*c);
                } else {
                    line.pieces.push(ComposerPiece {
                        text: ch,
                        style: Style::new(),
                        chip: None,
                    });
                }
                line.width += cw;
            }
            ComposerAtom::Chip { index, label } => {
                let text = chip_text(label, max);
                let tw = width(&text).min(max).max(1);
                if lines
                    .last()
                    .is_some_and(|l| l.width + tw > max && l.width > 0)
                {
                    newline(&mut lines);
                }
                let Some(line) = lines.last_mut() else {
                    continue;
                };
                line.pieces.push(ComposerPiece {
                    text,
                    style: pal_col(pal().text).bg(rgb_color(pal().queue)),
                    chip: Some(*index),
                });
                line.width += tw;
            }
        }
    }
    if cursor == atoms.len() {
        mark(&lines, &mut cursor_line, &mut cursor_col);
    }
    ComposerView {
        lines,
        cursor_line,
        cursor_col,
    }
}

pub(super) fn composer(
    buf: &mut Buffer,
    area: Rect,
    atoms: &[ComposerAtom],
    cursor: usize,
    working: bool,
) -> ((u16, u16), Vec<(Rect, usize)>) {
    let border = if working { tool_col() } else { muted() };
    Block::new().border(border).render(buf, area);
    let inner = Block::inner(area);
    if inner.is_empty() {
        return (
            (area.x.saturating_add(1), area.y.saturating_add(1)),
            Vec::new(),
        );
    }
    let max = inner.width.saturating_sub(4).max(1) as usize;
    let view = composer_view(atoms, cursor, max);
    let h = inner.height.max(1) as usize;
    let start = (view.cursor_line + 1).saturating_sub(h);
    let mut hits = Vec::new();
    for (i, line) in view.lines.iter().enumerate().skip(start).take(h) {
        let y = inner.y.saturating_add((i - start) as u16);
        let mut x = inner.x.saturating_add(1);
        if i == 0 {
            x = buf.write(inner, x, y, "› ", accent().bold());
        } else {
            x = buf.write(inner, x, y, "  ", muted());
        }
        for piece in &line.pieces {
            let start_x = x;
            x = buf.write(inner, x, y, &piece.text, piece.style);
            if let Some(index) = piece.chip {
                let w = x.saturating_sub(start_x).max(1);
                let close_w = 3u16.min(w);
                let close_x = start_x.saturating_add(w.saturating_sub(close_w));
                hits.push((Rect::new(close_x, y, close_w, 1), index));
            }
        }
    }
    let vis = view
        .cursor_line
        .saturating_sub(start)
        .min(h.saturating_sub(1));
    let y = inner.y.saturating_add(vis as u16);
    let x = inner
        .x
        .saturating_add(1)
        .saturating_add(2)
        .saturating_add(view.cursor_col as u16);
    ((x, y), hits)
}

pub(super) fn wrap_lines(lines: &[Line], max: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for line in lines {
        out.extend(wrap_row(&line.spans, max, line.hang));
    }
    out
}

pub(super) fn wrap_row(spans: &[Span], max: usize, hang: usize) -> Vec<Line> {
    if max == 0 {
        return vec![Line::empty()];
    }
    if spans.is_empty() {
        return vec![Line::empty().with_hang(hang)];
    }

    struct Chunk {
        text: String,
        style: Style,
        link: Option<String>,
        width: usize,
        space: bool,
        newline: bool,
    }

    let mut chunks: Vec<Chunk> = Vec::new();
    for span in spans {
        for c in span.text.chars() {
            if c == '\n' {
                chunks.push(Chunk {
                    text: String::new(),
                    style: span.style,
                    link: span.link.clone(),
                    width: 0,
                    space: false,
                    newline: true,
                });
                continue;
            }
            let cw = c.width().unwrap_or(0);
            if cw == 0 {
                continue;
            }
            let space = c.is_whitespace();
            if let Some(last) = chunks.last_mut()
                && !last.newline
                && last.style == span.style
                && last.link == span.link
                && last.space == space
            {
                last.text.push(c);
                last.width += cw;
                continue;
            }
            chunks.push(Chunk {
                text: c.to_string(),
                style: span.style,
                link: span.link.clone(),
                width: cw,
                space,
                newline: false,
            });
        }
    }

    let hang = hang.min(max.saturating_sub(1));
    let mut rows: Vec<Vec<Span>> = vec![Vec::new()];
    let mut w = 0usize;
    let mut line_idx = 0usize;

    let line_max = |idx: usize| -> usize {
        if idx == 0 {
            max
        } else {
            max.saturating_sub(hang).max(1)
        }
    };

    let push_span =
        |rows: &mut Vec<Vec<Span>>, text: String, style: Style, link: Option<String>| {
            if rows.is_empty() {
                rows.push(Vec::new());
            }
            let Some(row) = rows.last_mut() else {
                return;
            };
            if let Some(prev) = row.last_mut()
                && prev.style == style
                && prev.link == link
            {
                prev.text.push_str(&text);
                return;
            }
            row.push(Span::linked(text, style, link));
        };
    let trim_trailing = |row: &mut Vec<Span>, w: &mut usize| {
        while let Some(last) = row.last_mut() {
            if last.text.is_empty() {
                row.pop();
                continue;
            }
            let trimmed = last
                .text
                .trim_end_matches(|c: char| c.is_whitespace() && c != '\n');
            if trimmed.len() == last.text.len() {
                break;
            }
            let dropped = width(&last.text[trimmed.len()..]);
            last.text.truncate(trimmed.len());
            *w = w.saturating_sub(dropped);
            if last.text.is_empty() {
                row.pop();
                continue;
            }
            break;
        }
    };
    let wrap = |rows: &mut Vec<Vec<Span>>, w: &mut usize, line_idx: &mut usize| {
        if *w == 0 {
            return;
        }
        if let Some(row) = rows.last_mut() {
            trim_trailing(row, w);
        }
        if *w == 0 {
            return;
        }
        rows.push(Vec::new());
        *line_idx += 1;
        *w = 0;
    };

    for chunk in chunks {
        if chunk.newline {
            rows.push(Vec::new());
            line_idx += 1;
            w = 0;
            continue;
        }
        let max_here = line_max(line_idx);
        if chunk.space {
            if w == 0 {
                // Keep indent on the original line; drop leading space after wrap.
                if line_idx == 0 {
                    push_span(&mut rows, chunk.text, chunk.style, chunk.link.clone());
                    w += chunk.width;
                }
                continue;
            }
            if w + chunk.width > max_here {
                wrap(&mut rows, &mut w, &mut line_idx);
                continue;
            }
            push_span(&mut rows, chunk.text, chunk.style, chunk.link.clone());
            w += chunk.width;
            continue;
        }
        if w > 0 && w + chunk.width > max_here {
            wrap(&mut rows, &mut w, &mut line_idx);
        }
        let max_here = line_max(line_idx);
        if chunk.width <= max_here {
            push_span(&mut rows, chunk.text, chunk.style, chunk.link.clone());
            w += chunk.width;
            continue;
        }
        wrap(&mut rows, &mut w, &mut line_idx);
        let mut buf = String::new();
        let mut buf_w = 0usize;
        let mut idx = line_idx;
        for c in chunk.text.chars() {
            let cw = c.width().unwrap_or(0);
            let max_here = line_max(idx);
            if buf_w + cw > max_here && buf_w > 0 {
                push_span(
                    &mut rows,
                    std::mem::take(&mut buf),
                    chunk.style,
                    chunk.link.clone(),
                );
                rows.push(Vec::new());
                idx += 1;
                buf_w = 0;
            }
            buf.push(c);
            buf_w += cw;
        }
        line_idx = idx;
        w = buf_w;
        if !buf.is_empty() {
            push_span(&mut rows, buf, chunk.style, chunk.link.clone());
        }
    }

    rows.into_iter()
        .enumerate()
        .map(|(i, mut spans)| {
            while let Some(last) = spans.last_mut() {
                let trimmed = last.text.trim_end_matches(|c: char| c.is_whitespace());
                if trimmed.len() == last.text.len() {
                    break;
                }
                last.text.truncate(trimmed.len());
                if last.text.is_empty() {
                    spans.pop();
                    continue;
                }
                break;
            }
            let mut line = Line::spans(spans);
            if i > 0 && hang > 0 && !line.spans.is_empty() {
                let mut spans = Vec::with_capacity(line.spans.len() + 1);
                spans.push(Span::new(" ".repeat(hang), Style::new()));
                spans.extend(line.spans);
                line.spans = spans;
            }
            line.hang = hang;
            line
        })
        .collect()
}

pub(super) fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub(super) fn clip_width(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if width(s) <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > keep {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

pub(super) fn visible_input(input: &str, cursor: usize, max: usize) -> (String, usize) {
    let cursor = {
        let mut c = cursor.min(input.len());
        while c > 0 && !input.is_char_boundary(c) {
            c -= 1;
        }
        c
    };
    let prefix = &input[..cursor];
    let prefix_w = width(prefix);
    if width(input) <= max {
        return (input.to_string(), prefix_w);
    }
    if prefix_w <= max {
        let mut out = String::new();
        let mut w = 0usize;
        for c in input.chars() {
            let cw = c.width().unwrap_or(0);
            if w + cw > max {
                break;
            }
            out.push(c);
            w += cw;
        }
        return (out, prefix_w);
    }
    let keep = max.saturating_sub(1);
    let mut tail = String::new();
    let mut w = 0usize;
    for c in prefix.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if w + cw > keep {
            break;
        }
        tail.insert(0, c);
        w += cw;
    }
    let shown = format!("…{tail}");
    let col = width(&shown).min(max);
    (shown, col)
}

pub(super) fn expand_tabs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize;
    for c in s.chars() {
        if c == '\t' {
            let n = 8 - (col % 8);
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else if c == '\n' {
            out.push('\n');
            col = 0;
        } else {
            out.push(c);
            col += c.width().unwrap_or(0);
        }
    }
    out
}

pub(super) fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\n' | '\t' => out.push(c),
            '\r' => {
                if it.peek() != Some(&'\n') {
                    out.push('\n');
                }
            }
            '\x1b' => match it.peek().copied() {
                Some('[') => {
                    it.next();
                    for x in it.by_ref() {
                        if x.is_ascii_alphabetic() || x == '~' {
                            break;
                        }
                    }
                }
                Some(']') => {
                    it.next();
                    while let Some(x) = it.next() {
                        if x == '\x07' {
                            break;
                        }
                        if x == '\x1b' {
                            let _ = it.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    it.next();
                }
                None => {}
            },
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}
