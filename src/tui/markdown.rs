use super::*;
use crate::ui::Style;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

pub(super) fn blank_after(out: &mut Vec<Line>) {
    if out.last().is_some_and(|l| !l.spans.is_empty()) {
        out.push(Line::empty());
    }
}

pub(super) fn render_plain(text: &str) -> Vec<Line> {
    expand_tabs(text)
        .split('\n')
        .map(|line| Line::plain(line.to_string(), Style::new()))
        .collect()
}

pub(super) fn next_http_url(s: &str) -> Option<(usize, usize, &str)> {
    let mut i = 0usize;
    while i < s.len() {
        let rest = &s[i..];
        let prefix = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            i += rest.chars().next()?.len_utf8();
            continue;
        };
        let mut end = i + prefix;
        for c in s[end..].chars() {
            if c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'') {
                break;
            }
            end += c.len_utf8();
        }
        let mut trimmed = end;
        while trimmed > i + prefix {
            let Some(last) = s[..trimmed].chars().last() else {
                break;
            };
            if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}') {
                trimmed -= last.len_utf8();
            } else {
                break;
            }
        }
        if trimmed > i + prefix
            && let Some(url) = http_url(&s[i..trimmed])
        {
            return Some((i, trimmed, url));
        }
        i += prefix;
    }
    None
}

pub(super) fn http_url(url: &str) -> Option<&str> {
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }
    if url.bytes().any(|b| b < b' ' || b == b'\x7f') {
        return None;
    }
    Some(url)
}

pub fn open_url(url: &str) -> bool {
    let Some(url) = http_url(url) else {
        return false;
    };
    #[cfg(target_os = "macos")]
    let bin = "open";
    #[cfg(target_os = "windows")]
    let bin = "cmd";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let bin = "xdg-open";
    let mut cmd = std::process::Command::new(bin);
    #[cfg(target_os = "windows")]
    {
        cmd.args(["/C", "start", "", url]);
    }
    #[cfg(not(target_os = "windows"))]
    {
        cmd.arg(url);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

pub(super) fn md_options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

#[derive(Clone, Copy)]
pub(super) enum Align {
    Left,
    Center,
    Right,
}

pub(super) struct Table {
    rows: Vec<Vec<Vec<Span>>>,
    align: Vec<Align>,
}

pub(super) struct ListState {
    ordered: bool,
    next: u64,
}

pub(super) struct Md {
    out: Vec<Line>,
    width: usize,
    spans: Vec<Span>,
    style: Style,
    style_stack: Vec<Style>,
    link: Option<String>,
    lists: Vec<ListState>,
    item_marker: Option<String>,
    quote: usize,
    code_lang: Option<String>,
    code_buf: Vec<String>,
    code_cur: String,
    table: Option<Table>,
    row: Vec<Vec<Span>>,
    cell: Vec<Span>,
    in_cell: bool,
}

impl Md {
    fn new(width: usize) -> Self {
        Self {
            out: Vec::new(),
            width,
            spans: Vec::new(),
            style: Style::new(),
            style_stack: Vec::new(),
            link: None,
            lists: Vec::new(),
            item_marker: None,
            quote: 0,
            code_lang: None,
            code_buf: Vec::new(),
            code_cur: String::new(),
            table: None,
            row: Vec::new(),
            cell: Vec::new(),
            in_cell: false,
        }
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(self.style);
        self.style = style;
    }

    fn pop_style(&mut self) {
        if let Some(style) = self.style_stack.pop() {
            self.style = style;
        }
    }

    fn dest(&mut self) -> &mut Vec<Span> {
        if self.in_cell {
            &mut self.cell
        } else {
            &mut self.spans
        }
    }

    fn push_text(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        let link = self.link.clone();
        let dest = self.dest();
        if let Some(last) = dest.last_mut()
            && last.style == style
            && last.link == link
        {
            last.text.push_str(text);
            return;
        }
        dest.push(Span::linked(text, style, link));
    }

    fn push_markdown_text(&mut self, text: &str) {
        if self.link.is_some() {
            self.push_text(text, self.style);
            return;
        }
        let mut rest = text;
        while !rest.is_empty() {
            if let Some((start, end, url)) = next_http_url(rest) {
                if start > 0 {
                    self.push_text(&rest[..start], self.style);
                }
                let prev = self.link.clone();
                self.link = Some(url.to_string());
                self.push_text(url, self.style.underline());
                self.link = prev;
                rest = &rest[end..];
            } else {
                self.push_text(rest, self.style);
                break;
            }
        }
    }

    fn indent(&self) -> usize {
        self.quote * 2 + self.lists.len().saturating_sub(1) * 2
    }

    fn quote_prefix(&self) -> Vec<Span> {
        if self.quote == 0 {
            return Vec::new();
        }
        vec![Span::new("│ ".repeat(self.quote), muted())]
    }

    fn flush_line(&mut self) {
        if self.in_cell {
            return;
        }
        let mut spans = std::mem::take(&mut self.spans);
        let empty = spans.iter().all(|s| s.text.trim().is_empty());
        let mut hang = 0usize;
        let mut prefix = self.quote_prefix();
        if let Some(marker) = self.item_marker.take() {
            let indent = self.indent();
            hang = indent + width(&marker);
            if indent > 0 {
                prefix.push(Span::new(" ".repeat(indent), Style::new()));
            }
            prefix.push(Span::new(marker, pal_col(pal().text)));
        } else if !self.lists.is_empty() {
            hang = self.indent() + 2;
            let pad = self.indent() + 2;
            if pad > 0 {
                prefix.push(Span::new(" ".repeat(pad), Style::new()));
            }
        } else if self.quote > 0 {
            hang = self.quote * 2;
        }
        if empty && prefix.iter().all(|s| s.text.trim().is_empty()) {
            return;
        }
        prefix.append(&mut spans);
        self.out.push(Line::spans(prefix).with_hang(hang));
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.code_lang.is_some() {
                    self.push_code(&text);
                } else {
                    self.push_markdown_text(&text);
                }
            }
            Event::Code(code) => self.push_text(&code, code_col().bold()),
            Event::SoftBreak => {
                if self.code_lang.is_some() {
                    self.push_code("\n");
                } else {
                    self.push_text(" ", self.style);
                }
            }
            Event::HardBreak => {
                if self.code_lang.is_some() {
                    self.push_code("\n");
                } else {
                    self.flush_line();
                }
            }
            Event::Rule => {
                self.flush_line();
                blank_after(&mut self.out);
                let n = self.width.clamp(3, 40);
                self.out
                    .push(Line::plain("─".repeat(n), muted()));
                blank_after(&mut self.out);
            }
            Event::TaskListMarker(checked) => {
                let mark = if checked { "✓ " } else { "• " };
                self.item_marker = Some(mark.to_string());
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                let t = html.trim();
                if !t.is_empty() && self.code_lang.is_none() {
                    self.push_text(t, muted());
                }
            }
            Event::FootnoteReference(name) => {
                self.push_text(&format!("[{name}]"), muted());
            }
            Event::InlineMath(m) | Event::DisplayMath(m) => {
                self.push_text(&m, code_col());
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph
            | Tag::HtmlBlock
            | Tag::MetadataBlock(_)
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript => {}
            Tag::Heading { level, .. } => {
                self.flush_line();
                blank_after(&mut self.out);
                let style = match level as u8 {
                    1 => pal_col(pal().text).bold().underline(),
                    2 => accent().bold(),
                    _ => pal_col(pal().text).bold(),
                };
                self.push_style(style);
            }
            Tag::BlockQuote(_) => {
                self.flush_line();
                self.quote += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush_line();
                blank_after(&mut self.out);
                self.code_lang = Some(match kind {
                    CodeBlockKind::Fenced(lang) => lang.trim().to_string(),
                    CodeBlockKind::Indented => String::new(),
                });
                self.code_buf.clear();
                self.code_cur.clear();
            }
            Tag::List(start) => {
                self.flush_line();
                if self.lists.is_empty() {
                    blank_after(&mut self.out);
                }
                self.lists.push(ListState {
                    ordered: start.is_some(),
                    next: start.unwrap_or(1),
                });
            }
            Tag::Item => {
                self.flush_line();
                let marker = if let Some(list) = self.lists.last_mut() {
                    if list.ordered {
                        let n = list.next;
                        list.next += 1;
                        format!("{n}. ")
                    } else {
                        "• ".to_string()
                    }
                } else {
                    "• ".to_string()
                };
                self.item_marker = Some(marker);
            }
            Tag::Table(aligns) => {
                self.flush_line();
                blank_after(&mut self.out);
                self.table = Some(Table {
                    rows: Vec::new(),
                    align: aligns
                        .iter()
                        .map(|a| match a {
                            pulldown_cmark::Alignment::Center => Align::Center,
                            pulldown_cmark::Alignment::Right => Align::Right,
                            _ => Align::Left,
                        })
                        .collect(),
                });
                self.row.clear();
            }
            Tag::TableHead | Tag::TableRow => {
                self.row.clear();
            }
            Tag::TableCell => {
                self.in_cell = true;
                self.cell.clear();
            }
            Tag::Emphasis => self.push_style(self.style.italic()),
            Tag::Strong => self.push_style(self.style.bold()),
            Tag::Strikethrough => self.push_style(self.style.strike()),
            Tag::Link { dest_url, .. } => {
                self.link = Some(dest_url.to_string());
                self.push_style(self.style.underline());
            }
            Tag::Image { dest_url, .. } => {
                self.push_text(&format!("[{dest_url}]"), muted());
            }
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Item => self.flush_line(),
            TagEnd::Heading(_) => {
                self.flush_line();
                self.pop_style();
                blank_after(&mut self.out);
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.quote = self.quote.saturating_sub(1);
                if self.quote == 0 {
                    blank_after(&mut self.out);
                }
            }
            TagEnd::CodeBlock => {
                if !self.code_cur.is_empty() {
                    self.code_buf.push(std::mem::take(&mut self.code_cur));
                }
                let lang = self.code_lang.take().unwrap_or_default();
                let lines = std::mem::take(&mut self.code_buf);
                self.out.extend(code_block(&lines, &lang, self.width));
                blank_after(&mut self.out);
            }
            TagEnd::List(_) => {
                self.flush_line();
                self.lists.pop();
                if self.lists.is_empty() {
                    blank_after(&mut self.out);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.out.extend(render_table(&table, self.width));
                    blank_after(&mut self.out);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.rows.push(std::mem::take(&mut self.row));
                }
            }
            TagEnd::TableCell => {
                self.in_cell = false;
                self.row.push(std::mem::take(&mut self.cell));
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.link = None;
                self.pop_style();
            }
            TagEnd::Image
            | TagEnd::HtmlBlock
            | TagEnd::MetadataBlock(_)
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript => {}
        }
    }

    fn push_code(&mut self, text: &str) {
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                self.code_buf.push(std::mem::take(&mut self.code_cur));
            }
            self.code_cur.push_str(part);
        }
    }
}

pub(super) fn render_md(text: &str, width: usize) -> Vec<Line> {
    let text = expand_tabs(text);
    let mut md = Md::new(width.max(1));
    for event in Parser::new_ext(&text, md_options()) {
        md.event(event);
    }
    md.flush_line();
    while md.out.last().is_some_and(|l| l.spans.is_empty()) {
        md.out.pop();
    }
    md.out
}

pub(super) fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| width(&s.text)).sum()
}

pub(super) fn truncate_spans(spans: &[Span], max: usize) -> Vec<Span> {
    if max == 0 {
        return Vec::new();
    }
    if spans_width(spans) <= max {
        return spans.to_vec();
    }
    let mut out = Vec::new();
    let mut used = 0usize;
    for span in spans {
        if used >= max {
            break;
        }
        let mut text = String::new();
        for c in span.text.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw > max {
                break;
            }
            text.push(c);
            used += cw;
        }
        if !text.is_empty() {
            out.push(Span::linked(text, span.style, span.link.clone()));
        }
        if used >= max {
            break;
        }
    }
    out
}

pub(super) fn fit_table_widths(widths: &mut [usize], max: usize) {
    let n = widths.len();
    if n == 0 {
        return;
    }
    let overhead = 2 * n + n.saturating_sub(1);
    let avail = max.max(1).saturating_sub(overhead).max(n);
    let mut total: usize = widths.iter().sum();
    while total > avail {
        let Some((idx, _)) = widths.iter().enumerate().max_by_key(|(_, w)| **w) else {
            break;
        };
        if widths[idx] <= 1 {
            break;
        }
        widths[idx] -= 1;
        total -= 1;
    }
}

pub(super) fn render_table(table: &Table, max: usize) -> Vec<Line> {
    let n = table.align.len();
    if n == 0 || table.rows.is_empty() {
        return Vec::new();
    }
    let mut widths = vec![1usize; n];
    for row in &table.rows {
        for (i, cell) in row.iter().enumerate().take(n) {
            widths[i] = widths[i].max(spans_width(cell).max(1));
        }
    }
    fit_table_widths(&mut widths, max);

    let mut out = Vec::with_capacity(table.rows.len() + 1);
    for (r, row) in table.rows.iter().enumerate() {
        if r == 1 {
            out.push(table_sep(&widths));
        }
        out.push(table_row(row, &widths, &table.align, r == 0));
    }
    out
}

pub(super) fn table_sep(widths: &[usize]) -> Line {
    let mut s = String::new();
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            s.push('┼');
        }
        s.extend(std::iter::repeat_n('─', w + 2));
    }
    Line::plain(s, muted())
}

pub(super) fn table_row(row: &[Vec<Span>], widths: &[usize], align: &[Align], header: bool) -> Line {
    let mut spans = Vec::new();
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            spans.push(Span::new("│", muted()));
        }
        let mut inner = truncate_spans(row.get(i).map(Vec::as_slice).unwrap_or(&[]), *w);
        if header {
            for span in &mut inner {
                span.style = span.style.bold();
            }
        }
        let pad = w.saturating_sub(spans_width(&inner));
        let (left, right) = match align.get(i).copied().unwrap_or(Align::Left) {
            Align::Left => (0, pad),
            Align::Right => (pad, 0),
            Align::Center => (pad / 2, pad - pad / 2),
        };
        spans.push(Span::new(" ", Style::new()));
        if left > 0 {
            spans.push(Span::new(" ".repeat(left), Style::new()));
        }
        spans.extend(inner);
        if right > 0 {
            spans.push(Span::new(" ".repeat(right), Style::new()));
        }
        spans.push(Span::new(" ", Style::new()));
    }
    Line::spans(spans)
}

pub(super) fn code_block(lines: &[String], lang: &str, max: usize) -> Vec<Line> {
    let code = code_col();
    let mut out = Vec::new();
    if !lang.is_empty() {
        out.push(Line::plain(format!(" {lang}"), muted().italic()));
    }
    if lines.is_empty() {
        out.push(Line::plain(String::new(), code));
    } else {
        for line in lines {
            let mut s = format!(" {line}");
            while width(&s) > max && !s.is_empty() {
                s.pop();
            }
            out.push(Line::plain(s, code));
        }
    }
    out
}

