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


use fun_core::agent::ToolRun;
use fun_core::config::{Action, Palette};
use crate::ui::{
    split, Color, Constraint, Rect, Style, Terminal,
};
#[cfg(test)]
use crate::ui::{Block, Buffer};
use anyhow::Result;
use std::cell::Cell;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

mod chrome;
mod markdown;
use chrome::*;
use markdown::*;


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
    link: Option<String>,
}

impl Span {
    fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            link: None,
        }
    }

    fn linked(text: impl Into<String>, style: Style, link: Option<String>) -> Self {
        Self {
            text: text.into(),
            style,
            link,
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
    Note {
        text: String,
        inspect: Option<Vec<String>>,
    },
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
    link_area: Rect,
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
    hover_url: Option<String>,
    select_held: bool,
    ask: Option<AskDialog>,
    prune: Option<Vec<String>>,
    prune_scroll: usize,
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
            link_area: Rect::new(0, 0, 0, 0),
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
            hover_url: None,
            select_held: false,
            ask: None,
            prune: None,
            prune_scroll: 0,
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
            self.select_held = false;
            return self.redraw();
        }
        let p = self.screen_to_sel(x, y);
        self.select = Some((p, p));
        self.select_held = true;
        self.redraw()
    }

    pub fn click_prune_note(&self, x: u16, y: u16) -> Option<Vec<String>> {
        if !self.in_body(x, y) {
            return None;
        }
        let row = self.screen_to_sel(x, y).1;
        self.item_rows
            .iter()
            .position(|(start, _, end)| row >= *start && row < *end)
            .and_then(|i| self.items.get(i))
            .and_then(|item| match item {
                Item::Note {
                    inspect: Some(lines),
                    ..
                } if !lines.is_empty() => Some(lines.clone()),
                Item::Note { text, .. } if is_prune_note(text) => Some(Vec::new()),
                _ => None,
            })
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

    fn link_at_pointer(&self, x: u16, y: u16) -> Option<String> {
        if self.link_area.contains(x, y) {
            return self.hover_url.clone();
        }
        link_at(
            &self.rows,
            &self.partial,
            self.working,
            self.body_area,
            x,
            y,
            self.body_start,
        )
    }

    pub fn open_clicked_link(&mut self, x: u16, y: u16) -> bool {
        let Some(url) = self.link_at_pointer(x, y) else {
            return false;
        };
        self.select = None;
        self.select_held = false;
        let _ = open_url(&url);
        true
    }

    pub fn hover_at(&mut self, x: u16, y: u16) -> Result<()> {
        if self.link_area.contains(x, y) && self.hover_url.is_some() {
            return Ok(());
        }
        let url = link_at(
            &self.rows,
            &self.partial,
            self.working,
            self.body_area,
            x,
            y,
            self.body_start,
        );
        if url == self.hover_url {
            return Ok(());
        }
        self.hover_url = url;
        self.redraw()
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
        self.select_held = false;
        if self.take_queue_pointer() {
            return Ok(None);
        }
        self.select_drag(x, y)?;
        Ok(self.selected_text())
    }

    pub fn is_selecting(&self) -> bool {
        self.select_held && self.select.is_some() && !self.queue_pointer
    }

    pub fn asking(&self) -> bool {
        self.ask.is_some()
    }

    pub fn pruning(&self) -> bool {
        self.prune.is_some()
    }

    pub fn open_prune(&mut self, lines: Vec<String>) -> Result<()> {
        self.prune = Some(lines);
        self.prune_scroll = 0;
        self.redraw()
    }

    pub fn close_prune(&mut self) -> Result<()> {
        self.prune = None;
        self.prune_scroll = 0;
        self.redraw()
    }

    pub fn prune_scroll(&mut self, delta: i32) -> Result<()> {
        let Some(lines) = self.prune.as_ref() else {
            return Ok(());
        };
        let max = prune_max_scroll(lines.len(), self.term.size);
        let next = next_prune_scroll(self.prune_scroll, delta, max);
        if next == self.prune_scroll {
            return Ok(());
        }
        self.prune_scroll = next;
        self.redraw()
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
        self.note_inspect(text, None)
    }

    pub fn note_inspect(&mut self, text: &str, inspect: Option<Vec<String>>) -> Result<()> {
        self.push_plain(Item::Note {
            text: sanitize(text),
            inspect,
        })
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
            Item::Note { text, .. } => text.trim().is_empty(),
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
        let hover_url = self.hover_url.as_deref();
        let ask = self.ask.as_ref();
        let prune = self.prune.as_ref();
        let prune_scroll = self.prune_scroll;
        let mut used_scroll = 0usize;
        let mut body_start = self.body_start;
        let mut body_area = Rect::new(0, 0, 0, 0);
        let mut queue_area = Rect::new(0, 0, 0, 0);
        let mut link_area = Rect::new(0, 0, 0, 0);
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
            if let Some(url) = hover_url.filter(|u| !u.is_empty()) {
                link_area = overlay_above(area, composer_area, 3);
                if !link_area.is_empty() {
                    fill_rect(buf, link_area, Style::new());
                    notice_panel(
                        buf,
                        link_area,
                        "link",
                        accent(),
                        accent().bold(),
                        &[url.to_string()],
                    );
                }
            }
            if show_copied {
                paint_copied(buf, area, composer_area);
            }
            if let Some(ask) = ask {
                return paint_ask(buf, area, ask);
            }
            if let Some(lines) = prune {
                paint_prune(buf, area, lines, prune_scroll);
            }
            cursor_pos
        })?;
        self.body_area = body_area;
        self.queue_area = queue_area;
        self.link_area = link_area;
        self.composer_area = composer_area;
        self.chip_hits = chip_hits;
        self.actions = actions;
        self.scroll = used_scroll;
        self.body_start = body_start;
        self.follow = used_scroll == 0;
        Ok(())
    }
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
    fn prune_scroll_clamps_and_pages() {
        assert_eq!(next_prune_scroll(0, -8, 20), 0);
        assert_eq!(next_prune_scroll(0, 8, 20), 8);
        assert_eq!(next_prune_scroll(18, 8, 20), 20);
        assert_eq!(next_prune_scroll(5, -8, 20), 0);
        let screen = Rect::new(0, 0, 80, 24);
        let max = prune_max_scroll(100, screen);
        assert!(max > 0 && max < 100, "{max}");
        assert_eq!(next_prune_scroll(max, 8, max), max);
        assert_eq!(next_prune_scroll(max, -8, max), max.saturating_sub(8));
    }

    #[test]
    fn prune_note_is_clickable_status() {
        assert!(is_prune_note("(dropped 25 messages from context)"));
        assert!(is_prune_note(
            "(dropped 19 messages from context, restored 4)"
        ));
        assert!(is_prune_note(
            "(resumed /tmp/s.jsonl, 40 entries, 12 pruned from context)"
        ));
        assert!(!is_prune_note("(pruning context)"));
        assert!(!is_prune_note("(nothing to drop)"));
        let (_, lines) = layout_item(
            &Item::Note {
                text: "(dropped 25 messages from context)".into(),
                inspect: Some(vec!["[1] assistant dump".into()]),
            },
            80,
        );
        assert!(lines
            .iter()
            .flat_map(|l| &l.spans)
            .any(|s| s.style.underline && s.text.contains("dropped 25")));
    }

    #[test]
    fn stream_keeps_markdown_raw() {
        let src = "see [docs](https://example.com/a) and https://example.com/b";
        let lines = render_plain(src);
        assert_eq!(plain_text(src), vec![src]);
        assert!(lines.iter().all(|l| l.spans.iter().all(|s| s.link.is_none())));
        assert!(lines.iter().all(|l| l.spans.iter().all(|s| !s.style.underline)));
    }

    #[test]
    fn markdown_joins_paragraph_softbreaks() {
        let lines = md_text("hello\nworld\n\nnext", 80);
        assert_eq!(lines, vec!["hello world", "next"]);
    }

    #[test]
    fn markdown_link_keeps_url() {
        let lines = render_md("see [docs](https://example.com/a) please", 80);
        let hit = lines.iter().find_map(|l| {
            l.spans
                .iter()
                .find(|s| s.link.as_deref() == Some("https://example.com/a"))
        });
        assert!(hit.is_some());
        assert!(hit.unwrap().text.contains("docs"));
        assert!(hit.unwrap().style.underline);
        assert!(hit.unwrap().style.fg.is_none());
        assert!(http_url("https://example.com/a").is_some());
        assert!(http_url("javascript:alert(1)").is_none());
        assert!(http_url("file:///etc/passwd").is_none());
    }

    #[test]
    fn markdown_autolinks_bare_http_url() {
        let lines = render_md("open https://example.com/a, please", 80);
        let hit = lines.iter().find_map(|l| {
            l.spans
                .iter()
                .find(|s| s.link.as_deref() == Some("https://example.com/a"))
        });
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().text, "https://example.com/a");
        assert!(hit.unwrap().style.underline);
    }

    #[test]
    fn link_at_hits_markdown_label() {
        let rows = indent_lines(
            wrap_lines(&render_md("see [docs](https://example.com/a) please", 40), 40),
            "  ",
        );
        let area = Rect::new(0, 0, 44, 3);
        assert_eq!(
            link_at(&rows, "", false, area, 7, 0, 0).as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(link_at(&rows, "", false, area, 3, 0, 0), None);
    }

    #[test]
    fn link_preview_uses_notice_box() {
        let url = "https://example.com/a";
        let items = vec![url.to_string()];
        let mut buf = Buffer::new(40, 12);
        let screen = buf.area();
        let composer = Rect::new(0, 8, 40, 3);
        let overlay = overlay_above(screen, composer, 3);
        assert_eq!(overlay, Rect::new(0, 5, 40, 3));
        fill_rect(&mut buf, overlay, Style::new());
        notice_panel(&mut buf, overlay, "link", accent(), accent().bold(), &items);
        assert_eq!(buf.get(3, 5).ch, 'l');
        let inner = Block::inner(overlay);
        let shown: String = (inner.x.saturating_add(1)..inner.right())
            .map(|x| buf.get(x, inner.y).ch)
            .collect::<String>()
            .trim()
            .to_string();
        assert!(shown.contains(url), "{shown:?}");
        assert_eq!(composer.y, 8);
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
