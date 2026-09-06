//! Fun coding agent chrome on top of the local UI kit.
//!
//! Layout:
//!   body      scrollback (user, agent, tools)
//!   think     live reasoning (boxed)
//!   interrupt stop-now prompts (one-line strip)
//!   steer     after-this-step prompts (boxed)
//!   queue     idle prompts for after this turn (boxed)
//!   actions   pull / commit chips above the composer
//!   composer
//!   status    tokens / model / keys
//!
//! Keys while a turn is running:
//!   Enter       queue (after the turn)
//!   Ctrl+Enter  interrupt (stop now and inject the composer)
//!   Esc         abort, then send from the composer
//!   click send now / edit / move up / move down / cancel  on a queued prompt


use fun_core::agent::{tool_counts, tool_summary, ToolRun};
use fun_core::config::{Action, ActionColor, ActionWhen, Palette};
use crate::ui::{
    split, Block, Buffer, Color, Constraint, Rect, Style, Terminal,
};
use anyhow::Result;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::cell::Cell;
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const THINK_HOLD: Duration = Duration::from_secs(2);
const QUEUE_FLASH: Duration = Duration::from_millis(1200);
const COPY_FLASH: Duration = Duration::from_millis(1400);

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

thread_local! {
    static PALETTE: Cell<Palette> = Cell::new(Palette::default());
    static ACTIONS: std::cell::RefCell<Vec<Action>> = std::cell::RefCell::new(Action::defaults());
    static GIT: Cell<bool> = const { Cell::new(false) };
    static ORIGIN: Cell<bool> = const { Cell::new(false) };
    static HOME: std::cell::RefCell<String> = std::cell::RefCell::new(String::from("master"));
}

fn pal() -> Palette {
    PALETTE.with(Cell::get)
}

pub fn set_palette(p: Palette) {
    PALETTE.with(|c| c.set(p));
}

pub fn set_actions(actions: Vec<Action>) {
    ACTIONS.with(|c| *c.borrow_mut() = actions);
}

pub fn configured_action(index: usize) -> Option<Action> {
    ACTIONS.with(|c| c.borrow().get(index).cloned())
}

pub fn set_git(present: bool) {
    GIT.with(|c| c.set(present));
}

pub fn set_origin(present: bool) {
    ORIGIN.with(|c| c.set(present));
}

pub fn set_home(home: String) {
    HOME.with(|c| *c.borrow_mut() = home);
}

pub fn configured_home() -> String {
    HOME.with(|c| c.borrow().clone())
}

pub fn has_origin() -> bool {
    ORIGIN.with(Cell::get)
}

fn git_present() -> bool {
    GIT.with(Cell::get)
}

fn actions() -> Vec<Action> {
    ACTIONS.with(|c| c.borrow().clone())
}

fn rgb_color(c: fun_core::config::Rgb) -> Color {
    Color::Rgb { r: c.r, g: c.g, b: c.b }
}

fn col(c: Color) -> Style {
    Style::new().fg(c)
}

fn pal_col(c: fun_core::config::Rgb) -> Style {
    col(rgb_color(c))
}

fn muted() -> Style {
    pal_col(pal().muted)
}

fn accent() -> Style {
    pal_col(pal().accent)
}

fn user_col() -> Style {
    pal_col(pal().user)
}

fn agent_col() -> Style {
    pal_col(pal().agent)
}

fn tool_col() -> Style {
    pal_col(pal().tool)
}

fn err_col() -> Style {
    pal_col(pal().error)
}

fn ok_col() -> Style {
    pal_col(pal().ok)
}

fn code_col() -> Style {
    pal_col(pal().code)
}

fn think_col() -> Style {
    pal_col(pal().think).italic()
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Bar {
    pub workspace: String,
    pub branch: Option<String>,
    pub pull: Option<String>,
    pub input: String,
    pub output: String,
    pub reasoning: String,
    pub cache_hit: String,
    pub cost: String,
    pub context: String,
    pub model: String,
    pub effort: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Queued {
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueHit {
    Steer(usize),
    Up(usize),
    Down(usize),
    Edit(usize),
    Drop(usize),
    Drag(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionHit {
    pub index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerAtom {
    Char(char),
    Chip { index: usize, label: String },
}

#[derive(Clone, Copy)]
enum QueueCtrl {
    Steer,
    Earlier,
    Later,
    Edit,
    Remove,
}

const QUEUE_CTRLS: &[(&str, QueueCtrl)] = &[
    ("send now", QueueCtrl::Steer),
    ("edit", QueueCtrl::Edit),
    ("move up", QueueCtrl::Earlier),
    ("move down", QueueCtrl::Later),
    ("cancel", QueueCtrl::Remove),
];

#[derive(Clone)]
struct Span {
    text: String,
    style: Style,
}

impl Span {
    fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

#[derive(Clone)]
struct Line {
    spans: Vec<Span>,
    hang: usize,
}

impl Line {
    fn plain(text: impl Into<String>, style: Style) -> Self {
        Self {
            spans: vec![Span::new(text, style)],
            hang: 0,
        }
    }

    fn spans(spans: Vec<Span>) -> Self {
        Self { spans, hang: 0 }
    }

    fn empty() -> Self {
        Self {
            spans: Vec::new(),
            hang: 0,
        }
    }

    fn with_hang(mut self, hang: usize) -> Self {
        self.hang = hang;
        self
    }
}

enum Item {
    User(String),
    Md(String),
    Note(String),
    Tools {
        runs: Vec<ToolRun>,
        open: bool,
    },
}

pub struct Ui {
    term: Terminal,
    items: Vec<Item>,
    rows: Vec<Line>,
    wrap_w: usize,
    partial: String,
    think: String,
    think_hide_at: Option<Instant>,
    atoms: Vec<ComposerAtom>,
    cursor: usize,
    bar: Bar,
    working: bool,
    spinner: usize,
    scroll: usize,
    follow: bool,
    paused: bool,
    body_area: Rect,
    queue_area: Rect,
    composer_area: Rect,
    chip_hits: Vec<(Rect, usize)>,
    actions: Vec<(Rect, ActionHit)>,
    queue_drag: Option<usize>,
    queue_flash: Option<(usize, Instant)>,
    queue_edit: Option<usize>,
    queue_pointer: bool,
    select: Option<((u16, usize), (u16, usize))>,
    body_start: usize,
    item_rows: Vec<(usize, usize, usize)>,
    queue: Vec<Queued>,
    interrupts: Vec<String>,
    steers: Vec<String>,
    copied_until: Option<Instant>,
    ask: Option<AskDialog>,
}

struct AskDialog {
    title: String,
    template: String,
    hint: String,
    placeholder: String,
    value: String,
    cursor: usize,
}

impl Ui {
    pub fn start() -> Result<Self> {
        let mut ui = Self {
            term: Terminal::start()?,
            items: Vec::new(),
            rows: Vec::new(),
            wrap_w: 0,
            partial: String::new(),
            think: String::new(),
            think_hide_at: None,
            atoms: Vec::new(),
            cursor: 0,
            bar: Bar::default(),
            working: false,
            spinner: 0,
            scroll: 0,
            follow: true,
            paused: false,
            body_area: Rect::new(0, 0, 0, 0),
            queue_area: Rect::new(0, 0, 0, 0),
            composer_area: Rect::new(0, 0, 0, 0),
            chip_hits: Vec::new(),
            actions: Vec::new(),
            queue_drag: None,
            queue_flash: None,
            queue_edit: None,
            queue_pointer: false,
            select: None,
            body_start: 0,
            item_rows: Vec::new(),
            queue: Vec::new(),
            interrupts: Vec::new(),
            steers: Vec::new(),
            copied_until: None,
            ask: None,
        };
        ui.redraw()?;
        Ok(ui)
    }

    pub fn batch(&mut self, f: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        self.paused = true;
        let result = f(self);
        self.paused = false;
        self.follow_end();
        self.redraw()?;
        result
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.term.resize(cols, rows)?;
        self.redraw()
    }

    pub fn set_bar(&mut self, bar: Bar) -> Result<()> {
        if bar == self.bar {
            return Ok(());
        }
        self.bar = bar;
        self.redraw()
    }

    pub fn set_composer(&mut self, atoms: Vec<ComposerAtom>, cursor: usize) -> Result<()> {
        let cursor = cursor.min(atoms.len());
        if atoms == self.atoms && cursor == self.cursor {
            return Ok(());
        }
        self.atoms = atoms;
        self.cursor = cursor;
        self.redraw()
    }

    pub fn chip_hit(&self, x: u16, y: u16) -> Option<usize> {
        self.chip_hits
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, i)| *i)
    }

    pub fn set_queue(&mut self, queue: Vec<Queued>) -> Result<()> {
        if queue == self.queue {
            return Ok(());
        }
        if let Some(i) = self.queue_drag
            && i >= queue.len()
        {
            self.queue_drag = None;
        }
        if let Some((i, _)) = self.queue_flash
            && i >= queue.len()
        {
            self.queue_flash = None;
        }
        if let Some(i) = self.queue_edit
            && i >= queue.len()
        {
            self.queue_edit = None;
        }
        self.queue = queue;
        self.redraw()
    }

    pub fn set_interrupts(&mut self, interrupts: Vec<String>) -> Result<()> {
        if interrupts == self.interrupts {
            return Ok(());
        }
        self.interrupts = interrupts;
        self.redraw()
    }

    pub fn set_steers(&mut self, steers: Vec<String>) -> Result<()> {
        if steers == self.steers {
            return Ok(());
        }
        self.steers = steers;
        self.redraw()
    }

    pub fn set_working(&mut self, working: bool) -> Result<()> {
        if working == self.working {
            return Ok(());
        }
        self.working = working;
        if working {
            self.spinner = 0;
        } else {
            self.dismiss_think();
            self.flush_partial();
        }
        self.redraw()
    }

    pub fn tick(&mut self) -> Result<()> {
        let mut dirty = false;
        if self.working {
            self.spinner = (self.spinner + 1) % SPINNER.len();
            dirty = true;
        }
        if let Some(at) = self.think_hide_at
            && Instant::now() >= at
        {
            self.think.clear();
            self.think_hide_at = None;
            dirty = true;
        }
        if let Some((_, at)) = self.queue_flash
            && Instant::now() >= at
        {
            self.queue_flash = None;
            dirty = true;
        }
        if let Some(at) = self.copied_until
            && Instant::now() >= at
        {
            self.copied_until = None;
            dirty = true;
        }
        if dirty {
            self.redraw()?;
        }
        Ok(())
    }

    pub fn scroll_by(&mut self, delta: i32) -> Result<()> {
        self.follow = false;
        if delta > 0 {
            self.body_start = self.body_start.saturating_sub(delta as usize);
        } else {
            self.body_start = self.body_start.saturating_add((-delta) as usize);
        }
        self.redraw()
    }

    pub fn scroll_end(&mut self) -> Result<()> {
        if self.follow && self.scroll == 0 {
            return Ok(());
        }
        self.follow_end();
        self.redraw()
    }

    fn follow_end(&mut self) {
        self.scroll = 0;
        self.follow = true;
    }

    pub fn select_start(&mut self, x: u16, y: u16) -> Result<()> {
        if !self.in_body(x, y) {
            self.select = None;
            self.term.set_mouse_motion(false)?;
            return self.redraw();
        }
        let p = self.screen_to_sel(x, y);
        self.select = Some((p, p));
        self.term.set_mouse_motion(true)?;
        self.redraw()
    }

    pub fn click_tools(&mut self, x: u16, y: u16) -> Result<bool> {
        if !self.in_body(x, y) {
            return Ok(false);
        }
        let row = self.screen_to_sel(x, y).1;
        let Some(i) = self.summary_at_row(row) else {
            return Ok(false);
        };
        let Item::Tools { open, runs } = &mut self.items[i] else {
            return Ok(false);
        };
        if runs.is_empty() {
            return Ok(false);
        }
        *open = !*open;
        self.rows.clear();
        self.select = None;
        self.redraw()?;
        Ok(true)
    }

    pub fn select_drag(&mut self, x: u16, y: u16) -> Result<()> {
        if self.queue_pointer {
            return Ok(());
        }
        let Some((start, _)) = self.select else {
            return Ok(());
        };
        let p = self.screen_to_sel(x, y);
        if self.select == Some((start, p)) {
            return Ok(());
        }
        self.select = Some((start, p));
        self.redraw()
    }

    pub fn select_end(&mut self, x: u16, y: u16) -> Result<Option<String>> {
        self.term.set_mouse_motion(false)?;
        if self.take_queue_pointer() {
            return Ok(None);
        }
        self.select_drag(x, y)?;
        Ok(self.selected_text())
    }

    pub fn is_selecting(&self) -> bool {
        self.select.is_some() && !self.queue_pointer
    }

    pub fn asking(&self) -> bool {
        self.ask.is_some()
    }

    pub fn open_ask(
        &mut self,
        title: impl Into<String>,
        template: impl Into<String>,
        hint: impl Into<String>,
        placeholder: impl Into<String>,
    ) -> Result<()> {
        self.ask = Some(AskDialog {
            title: title.into(),
            template: template.into(),
            hint: hint.into(),
            placeholder: placeholder.into(),
            value: String::new(),
            cursor: 0,
        });
        self.redraw()
    }

    pub fn ask_insert(&mut self, ch: char) -> Result<()> {
        if let Some(ask) = &mut self.ask {
            ask.value.insert(ask.cursor, ch);
            ask.cursor += ch.len_utf8();
        }
        self.redraw()
    }

    pub fn ask_backspace(&mut self) -> Result<()> {
        if let Some(ask) = &mut self.ask
            && ask.cursor > 0
        {
            let from = prev_char(ask.value.as_str(), ask.cursor);
            ask.value.replace_range(from..ask.cursor, "");
            ask.cursor = from;
        }
        self.redraw()
    }

    pub fn ask_delete(&mut self) -> Result<()> {
        if let Some(ask) = &mut self.ask
            && ask.cursor < ask.value.len()
        {
            let to = next_char(ask.value.as_str(), ask.cursor);
            ask.value.replace_range(ask.cursor..to, "");
        }
        self.redraw()
    }

    pub fn ask_left(&mut self) -> Result<()> {
        if let Some(ask) = &mut self.ask {
            ask.cursor = prev_char(ask.value.as_str(), ask.cursor);
        }
        self.redraw()
    }

    pub fn ask_right(&mut self) -> Result<()> {
        if let Some(ask) = &mut self.ask {
            ask.cursor = next_char(ask.value.as_str(), ask.cursor);
        }
        self.redraw()
    }

    pub fn close_ask(&mut self) -> Result<()> {
        self.ask = None;
        self.redraw()
    }

    pub fn take_ask(&mut self) -> Option<(String, String)> {
        let ask = self.ask.take()?;
        let value = ask.value.trim();
        if value.is_empty() {
            None
        } else {
            Some((ask.template, value.to_string()))
        }
    }

    pub fn flash_copied(&mut self) -> Result<()> {
        self.copied_until = Some(Instant::now() + COPY_FLASH);
        self.redraw()
    }

    pub fn selected_text(&self) -> Option<String> {
        let select = self.select?;
        if select.0 == select.1 {
            return None;
        }
        let width = self.body_area.width.saturating_sub(2).max(1) as usize;
        let lines = wrap_body(&self.rows, &self.partial, self.working, width);
        let origin = self.body_area.x.saturating_add(1);
        let text = selected_from_lines(&lines, select, origin, self.body_area.right());
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    fn screen_to_sel(&self, x: u16, y: u16) -> (u16, usize) {
        let a = self.body_area;
        let x = x.clamp(a.left(), a.right().saturating_sub(1));
        let y = y.clamp(a.top(), a.bottom().saturating_sub(1));
        (
            x,
            self.body_start + y.saturating_sub(a.top()) as usize,
        )
    }

    pub fn queue_hit(&self, x: u16, y: u16) -> Option<QueueHit> {
        queue_hit(self.queue_area, &self.queue, x, y)
    }

    pub fn action_hit(&self, x: u16, y: u16) -> Option<ActionHit> {
        self.actions
            .iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, hit)| *hit)
    }

    pub fn begin_queue_drag(&mut self, index: usize) {
        self.queue_pointer = true;
        if index < self.queue.len() {
            self.queue_drag = Some(index);
        }
    }

    pub fn capture_queue_pointer(&mut self) {
        self.queue_pointer = true;
        if self.select.take().is_some() {
            let _ = self.redraw();
        }
    }

    fn take_queue_pointer(&mut self) -> bool {
        let held = self.queue_pointer || self.queue_drag.is_some();
        self.queue_pointer = false;
        self.queue_drag = None;
        held
    }

    pub fn highlight_queue(&mut self, index: usize) -> Result<()> {
        if index >= self.queue.len() {
            return Ok(());
        }
        self.queue_flash = Some((index, Instant::now() + QUEUE_FLASH));
        self.redraw()
    }

    pub fn set_queue_edit(&mut self, index: Option<usize>) {
        self.queue_edit = index.filter(|&i| i < self.queue.len());
    }

    pub fn queue_drag_index(&self) -> Option<usize> {
        self.queue_drag
    }

    pub fn end_queue_drag(&mut self) {
        self.queue_drag = None;
        self.queue_pointer = false;
    }

    fn in_body(&self, x: u16, y: u16) -> bool {
        self.body_area.contains(x, y)
    }

    fn summary_at_row(&self, row: usize) -> Option<usize> {
        self.item_rows
            .iter()
            .position(|(start, summary_end, _)| row >= *start && row < *summary_end)
            .filter(|&i| matches!(self.items.get(i), Some(Item::Tools { .. })))
    }

    pub fn user(&mut self, text: &str) -> Result<()> {
        self.clear_think();
        self.flush_partial();
        self.follow_end();
        self.items.push(Item::User(sanitize(text)));
        self.rows.clear();
        self.redraw()
    }

    pub fn markdown(&mut self, text: &str) -> Result<()> {
        self.dismiss_think();
        self.flush_partial();
        let text = sanitize(text);
        if !text.trim().is_empty() {
            self.extend_md(text);
        }
        self.redraw()
    }

    pub fn note(&mut self, text: &str) -> Result<()> {
        self.push_plain(Item::Note(sanitize(text)))
    }

    pub fn tools(&mut self, runs: Vec<ToolRun>) -> Result<()> {
        let runs: Vec<ToolRun> = runs
            .into_iter()
            .filter_map(|e| {
                let name = sanitize(&e.name);
                let args = sanitize(&e.args);
                let detail = sanitize(&e.detail);
                if name.trim().is_empty() && args.trim().is_empty() && detail.trim().is_empty() {
                    None
                } else {
                    Some(ToolRun {
                        name,
                        args,
                        detail,
                        is_error: e.is_error,
                    })
                }
            })
            .collect();
        if runs.is_empty() {
            return Ok(());
        }
        if let Some(Item::Tools { runs: have, .. }) = self.items.last_mut() {
            have.extend(runs);
            self.rows.clear();
            return self.redraw();
        }
        self.push_plain(Item::Tools {
            runs,
            open: false,
        })
    }

    fn push_plain(&mut self, item: Item) -> Result<()> {
        self.dismiss_think();
        self.flush_partial();
        let empty = match &item {
            Item::Note(t) => t.trim().is_empty(),
            Item::Tools { runs, .. } if runs.is_empty() => true,
            _ => false,
        };
        if !empty {
            self.items.push(item);
            self.rows.clear();
        }
        self.redraw()
    }

    pub fn delta(&mut self, text: &str) -> Result<()> {
        self.dismiss_think();
        self.partial.push_str(&sanitize(text));
        if self.working {
            return Ok(());
        }
        self.redraw()
    }

    pub fn think(&mut self, text: &str) -> Result<()> {
        self.think_hide_at = None;
        self.think.push_str(&sanitize(text));
        if self.working {
            return Ok(());
        }
        self.redraw()
    }

    pub fn end_stream(&mut self) -> Result<()> {
        self.dismiss_think();
        self.flush_partial();
        self.redraw()
    }

    fn dismiss_think(&mut self) {
        if self.think.trim().is_empty() {
            self.think_hide_at = None;
            return;
        }
        if self.think_hide_at.is_none() {
            self.think_hide_at = Some(Instant::now() + THINK_HOLD);
        }
    }

    fn clear_think(&mut self) {
        self.think.clear();
        self.think_hide_at = None;
    }

    fn flush_partial(&mut self) {
        if self.partial.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.partial);
        if !text.trim().is_empty() {
            self.extend_md(text);
        }
    }

    fn extend_md(&mut self, text: String) {
        if let Some(Item::Md(existing)) = self.items.last_mut() {
            if !existing.ends_with('\n') && !text.starts_with('\n') {
                existing.push('\n');
            }
            existing.push_str(&text);
        } else {
            self.items.push(Item::Md(text));
        }
        self.rows.clear();
    }

    fn ensure_rows(&mut self, width: usize) {
        if self.rows.is_empty() || self.wrap_w != width {
            let (rows, map) = layout_items(&self.items, width);
            self.rows = rows;
            self.item_rows = map;
            if self.wrap_w != width {
                self.select = None;
                self.follow = true;
            }
            self.wrap_w = width;
        }
    }

    fn redraw(&mut self) -> Result<()> {
        if self.paused {
            return Ok(());
        }
        let width = self.term.size.width.saturating_sub(2).max(1) as usize;
        self.ensure_rows(width);
        let rows = &self.rows;
        let partial = &self.partial;
        let think = &self.think;
        let atoms = &self.atoms;
        let cursor = self.cursor;
        let bar = &self.bar;
        let working = self.working;
        let spinner = self.spinner;
        let scroll = self.scroll;
        let select = self.select;
        let queue = &self.queue;
        let queue_edit = self.queue_edit;
        let queue_edit_owned = queue_edit.map(|_| {
            atoms
                .iter()
                .map(|atom| match atom {
                    ComposerAtom::Char(c) => c.to_string(),
                    ComposerAtom::Chip { label, .. } => format!("[{label}]"),
                })
                .collect::<String>()
        });
        let queue_edit_text = queue_edit_owned.as_deref();
        let queue_hl = queue_edit
            .or_else(|| {
                self.queue_flash
                    .and_then(|(i, at)| (Instant::now() < at).then_some(i))
            })
            .or(self.queue_drag);
        let interrupts = &self.interrupts;
        let steers = &self.steers;
        let show_copied = self
            .copied_until
            .is_some_and(|at| Instant::now() < at);
        let follow = self.follow;
        let ask = self.ask.as_ref();
        let mut used_scroll = 0usize;
        let mut body_start = self.body_start;
        let mut body_area = Rect::new(0, 0, 0, 0);
        let mut queue_area = Rect::new(0, 0, 0, 0);
        let mut composer_area = Rect::new(0, 0, 0, 0);
        let mut chip_hits = Vec::new();
        let mut actions = Vec::new();

        self.term.draw_with_cursor(|buf, area| {
            let status_wanted = status_rows(bar, working, spinner, scroll, area.width)
                .len()
                .clamp(1, 4) as u16;
            let composer_wanted = composer_wanted_height(atoms, area.width);
            let (composer_h, status_h) =
                chrome_heights(area.height, status_wanted, composer_wanted);
            let mut room = area.height.saturating_sub(composer_h.saturating_add(status_h));
            let think_h = think_height(think, area.width, room);
            room = room.saturating_sub(think_h);
            let interrupt_h = if interrupts.is_empty() || room == 0 { 0 } else { 1 };
            room = room.saturating_sub(interrupt_h);
            let steer_h = notice_height(steers, room);
            room = room.saturating_sub(steer_h);
            let queue_h = queue_height(queue, room);
            room = room.saturating_sub(queue_h);
            let action_h = action_bar_height(
                area.width,
                bar.pull.as_deref(),
                bar.branch.as_deref(),
                room,
            );
            let mut constraints = vec![Constraint::Fill];
            if think_h > 0 {
                constraints.push(Constraint::Length(think_h));
            }
            if interrupt_h > 0 {
                constraints.push(Constraint::Length(interrupt_h));
            }
            if steer_h > 0 {
                constraints.push(Constraint::Length(steer_h));
            }
            if queue_h > 0 {
                constraints.push(Constraint::Length(queue_h));
            }
            if action_h > 0 {
                constraints.push(Constraint::Length(action_h));
            }
            if composer_h > 0 {
                constraints.push(Constraint::Length(composer_h));
            }
            if status_h > 0 {
                constraints.push(Constraint::Length(status_h));
            }
            let parts = split(area, &constraints);
            body_area = parts[0];
            (used_scroll, body_start) =
                body(buf, parts[0], rows, partial, working, follow, body_start);
            paint_select(buf, parts[0], select, body_start);
            let mut i = 1usize;
            if think_h > 0 {
                think_panel(buf, parts[i], think);
                i += 1;
            }
            if interrupt_h > 0 {
                notice_strip(buf, parts[i], "interrupt", err_col().bold(), interrupts);
                i += 1;
            }
            if steer_h > 0 {
                notice_panel(buf, parts[i], "send now", accent(), accent().bold(), steers);
                i += 1;
            }
            if queue_h > 0 {
                queue_area = parts[i];
                queue_panel(buf, parts[i], queue, queue_hl, queue_edit.zip(queue_edit_text));
                i += 1;
            }
            if action_h > 0 {
                actions = paint_action_bar(
                    buf,
                    parts[i],
                    bar.pull.as_deref(),
                    bar.branch.as_deref(),
                );
                i += 1;
            }
            let cursor_pos = if composer_h > 0 {
                composer_area = parts[i];
                let (pos, hits) = composer(buf, parts[i], atoms, cursor, working);
                chip_hits = hits;
                i += 1;
                pos
            } else {
                (area.x, area.y)
            };
            if status_h > 0 {
                status(buf, parts[i], bar, working, spinner, used_scroll);
            }
            if show_copied {
                paint_copied(buf, area, composer_area);
            }
            if let Some(ask) = ask {
                return paint_ask(buf, area, ask);
            }
            cursor_pos
        })?;
        self.body_area = body_area;
        self.queue_area = queue_area;
        self.composer_area = composer_area;
        self.chip_hits = chip_hits;
        self.actions = actions;
        self.scroll = used_scroll;
        self.body_start = body_start;
        self.follow = used_scroll == 0;
        Ok(())
    }
}

const COMPOSER_MAX: u16 = 12;

fn chrome_heights(total: u16, status_wanted: u16, composer_wanted: u16) -> (u16, u16) {
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

fn composer_text_width(area_width: u16) -> usize {
    area_width.saturating_sub(6).max(1) as usize
}

fn composer_wanted_height(atoms: &[ComposerAtom], width: u16) -> u16 {
    let lines = composer_view(atoms, 0, composer_text_width(width))
        .lines
        .len()
        .max(1) as u16;
    lines.saturating_add(2).max(3)
}

fn status_rows(
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
    let add_stat = |out: &mut Vec<(String, Style)>, label: &str, value: &str, color: fun_core::config::Rgb| {
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
    let gap = |a: usize, b: usize| -> usize {
        if a > 0 && b > 0 {
            2
        } else {
            0
        }
    };
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

fn segs_width(segs: &[(String, Style)]) -> usize {
    segs.iter().map(|(t, _)| width(t)).sum()
}

fn align_right(
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

fn wrap_status(
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

fn status(
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

fn prev_char(s: &str, mut i: usize) -> usize {
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_char(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    i += 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn fill_rect(buf: &mut Buffer, area: Rect, style: Style) {
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

fn paint_ask(buf: &mut Buffer, screen: Rect, ask: &AskDialog) -> (u16, u16) {
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
    let field_y = inner.y.saturating_add(1).min(inner.bottom().saturating_sub(1));
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

fn order_sel(
    start: (u16, usize),
    end: (u16, usize),
) -> ((u16, usize), (u16, usize)) {
    if (start.1, start.0) <= (end.1, end.0) {
        (start, end)
    } else {
        (end, start)
    }
}

fn action_items(home: Option<&str>, branch: Option<&str>) -> Vec<(String, ActionHit, ActionColor)> {
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

fn action_style(color: ActionColor) -> Style {
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

fn action_bar_height(width: u16, home: Option<&str>, branch: Option<&str>, room: u16) -> u16 {
    if room == 0 || width < 8 || action_items(home, branch).is_empty() {
        0
    } else {
        1
    }
}

fn paint_action_bar(
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

fn paint_copied(buf: &mut Buffer, screen: Rect, composer: Rect) {
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
    let anchor = if composer.is_empty() { screen } else { composer };
    let x = anchor
        .x
        .saturating_add(anchor.width.saturating_sub(w) / 2)
        .clamp(screen.x, screen.right().saturating_sub(w));
    let y = if !composer.is_empty() && composer.y >= screen.y.saturating_add(h) {
        composer.y.saturating_sub(h)
    } else if !composer.is_empty() && composer.height > 1 {
        composer.y
    } else {
        screen.bottom().saturating_sub(h.saturating_add(1)).max(screen.y)
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

fn paint_select(
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
    let vis_bot = body_start
        .saturating_add(area.height.saturating_sub(1) as usize);
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

fn is_status(item: &Item) -> bool {
    matches!(item, Item::Note(_) | Item::Tools { .. })
}

fn gap_before(prev: &Item, item: &Item) -> usize {
    if matches!((prev, item), (Item::Md(_), Item::Md(_))) || is_status(item) || is_status(prev) {
        1
    } else {
        2
    }
}

fn indent_lines(lines: Vec<Line>, pad: &str) -> Vec<Line> {
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

fn layout_items(items: &[Item], width: usize) -> (Vec<Line>, Vec<(usize, usize, usize)>) {
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

fn layout_item(item: &Item, width: usize) -> (usize, Vec<Line>) {
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
        Item::Note(text) => {
            let lines = indent_lines(wrap_lines(&[Line::plain(text.clone(), muted())], body_w), "  ");
            (lines.len(), lines)
        }
        Item::Tools { runs, open } => tool_block(runs, *open, width.max(1)),
    }
}

fn tool_summary_spans(ok: usize, fail: usize) -> Vec<Span> {
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

fn tool_block(runs: &[ToolRun], open: bool, width: usize) -> (usize, Vec<Line>) {
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
                    &[Line::plain(format!("{detail_pad}{line}"), err_col()).with_hang(detail_hang)],
                    width,
                ));
            }
        }
    }
    (summary_len, lines)
}

fn think_lines(text: &str, width: usize) -> Vec<Line> {
    let body: Vec<Line> = text
        .split('\n')
        .map(|chunk| Line::plain(chunk.to_string(), think_col()))
        .collect();
    wrap_lines(&body, width.max(1))
}

fn think_height(think: &str, width: u16, room: u16) -> u16 {
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

fn think_panel(buf: &mut Buffer, area: Rect, think: &str) {
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

fn queue_height(queue: &[Queued], room: u16) -> u16 {
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

fn queue_visible(queue: &[Queued], height: u16) -> usize {
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

fn queue_inner(area: Rect) -> Rect {
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

fn queue_ctrl_hit(kind: QueueCtrl, row: usize) -> QueueHit {
    match kind {
        QueueCtrl::Steer => QueueHit::Steer(row),
        QueueCtrl::Earlier => QueueHit::Up(row),
        QueueCtrl::Later => QueueHit::Down(row),
        QueueCtrl::Edit => QueueHit::Edit(row),
        QueueCtrl::Remove => QueueHit::Drop(row),
    }
}

fn queue_ctrl_width() -> u16 {
    let mut w = 0usize;
    for (i, (label, _)) in QUEUE_CTRLS.iter().enumerate() {
        w = w.saturating_add(if i == 0 { 1 } else { 2 });
        w = w.saturating_add(width(label));
    }
    w as u16
}

fn queue_ctrl_origin(inner: Rect) -> u16 {
    inner.right().saturating_sub(queue_ctrl_width())
}

fn queue_hit(area: Rect, queue: &[Queued], x: u16, y: u16) -> Option<QueueHit> {
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

fn notice_strip(buf: &mut Buffer, area: Rect, label: &str, label_style: Style, items: &[String]) {
    if area.is_empty() || items.is_empty() {
        return;
    }
    let y = area.y;
    let mut x = area.x.saturating_add(1);
    x = buf.write(area, x, y, &format!("{label}  "), label_style);
    write_notice_line(buf, area, x, y, items, label_style);
}

fn notice_height(items: &[String], room: u16) -> u16 {
    if items.is_empty() || room < 3 {
        0
    } else {
        3.min(room)
    }
}

fn write_notice_line(
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
        let cx = area.right().saturating_sub(count_w as u16).saturating_sub(1);
        if cx > x {
            buf.write(area, cx, y, &count, count_style);
        }
    }
}

fn notice_panel(
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

fn queue_panel(
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
            let gap_style = if hot { muted().bg(rgb_color(pal().queue)) } else { muted() };
            cx = buf.write(inner, cx, y, gap, gap_style);
            let style = match kind {
                QueueCtrl::Steer => accent(),
                QueueCtrl::Earlier => earlier_style,
                QueueCtrl::Later => later_style,
                QueueCtrl::Edit => user_col(),
                QueueCtrl::Remove => err_col(),
            };
            let style = if hot { style.bold().bg(rgb_color(pal().queue)) } else { style };
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

fn wrap_body(rows: &[Line], partial: &str, working: bool, width: usize) -> Vec<Line> {
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

fn selected_from_lines(
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

fn slice_line(line: &Line, origin_x: u16, x0: u16, x1: u16) -> String {
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

fn body_window(len: usize, height: usize, follow: bool, start: usize) -> (usize, usize) {
    let max_start = len.saturating_sub(height);
    let start = if follow { max_start } else { start.min(max_start) };
    (max_start.saturating_sub(start), start)
}

fn body(
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

struct ComposerPiece {
    text: String,
    style: Style,
    chip: Option<usize>,
}

struct ComposerLine {
    pieces: Vec<ComposerPiece>,
    width: usize,
}

struct ComposerView {
    lines: Vec<ComposerLine>,
    cursor_line: usize,
    cursor_col: usize,
}

fn chip_text(label: &str, max: usize) -> String {
    let max = max.max(5);
    let budget = max.saturating_sub(4);
    format!("[{} ×]", clip_width(label, budget))
}

fn composer_view(atoms: &[ComposerAtom], cursor: usize, max: usize) -> ComposerView {
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
                if lines.last().is_some_and(|l| l.width + cw > max && l.width > 0) {
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
                if lines.last().is_some_and(|l| l.width + tw > max && l.width > 0) {
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

fn composer(
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
    let vis = view.cursor_line.saturating_sub(start).min(h.saturating_sub(1));
    let y = inner.y.saturating_add(vis as u16);
    let x = inner
        .x
        .saturating_add(1)
        .saturating_add(2)
        .saturating_add(view.cursor_col as u16);
    ((x, y), hits)
}

fn wrap_lines(lines: &[Line], max: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for line in lines {
        out.extend(wrap_row(&line.spans, max, line.hang));
    }
    out
}

fn wrap_row(spans: &[Span], max: usize, hang: usize) -> Vec<Line> {
    if max == 0 {
        return vec![Line::empty()];
    }
    if spans.is_empty() {
        return vec![Line::empty().with_hang(hang)];
    }

    struct Chunk {
        text: String,
        style: Style,
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
                && last.space == space
            {
                last.text.push(c);
                last.width += cw;
                continue;
            }
            chunks.push(Chunk {
                text: c.to_string(),
                style: span.style,
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

    let push_span = |rows: &mut Vec<Vec<Span>>, text: String, style: Style| {
        if rows.is_empty() {
            rows.push(Vec::new());
        }
        let Some(row) = rows.last_mut() else {
            return;
        };
        if let Some(prev) = row.last_mut()
            && prev.style == style
        {
            prev.text.push_str(&text);
            return;
        }
        row.push(Span::new(text, style));
    };
    let trim_trailing = |row: &mut Vec<Span>, w: &mut usize| {
        while let Some(last) = row.last_mut() {
            if last.text.is_empty() {
                row.pop();
                continue;
            }
            let trimmed = last.text.trim_end_matches(|c: char| c.is_whitespace() && c != '\n');
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
                    push_span(&mut rows, chunk.text, chunk.style);
                    w += chunk.width;
                }
                continue;
            }
            if w + chunk.width > max_here {
                wrap(&mut rows, &mut w, &mut line_idx);
                continue;
            }
            push_span(&mut rows, chunk.text, chunk.style);
            w += chunk.width;
            continue;
        }
        if w > 0 && w + chunk.width > max_here {
            wrap(&mut rows, &mut w, &mut line_idx);
        }
        let max_here = line_max(line_idx);
        if chunk.width <= max_here {
            push_span(&mut rows, chunk.text, chunk.style);
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
                push_span(&mut rows, std::mem::take(&mut buf), chunk.style);
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
            push_span(&mut rows, buf, chunk.style);
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

fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

fn clip_width(s: &str, max: usize) -> String {
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

fn visible_input(input: &str, cursor: usize, max: usize) -> (String, usize) {
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

fn expand_tabs(s: &str) -> String {
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

fn sanitize(s: &str) -> String {
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

fn blank_after(out: &mut Vec<Line>) {
    if out.last().is_some_and(|l| !l.spans.is_empty()) {
        out.push(Line::empty());
    }
}

fn render_plain(text: &str) -> Vec<Line> {
    expand_tabs(text)
        .split('\n')
        .map(|line| Line::plain(line.to_string(), Style::new()))
        .collect()
}

fn md_options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

struct Table {
    rows: Vec<Vec<Vec<Span>>>,
    align: Vec<Align>,
}

struct ListState {
    ordered: bool,
    next: u64,
}

struct Md {
    out: Vec<Line>,
    width: usize,
    spans: Vec<Span>,
    style: Style,
    style_stack: Vec<Style>,
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
        let dest = self.dest();
        if let Some(last) = dest.last_mut()
            && last.style == style
        {
            last.text.push_str(text);
            return;
        }
        dest.push(Span::new(text, style));
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
                    self.push_text(&text, self.style);
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
            Tag::Link { .. } => {
                self.push_style(accent().underline());
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
            TagEnd::Link => self.pop_style(),
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

fn render_md(text: &str, width: usize) -> Vec<Line> {
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

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| width(&s.text)).sum()
}

fn truncate_spans(spans: &[Span], max: usize) -> Vec<Span> {
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
            out.push(Span::new(text, span.style));
        }
        if used >= max {
            break;
        }
    }
    out
}

fn fit_table_widths(widths: &mut [usize], max: usize) {
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

fn render_table(table: &Table, max: usize) -> Vec<Line> {
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

fn table_sep(widths: &[usize]) -> Line {
    let mut s = String::new();
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            s.push('┼');
        }
        s.extend(std::iter::repeat_n('─', w + 2));
    }
    Line::plain(s, muted())
}

fn table_row(row: &[Vec<Span>], widths: &[usize], align: &[Align], header: bool) -> Line {
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

fn code_block(lines: &[String], lang: &str, max: usize) -> Vec<Line> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.text.as_str()).collect()
    }

    fn md_text(text: &str, width: usize) -> Vec<String> {
        render_md(text, width)
            .into_iter()
            .map(|l| line_text(&l))
            .collect()
    }

    fn plain_text(text: &str) -> Vec<String> {
        render_plain(text).into_iter().map(|l| line_text(&l)).collect()
    }

    fn laid_out(text: &str, width: usize) -> Vec<String> {
        wrap_lines(&render_md(text, width), width)
            .into_iter()
            .map(|l| line_text(&l))
            .collect()
    }

    #[test]
    fn stream_preserves_newlines() {
        let lines = plain_text("hello\nworld\n\nnext");
        assert_eq!(lines, vec!["hello", "world", "", "next"]);
    }

    #[test]
    fn stream_keeps_list_markers_raw() {
        let lines = plain_text("- one\n- two\n- three");
        assert_eq!(lines, vec!["- one", "- two", "- three"]);
    }

    #[test]
    fn markdown_joins_paragraph_softbreaks() {
        let lines = md_text("hello\nworld\n\nnext", 80);
        assert_eq!(lines, vec!["hello world", "next"]);
    }

    #[test]
    fn renders_markdown_table() {
        let src = concat!(
            "| Name | Qty |\n",
            "| ---- | --: |\n",
            "| eggs | 12 |\n",
            "| milk | 1 |\n",
        );
        let lines = md_text(src, 80);
        assert!(lines.iter().any(|l| l.contains("Name") && l.contains("Qty")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains('─') && l.contains('┼')), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("eggs") && l.contains("12")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("milk")), "{lines:?}");
    }

    #[test]
    fn table_does_not_join_into_paragraph() {
        let src = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        let lines = md_text(src, 40);
        assert!(
            !lines.iter().any(|l| l.contains("| --- |")),
            "{lines:?}"
        );
        assert!(lines.len() >= 3, "{lines:?}");
    }

    #[test]
    fn renders_unordered_list() {
        let lines = laid_out("- one\n- two\n- three\n- four", 80);
        assert_eq!(lines, vec!["• one", "• two", "• three", "• four"]);
    }

    #[test]
    fn renders_ordered_list() {
        let lines = laid_out("1. alpha\n2. beta\n3. gamma", 80);
        assert_eq!(lines, vec!["1. alpha", "2. beta", "3. gamma"]);
    }

    #[test]
    fn renders_nested_list() {
        let lines = laid_out("- a\n  - b\n    - c\n- d", 80);
        assert_eq!(lines, vec!["• a", "  • b", "    • c", "• d"]);
    }

    #[test]
    fn wraps_list_item_with_hanging_indent() {
        let lines = laid_out("- hello world there", 12);
        assert_eq!(lines[0], "• hello");
        assert!(lines.len() > 1, "{lines:?}");
        for cont in &lines[1..] {
            assert!(
                cont.starts_with("  ") && !cont.starts_with("   "),
                "{lines:?}"
            );
            assert!(!cont.starts_with("•"), "{lines:?}");
        }
    }

    #[test]
    fn joins_indented_list_continuation() {
        let lines = laid_out("- hello\n  world", 80);
        assert_eq!(lines, vec!["• hello world"]);
    }

    #[test]
    fn task_list_markers() {
        let lines = laid_out("- [ ] todo\n- [x] done", 80);
        assert_eq!(lines, vec!["• todo", "✓ done"]);
    }

    #[test]
    fn inline_code_and_emphasis() {
        let lines = md_text("say `code` and **bold**", 80);
        assert_eq!(lines, vec!["say code and bold"]);
    }

    #[test]
    fn queue_hit_maps_controls() {
        let area = Rect::new(0, 10, 80, 4);
        let queue = vec![
            Queued { text: "one".into() },
            Queued { text: "two".into() },
        ];
        let inner = queue_inner(area);
        let mut cx = queue_ctrl_origin(inner).saturating_add(1);
        let mut hits = Vec::new();
        for (i, (label, kind)) in QUEUE_CTRLS.iter().enumerate() {
            if i > 0 {
                cx = cx.saturating_add(2);
            }
            hits.push((cx, queue_ctrl_hit(*kind, 0), queue_ctrl_hit(*kind, 1)));
            cx = cx.saturating_add(width(label) as u16);
        }
        assert_eq!(queue_hit(area, &queue, inner.x, 11), Some(QueueHit::Drag(0)));
        assert_eq!(queue_hit(area, &queue, hits[0].0, 11), Some(QueueHit::Steer(0)));
        assert_eq!(queue_hit(area, &queue, hits[1].0, 11), Some(QueueHit::Edit(0)));
        assert_eq!(queue_hit(area, &queue, hits[2].0, 12), Some(hits[2].2));
        assert_eq!(queue_hit(area, &queue, hits[3].0, 11), Some(hits[3].1));
        assert_eq!(queue_hit(area, &queue, hits[4].0, 12), Some(QueueHit::Drop(1)));
        assert_eq!(queue_hit(area, &queue, inner.x, 10), None);
        assert_eq!(queue_hit(area, &queue, inner.x, 13), None);
    }

    #[test]
    fn tool_block_hides_ok_until_open() {
        let runs = vec![
            ToolRun {
                name: "Bash".into(),
                args: "cmd=\"ls\"".into(),
                detail: String::new(),
                is_error: false,
            },
            ToolRun {
                name: "Read".into(),
                args: "path=\"x\"".into(),
                detail: "missing".into(),
                is_error: true,
            },
        ];
        let (summary, closed) = tool_block(&runs, false, 80);
        assert_eq!(summary, 1);
        let closed: Vec<String> = closed.iter().map(line_text).collect();
        assert!(closed.iter().any(|l| l.contains("1 tool succeeded")));
        assert!(closed.iter().any(|l| l.contains("  ⏺ Read(path=\"x\")")));
        assert!(closed.iter().any(|l| l.contains("    missing")));
        assert!(!closed.iter().any(|l| l.contains("Bash(cmd=\"ls\")")));
        let (_, open) = tool_block(&runs, true, 80);
        let open: Vec<String> = open.iter().map(line_text).collect();
        let bash = open.iter().position(|l| l.contains("Bash(cmd=\"ls\")"));
        let read = open.iter().position(|l| l.contains("Read(path=\"x\")"));
        assert!(bash.is_some(), "missing bash: {open:?}");
        assert!(read.is_some(), "missing read: {open:?}");
        if let (Some(bash), Some(read)) = (bash, read) {
            assert!(open[bash].starts_with("  ⏺ "), "{open:?}");
            assert!(
                open[bash + 1..read].iter().any(|l| l.is_empty()),
                "{open:?}"
            );
        }
    }

    #[test]
    fn queue_height_includes_box_chrome() {
        let q = vec![Queued { text: "a".into() }];
        assert_eq!(queue_height(&q, 24), 3);
        let q = vec![
            Queued { text: "a".into() },
            Queued { text: "b".into() },
            Queued { text: "c".into() },
            Queued { text: "d".into() },
            Queued { text: "e".into() },
        ];
        assert_eq!(queue_height(&q, 24), 7);
        assert_eq!(queue_height(&[], 24), 0);
        assert_eq!(queue_height(&q, 2), 0);
    }

    fn sample_bar() -> Bar {
        Bar {
            workspace: "~/fun-coding-agent".into(),
            branch: Some("main".into()),
            pull: Some("main".into()),
            input: "12.3k".into(),
            output: "4.5k".into(),
            reasoning: "8.1k".into(),
            cache_hit: "80.0%".into(),
            cost: "$0.012".into(),
            context: "12.3%/500k".into(),
            model: "grok-4.6".into(),
            effort: "medium".into(),
        }
    }

    fn status_line_text(bar: &Bar, width: u16) -> Vec<String> {
        status_rows(bar, false, 0, 0, width)
            .into_iter()
            .map(|row| row.into_iter().map(|(t, _)| t).collect())
            .collect()
    }

    #[test]
    fn chrome_keeps_status_visible() {
        assert_eq!(chrome_heights(1, 2, 3), (0, 1));
        assert_eq!(chrome_heights(4, 2, 3), (3, 1));
        assert_eq!(chrome_heights(8, 2, 3), (3, 2));
        assert_eq!(chrome_heights(10, 3, 3), (3, 3));
        assert_eq!(chrome_heights(20, 2, 6), (6, 2));
    }

    #[test]
    fn composer_keeps_pasted_newlines() {
        let atoms: Vec<ComposerAtom> = "hello\nworld\n!"
            .chars()
            .map(ComposerAtom::Char)
            .collect();
        let view = composer_view(&atoms, 13, 40);
        let lines: Vec<String> = view
            .lines
            .iter()
            .map(|l| l.pieces.iter().map(|p| p.text.as_str()).collect())
            .collect();
        assert_eq!(lines, vec!["hello", "world", "!"]);
        assert_eq!(view.cursor_line, 2);
        assert_eq!(view.cursor_col, 1);
        let after_nl: Vec<ComposerAtom> = "hello\n".chars().map(ComposerAtom::Char).collect();
        let after = composer_view(&after_nl, 6, 40);
        let after_lines: Vec<String> = after
            .lines
            .iter()
            .map(|l| l.pieces.iter().map(|p| p.text.as_str()).collect())
            .collect();
        assert_eq!(after_lines, vec!["hello", ""]);
        assert_eq!(after.cursor_line, 1);
        let three: Vec<ComposerAtom> = "a\nb\nc".chars().map(ComposerAtom::Char).collect();
        assert_eq!(composer_wanted_height(&three, 80), 5);
        let mut buf = Buffer::new(20, 5);
        let area = Rect::new(0, 0, 20, 5);
        let two: Vec<ComposerAtom> = "one\ntwo".chars().map(ComposerAtom::Char).collect();
        composer(&mut buf, area, &two, 7, false);
        let row = |y: u16| {
            (1..19)
                .map(|x| buf.get(x, y).ch)
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        assert!(row(1).contains("› one"), "{}", row(1));
        assert!(row(2).contains("two"), "{}", row(2));
        assert!(!row(1).contains("one two"), "{}", row(1));
    }

    #[test]
    fn composer_draws_paste_chip_inline() {
        let atoms = vec![
            ComposerAtom::Char('h'),
            ComposerAtom::Char('i'),
            ComposerAtom::Char(' '),
            ComposerAtom::Chip {
                index: 0,
                label: "paste 12 lines".into(),
            },
        ];
        let mut buf = Buffer::new(40, 3);
        let area = Rect::new(0, 0, 40, 3);
        let (cursor, hits) = composer(&mut buf, area, &atoms, 4, false);
        let row = (1..39)
            .map(|x| buf.get(x, 1).ch)
            .collect::<String>()
            .trim_end()
            .to_string();
        assert!(row.contains("› hi [paste 12 lines ×]"), "{row}");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, 0);
        assert!(hits[0].0.contains(cursor.0.saturating_sub(2), cursor.1) || hits[0].0.y == 1);
        assert_eq!(composer_wanted_height(&atoms, 80), 3);
    }

    #[test]
    fn body_window_follows_bottom() {
        assert_eq!(body_window(20, 10, true, 0), (0, 10));
        assert_eq!(body_window(20, 10, true, 3), (0, 10));
        assert_eq!(body_window(5, 10, true, 3), (0, 0));
    }

    #[test]
    fn body_window_pin_grows_hidden_count() {
        assert_eq!(body_window(20, 10, false, 5), (5, 5));
        assert_eq!(body_window(30, 10, false, 5), (15, 5));
        assert_eq!(body_window(12, 10, false, 5), (0, 2));
        assert_eq!(body_window(20, 10, false, 100), (0, 10));
    }

    #[test]
    fn status_fits_narrow_width() {
        let bar = sample_bar();
        for cols in [24u16, 40, 60, 80, 120] {
            let lines = status_line_text(&bar, cols);
            let inner = cols.saturating_sub(2).max(1) as usize;
            assert!(!lines.is_empty(), "width {cols}");
            for line in &lines {
                assert!(
                    width(line) <= inner,
                    "width {cols}: {line:?} is {}",
                    width(line)
                );
            }
            let joined = lines.join(" ");
            assert!(joined.contains("~/fun-coding-agent"), "{joined}");
            assert!(!joined.contains("pull master"), "{joined}");
            assert!(joined.contains("grok-4.6"), "{joined}");
            assert!(joined.contains("medium"), "{joined}");
            let last = lines.last().map(String::as_str).unwrap_or("");
            assert!(last.contains("grok-4.6"), "last {last:?}");
            assert!(last.contains("medium"), "last {last:?}");
        }
    }

    #[test]
    fn pull_master_sits_above_composer() {
        let area = Rect::new(0, 19, 80, 1);
        assert_eq!(
            fun_core::config::fill_action("checkout and pull {home}", Some("master"), None, None).as_deref(),
            Some("checkout and pull master")
        );
        assert_eq!(
            fun_core::config::fill_action(&Action::defaults()[0].label, Some("master"), None, None).as_deref(),
            Some("[ checkout and pull master ]")
        );
        assert_eq!(
            fun_core::config::fill_action(&Action::defaults()[0].label, Some("main"), None, None).as_deref(),
            Some("[ checkout and pull main ]")
        );
        assert_eq!(
            Action::defaults()[1].filled_label(Some("master"), Some("feat")).as_deref(),
            Some("[ commit to feat and push ]")
        );
        assert_eq!(
            Action::defaults()[1].filled_label(Some("master"), Some("master")).as_deref(),
            Some("[ commit and push ]")
        );
        crate::tui::set_git(true);
        crate::tui::set_origin(true);
        assert_eq!(action_bar_height(80, Some("master"), Some("feat"), 4), 1);
        assert_eq!(action_bar_height(80, Some("master"), Some("feat"), 0), 0);
        let hits = paint_action_bar(
            &mut Buffer::new(80, 24),
            area,
            Some("master"),
            Some("feat"),
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].1, ActionHit { index: 0 });
        assert_eq!(hits[1].1, ActionHit { index: 1 });
        let pull = hits[0].0;
        let push = hits[1].0;
        assert_eq!(pull.height, 1);
        assert_eq!(push.height, 1);
        assert_eq!(pull.y, area.y);
        assert_eq!(push.y, area.y);
        assert!(push.x >= pull.right());
        assert_eq!(push.right(), area.right());
        assert_eq!(pull.width as usize, width("[ checkout and pull master ]"));
        assert_eq!(push.width as usize, width("[ commit to feat and push ]"));
        let on_home = paint_action_bar(
            &mut Buffer::new(80, 24),
            area,
            Some("master"),
            Some("master"),
        );
        assert_eq!(on_home.len(), 2);
        assert_eq!(
            on_home[1].0.width as usize,
            width("[ commit and push ]")
        );
        crate::tui::set_origin(false);
        let no_origin = paint_action_bar(
            &mut Buffer::new(80, 24),
            area,
            Some("master"),
            Some("feat"),
        );
        assert_eq!(no_origin.len(), 2);
        crate::tui::set_git(false);
        let no_git = paint_action_bar(&mut Buffer::new(80, 24), area, Some("master"), None);
        assert_eq!(no_git.len(), 1);
        assert_eq!(no_git[0].1, ActionHit { index: 1 });
        assert_eq!(
            no_git[0].0.width as usize,
            width("[ commit to master and push ]")
        );
    }

    #[test]
    fn status_keeps_full_labels_when_narrow() {
        let bar = sample_bar();
        let wide = status_line_text(&bar, 160).join(" ");
        let narrow = status_line_text(&bar, 60).join(" ");
        for text in [&wide, &narrow] {
            assert!(text.contains("input tokens"), "{text}");
            assert!(text.contains("output tokens"), "{text}");
            assert!(text.contains("reasoning tokens"), "{text}");
            assert!(text.contains("cache hit"), "{text}");
            assert!(!text.contains("in 12.3k"), "{text}");
            assert!(!text.contains("out 4.5k"), "{text}");
            assert!(!text.contains("think 8.1k"), "{text}");
        }
    }
}
