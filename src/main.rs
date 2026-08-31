mod agent;
mod config;
mod grok;
mod session;
mod tool;
mod tui;
mod ui;

use agent::{mailbox, print_log, prompt, tool_fail, tool_ok, Agent, LogLine, MailboxTx, ToolRun};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use crossterm::event::{
    self, Event as CEvent, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use grok::{login, logout, Grok};
use session::{empty_args, list_sessions, Call, Entry, Session, Usage};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tool::get_tools;
use tui::{Bar, QueueHit, Queued, Ui};

enum KeyAction {
    Skip,
    Confirm,
    Interrupt,
    Abort,
    Quit,
    Scroll(i32),
    ScrollEnd,
    Copy,
    SelectDrag(u16, u16),
    SelectEnd(u16, u16),
    ClickTools(u16, u16),
    QueueSteer(usize),
    QueueUp(usize),
    QueueDown(usize),
    QueueEdit(usize),
    QueueDrop(usize),
    QueueMove { from: usize, to: usize },
    Action(usize),
    AskSubmit,
}

struct LineEdit {
    input: String,
    cursor: usize,
    history: Vec<String>,
    idx: usize,
    draft: String,
    queue_slot: Option<usize>,
}

fn prev_boundary(s: &str, mut i: usize) -> usize {
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    i += 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn word_left(s: &str, mut i: usize) -> usize {
    i = prev_boundary(s, i.min(s.len()));
    while i > 0 && s[i..].chars().next().is_some_and(char::is_whitespace) {
        i = prev_boundary(s, i);
    }
    while i > 0 {
        let prev = prev_boundary(s, i);
        if s[prev..].chars().next().is_some_and(char::is_whitespace) {
            break;
        }
        i = prev;
    }
    i
}

impl LineEdit {
    fn from_session(session: &Session) -> Self {
        let history: Vec<String> = session
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::User { text } if !text.is_empty() => Some(text.clone()),
                _ => None,
            })
            .collect();
        let idx = history.len();
        Self {
            input: String::new(),
            cursor: 0,
            history,
            idx,
            draft: String::new(),
            queue_slot: None,
        }
    }

    fn set_input(&mut self, text: String) {
        self.input = text;
        self.cursor = self.input.len();
    }

    fn remember(&mut self, text: &str) {
        self.history.push(text.to_string());
        self.idx = self.history.len();
        self.draft.clear();
    }

    fn insert(&mut self, ch: char) {
        self.input.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let from = prev_boundary(&self.input, self.cursor);
        self.input.replace_range(from..self.cursor, "");
        self.cursor = from;
    }

    fn delete(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        let to = next_boundary(&self.input, self.cursor);
        self.input.replace_range(self.cursor..to, "");
    }

    fn kill_word(&mut self) {
        let from = word_left(&self.input, self.cursor);
        self.input.replace_range(from..self.cursor, "");
        self.cursor = from;
    }
}

fn on_event(ui: &mut Ui, edit: &mut LineEdit, ev: CEvent) -> KeyAction {
    if ui.asking() {
        return on_ask(ui, ev);
    }
    if let CEvent::Paste(s) = ev {
        for c in s.chars() {
            if c == '\n' || c == '\r' {
                edit.insert(' ');
            } else if !c.is_control() {
                edit.insert(c);
            }
        }
        return KeyAction::Skip;
    }
    if let CEvent::Mouse(mouse) = ev {
        return on_mouse(ui, mouse);
    }
    on_key(edit, ev)
}

fn on_ask(ui: &mut Ui, ev: CEvent) -> KeyAction {
    if let CEvent::Paste(s) = ev {
        for c in s.chars() {
            if !c.is_control() {
                let _ = ui.ask_insert(c);
            }
        }
        return KeyAction::Skip;
    }
    let CEvent::Key(key) = ev else {
        return KeyAction::Skip;
    };
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c' | 'C'))
        && !key.modifiers.contains(KeyModifiers::SHIFT)
    {
        return KeyAction::Quit;
    }
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return KeyAction::Skip;
    }
    match key.code {
        KeyCode::Esc => {
            let _ = ui.close_ask();
            KeyAction::Skip
        }
        KeyCode::Enter | KeyCode::Char('\n') => KeyAction::AskSubmit,
        KeyCode::Backspace => {
            let _ = ui.ask_backspace();
            KeyAction::Skip
        }
        KeyCode::Delete => {
            let _ = ui.ask_delete();
            KeyAction::Skip
        }
        KeyCode::Left => {
            let _ = ui.ask_left();
            KeyAction::Skip
        }
        KeyCode::Right => {
            let _ = ui.ask_right();
            KeyAction::Skip
        }
        KeyCode::Char(c) if !c.is_control() => {
            let _ = ui.ask_insert(c);
            KeyAction::Skip
        }
        _ => KeyAction::Skip,
    }
}

fn on_mouse(ui: &mut Ui, mouse: crossterm::event::MouseEvent) -> KeyAction {
    match mouse.kind {
        MouseEventKind::ScrollUp => KeyAction::Scroll(3),
        MouseEventKind::ScrollDown => KeyAction::Scroll(-3),
        MouseEventKind::Down(MouseButton::Left) => match ui.action_hit(mouse.column, mouse.row) {
            Some(hit) => KeyAction::Action(hit.index),
            None => match ui.queue_hit(mouse.column, mouse.row) {
                Some(QueueHit::Steer(i)) => {
                    ui.capture_queue_pointer();
                    KeyAction::QueueSteer(i)
                }
                Some(QueueHit::Up(i)) => {
                    ui.capture_queue_pointer();
                    KeyAction::QueueUp(i)
                }
                Some(QueueHit::Down(i)) => {
                    ui.capture_queue_pointer();
                    KeyAction::QueueDown(i)
                }
                Some(QueueHit::Edit(i)) => {
                    ui.capture_queue_pointer();
                    KeyAction::QueueEdit(i)
                }
                Some(QueueHit::Drop(i)) => {
                    ui.capture_queue_pointer();
                    KeyAction::QueueDrop(i)
                }
                Some(QueueHit::Drag(i)) => {
                    ui.begin_queue_drag(i);
                    KeyAction::Skip
                }
                None => KeyAction::ClickTools(mouse.column, mouse.row),
            },
        },
        MouseEventKind::Drag(_) => {
            if let Some(from) = ui.queue_drag_index() {
                match ui.queue_hit(mouse.column, mouse.row) {
                    Some(
                        QueueHit::Drag(to)
                        | QueueHit::Steer(to)
                        | QueueHit::Up(to)
                        | QueueHit::Down(to)
                        | QueueHit::Edit(to)
                        | QueueHit::Drop(to),
                    ) if to != from => KeyAction::QueueMove { from, to },
                    _ => KeyAction::Skip,
                }
            } else {
                KeyAction::SelectDrag(mouse.column, mouse.row)
            }
        }
        MouseEventKind::Moved => {
            if ui.is_selecting() {
                KeyAction::SelectDrag(mouse.column, mouse.row)
            } else {
                KeyAction::Skip
            }
        }
        MouseEventKind::Up(_) => {
            if ui.queue_drag_index().is_some() {
                ui.end_queue_drag();
                KeyAction::Skip
            } else {
                KeyAction::SelectEnd(mouse.column, mouse.row)
            }
        }
        _ => KeyAction::Skip,
    }
}

fn on_key(edit: &mut LineEdit, ev: CEvent) -> KeyAction {
    let CEvent::Key(key) = ev else {
        return KeyAction::Skip;
    };
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c' | 'C'))
    {
        // Shift+Ctrl+C copies the highlight; Ctrl+C always quits.
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            return KeyAction::Copy;
        }
        return KeyAction::Quit;
    }
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return KeyAction::Skip;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Enter | KeyCode::Char('\n') => return KeyAction::Interrupt,
            KeyCode::Char('w' | 'h') => {
                if key.code == KeyCode::Char('w') {
                    edit.kill_word();
                } else {
                    edit.backspace();
                }
                return KeyAction::Skip;
            }
            KeyCode::Char('a') => {
                edit.cursor = 0;
                return KeyAction::Skip;
            }
            KeyCode::Char('u') => {
                edit.input.replace_range(..edit.cursor, "");
                edit.cursor = 0;
                return KeyAction::Skip;
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc => KeyAction::Abort,
        KeyCode::PageUp => KeyAction::Scroll(8),
        KeyCode::PageDown => KeyAction::Scroll(-8),
        KeyCode::End => KeyAction::ScrollEnd,
        KeyCode::Left => {
            edit.cursor = prev_boundary(&edit.input, edit.cursor);
            KeyAction::Skip
        }
        KeyCode::Right => {
            edit.cursor = next_boundary(&edit.input, edit.cursor);
            KeyAction::Skip
        }
        KeyCode::Home => {
            edit.cursor = 0;
            KeyAction::Skip
        }
        KeyCode::Delete => {
            edit.delete();
            KeyAction::Skip
        }
        KeyCode::Up => {
            if edit.history.is_empty() {
                return KeyAction::Skip;
            }
            if edit.idx == edit.history.len() {
                edit.draft = edit.input.clone();
            }
            if edit.idx > 0 {
                edit.idx -= 1;
                edit.set_input(edit.history[edit.idx].clone());
            }
            KeyAction::Skip
        }
        KeyCode::Down => {
            if edit.idx < edit.history.len() {
                edit.idx += 1;
                let text = if edit.idx == edit.history.len() {
                    edit.draft.clone()
                } else {
                    edit.history[edit.idx].clone()
                };
                edit.set_input(text);
            }
            KeyAction::Skip
        }
        KeyCode::Enter | KeyCode::Char('\n') => KeyAction::Confirm,
        KeyCode::Backspace => {
            edit.backspace();
            KeyAction::Skip
        }
        KeyCode::Char(c) if !c.is_control() => {
            edit.insert(c);
            KeyAction::Skip
        }
        _ => KeyAction::Skip,
    }
}

fn spawn_events() -> tokio::sync::mpsc::UnboundedReceiver<CEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
    rx
}

fn paint(ui: &mut Ui, line: &LogLine) -> Result<()> {
    match line {
        LogLine::Delta(t) => ui.delta(t),
        LogLine::Think(t) => ui.think(t),
        LogLine::End => ui.end_stream(),
        LogLine::User(t) => ui.user(t),
        LogLine::Text(t) => ui.markdown(t),
        LogLine::Dim(t) => ui.note(t),
        LogLine::Tools { runs } => ui.tools(runs.clone()),
        LogLine::Usage(_) => Ok(()),
    }
}

fn apply_log(
    ui: &mut Ui,
    status: &mut Status,
    pending: &mut Vec<Pending>,
    line: LogLine,
) -> Result<()> {
    match line {
        LogLine::Usage(usage) => {
            status.usage = usage;
            refresh_bar(ui, status)
        }
        LogLine::User(text) => {
            if let Some(i) = pending.iter().position(|p| p.text() == text) {
                pending.remove(i);
            }
            ui.user(&text)
        }
        other => paint(ui, &other),
    }
}

fn flush_log(
    ui: &mut Ui,
    status: &mut Status,
    pending: &mut Vec<Pending>,
    rx: &Receiver<LogLine>,
) -> Result<()> {
    let mut dirty = false;
    while let Ok(line) = rx.try_recv() {
        if matches!(line, LogLine::User(_)) {
            dirty = true;
        }
        apply_log(ui, status, pending, line)?;
    }
    if dirty {
        sync_queue(ui, pending)?;
    }
    Ok(())
}

fn git_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let git = dir.join(".git");
        if git.is_dir() {
            return Some(git);
        }
        if git.is_file() {
            let text = fs::read_to_string(&git).ok()?;
            let path = text.strip_prefix("gitdir:")?.trim();
            if path.is_empty() {
                return None;
            }
            let path = Path::new(path);
            return Some(if path.is_absolute() {
                path.to_path_buf()
            } else {
                dir.join(path)
            });
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn branch_from_head(head: &str) -> Option<String> {
    let rest = head.trim().strip_prefix("ref: ")?;
    let name = rest.strip_prefix("refs/heads/")?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn git_branch(workspace: &Path) -> Option<String> {
    let head = fs::read_to_string(git_dir(workspace)?.join("HEAD")).ok()?;
    branch_from_head(&head)
}

fn branch_from_symref(text: &str) -> Option<String> {
    let rest = text.trim().strip_prefix("ref: ")?;
    let name = rest.rsplit('/').next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn git_ref_exists(git: &Path, rel: &str) -> bool {
    git.join(rel).is_file()
}

fn git_has_origin(git: &Path) -> bool {
    git.join("refs/remotes/origin").is_dir()
        || git.join("refs/remotes/origin/HEAD").is_file()
        || fs::read_to_string(git.join("config"))
            .ok()
            .is_some_and(|text| {
                text.lines().any(|line| {
                    let line = line.trim();
                    line.eq_ignore_ascii_case("[remote \"origin\"]")
                })
            })
}

fn git_origin_head(git: &Path) -> Option<String> {
    let text = fs::read_to_string(git.join("refs/remotes/origin/HEAD")).ok()?;
    branch_from_symref(&text)
}

fn pick_home_branch(
    locals: &[&str],
    origin_head: Option<&str>,
    preferred: &str,
) -> String {
    if locals.contains(&preferred) {
        return preferred.to_string();
    }
    for name in ["master", "main", "dev"] {
        if name != preferred && locals.contains(&name) {
            return name.to_string();
        }
    }
    origin_head
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| preferred.to_string())
}

fn git_home_branch(workspace: &Path) -> Option<String> {
    let git = git_dir(workspace)?;
    let preferred = preferred_home();
    let locals: Vec<&str> = ["master", "main", "dev"]
        .into_iter()
        .filter(|name| git_ref_exists(&git, &format!("refs/heads/{name}")))
        .collect();
    Some(pick_home_branch(
        &locals,
        git_origin_head(&git).as_deref(),
        &preferred,
    ))
}

fn preferred_home() -> String {
    tui::configured_home()
}

fn resolve_home(workspace: &Path) -> String {
    git_home_branch(workspace).unwrap_or_else(preferred_home)
}

fn sync_branch(ui: &mut Ui, status: &mut Status) -> Result<()> {
    let now = Instant::now();
    if status
        .git_at
        .is_some_and(|at| now.duration_since(at) < Duration::from_secs(1))
    {
        return Ok(());
    }
    status.git_at = Some(now);
    let branch = git_branch(&status.workspace);
    let pull = Some(resolve_home(&status.workspace));
    let git = git_dir(&status.workspace);
    tui::set_git(git.is_some());
    tui::set_origin(git.as_ref().is_some_and(|p| git_has_origin(p)));
    if branch != status.branch || pull != status.pull {
        status.branch = branch;
        status.pull = pull;
        refresh_bar(ui, status)?;
    }
    Ok(())
}

fn action_prompt(action: &KeyAction, status: &Status) -> Option<String> {
    match *action {
        KeyAction::Action(i) => {
            let spec = tui::configured_action(i)?;
            if spec.ask.is_some() && !tui::has_origin() {
                return None;
            }
            spec.filled_prompt(status.pull.as_deref(), status.branch.as_deref(), None)
        }
        _ => None,
    }
}

fn action_ask(action: &KeyAction, _status: &Status) -> Option<String> {
    match *action {
        KeyAction::Action(i) => {
            let spec = tui::configured_action(i)?;
            if tui::has_origin() {
                return None;
            }
            spec.ask.clone()
        }
        _ => None,
    }
}

fn action_title(_action: &KeyAction, _status: &Status) -> String {
    "git origin URL".into()
}

fn short_path(path: &Path) -> String {
    let text = path.display().to_string();
    if let Ok(home) = env::var("HOME")
        && let Some(rest) = text.strip_prefix(&home)
    {
        return format!("~{rest}");
    }
    text
}

fn fmt_compact(n: u64) -> String {
    if n >= 1_000_000 {
        let s = format!("{:.1}M", n as f64 / 1_000_000.0);
        if let Some(whole) = s.strip_suffix(".0M") {
            format!("{whole}M")
        } else {
            s
        }
    } else if n >= 1000 {
        let s = format!("{:.1}k", n as f64 / 1000.0);
        if let Some(whole) = s.strip_suffix(".0k") {
            format!("{whole}k")
        } else {
            s
        }
    } else {
        n.to_string()
    }
}

const INPUT_PER_M: f64 = 3.0;
const CACHED_PER_M: f64 = 0.75;
const OUTPUT_PER_M: f64 = 15.0;
const CONTEXT_WINDOW: u64 = 500_000;

fn estimate_cost(input: u64, cached: u64, output: u64) -> f64 {
    let cached = cached.min(input);
    let uncached = input.saturating_sub(cached);
    (uncached as f64 / 1_000_000.0) * INPUT_PER_M
        + (cached as f64 / 1_000_000.0) * CACHED_PER_M
        + (output as f64 / 1_000_000.0) * OUTPUT_PER_M
}

fn fmt_window(n: u64) -> String {
    if n >= 1_000_000 && n.is_multiple_of(1_000_000) {
        format!("{}M", n / 1_000_000)
    } else if n >= 1000 && n.is_multiple_of(1000) {
        format!("{}k", n / 1000)
    } else {
        fmt_compact(n)
    }
}

struct Status {
    workspace: PathBuf,
    model: String,
    effort: String,
    branch: Option<String>,
    pull: Option<String>,
    git_at: Option<Instant>,
    usage: Usage,
}

enum Pending {
    Interrupt(String),
    Steer(String),
    Idle(String),
}

impl Pending {
    fn text(&self) -> &str {
        match self {
            Self::Interrupt(t) | Self::Steer(t) | Self::Idle(t) => t,
        }
    }
}

fn idle_texts(pending: &[Pending]) -> Vec<String> {
    pending
        .iter()
        .filter_map(|p| match p {
            Pending::Idle(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

fn idle_queue(pending: &[Pending]) -> Vec<Queued> {
    idle_texts(pending)
        .into_iter()
        .map(|text| Queued { text })
        .collect()
}

fn idle_mailbox(pending: &[Pending], skip: Option<usize>) -> Vec<String> {
    idle_texts(pending)
        .into_iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != skip)
        .map(|(_, t)| t)
        .collect()
}

fn steer_list(pending: &[Pending]) -> Vec<String> {
    pending
        .iter()
        .filter_map(|p| match p {
            Pending::Steer(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

fn interrupt_list(pending: &[Pending]) -> Vec<String> {
    pending
        .iter()
        .filter_map(|p| match p {
            Pending::Interrupt(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

fn sync_queue(ui: &mut Ui, pending: &[Pending]) -> Result<()> {
    ui.set_interrupts(interrupt_list(pending))?;
    ui.set_steers(steer_list(pending))?;
    ui.set_queue(idle_queue(pending))
}

fn take_composer(edit: &mut LineEdit) -> Option<String> {
    let text = edit.input.trim().to_string();
    if text.is_empty() {
        return None;
    }
    edit.set_input(String::new());
    edit.remember(&text);
    Some(text)
}

fn enqueue(tx: &MailboxTx, pending: &mut Vec<Pending>, text: String, slot: &mut Option<usize>) {
    if let Some(slot) = slot.take() {
        if !set_idle_text(pending, slot, text.clone()) {
            insert_idle(pending, Some(slot), text);
        }
        tx.set_idle(idle_texts(pending));
    } else {
        pending.push(Pending::Idle(text.clone()));
        tx.idle(text);
    }
}

fn insert_idle(pending: &mut Vec<Pending>, slot: Option<usize>, text: String) {
    let Some(display) = slot else {
        pending.push(Pending::Idle(text));
        return;
    };
    let positions: Vec<usize> = pending
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Pending::Idle(_)))
        .map(|(i, _)| i)
        .collect();
    let at = if display >= positions.len() {
        pending.len()
    } else {
        positions[display]
    };
    pending.insert(at, Pending::Idle(text));
}

fn set_idle_text(pending: &mut [Pending], display: usize, text: String) -> bool {
    let Some(idx) = idle_index(pending, display) else {
        return false;
    };
    pending[idx] = Pending::Idle(text);
    true
}

fn shift_slot(slot: usize, from: usize, to: usize) -> usize {
    if slot == from {
        to
    } else if from < to && slot > from && slot <= to {
        slot - 1
    } else if to < from && slot >= to && slot < from {
        slot + 1
    } else {
        slot
    }
}

fn take_idle_slot(pending: &mut Vec<Pending>, slot: Option<usize>) {
    if let Some(slot) = slot
        && let Some(idx) = idle_index(pending, slot)
    {
        pending.remove(idx);
    }
}

fn idle_index(pending: &[Pending], display: usize) -> Option<usize> {
    pending
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Pending::Idle(_)))
        .map(|(i, _)| i)
        .nth(display)
}

fn move_idle(pending: &mut Vec<Pending>, from: usize, to: usize) -> bool {
    if from == to {
        return false;
    }
    let positions: Vec<usize> = pending
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Pending::Idle(_)))
        .map(|(i, _)| i)
        .collect();
    if from >= positions.len() || to >= positions.len() {
        return false;
    }
    let from_i = positions[from];
    let item = pending.remove(from_i);
    let positions: Vec<usize> = pending
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, Pending::Idle(_)))
        .map(|(i, _)| i)
        .collect();
    let to_i = if to >= positions.len() {
        pending.len()
    } else {
        positions[to]
    };
    pending.insert(to_i, item);
    true
}

fn apply_queue(
    tx: &MailboxTx,
    ui: &mut Ui,
    edit: &mut LineEdit,
    pending: &mut Vec<Pending>,
    action: &KeyAction,
) -> Result<bool> {
    let changed = match *action {
        KeyAction::QueueSteer(i) => {
            if edit.queue_slot == Some(i) {
                edit.queue_slot = None;
                edit.set_input(String::new());
            } else if let Some(slot) = edit.queue_slot
                && slot > i
            {
                edit.queue_slot = Some(slot - 1);
            }
            idle_index(pending, i).is_some_and(|idx| {
                let text = pending.remove(idx).text().to_string();
                pending.push(Pending::Steer(text.clone()));
                tx.steer(text);
                true
            })
        }
        KeyAction::QueueUp(i) => {
            let ok = i > 0 && move_idle(pending, i, i - 1);
            if ok && let Some(slot) = edit.queue_slot {
                edit.queue_slot = Some(shift_slot(slot, i, i - 1));
            }
            ok
        }
        KeyAction::QueueDown(i) => {
            let ok = move_idle(pending, i, i + 1);
            if ok && let Some(slot) = edit.queue_slot {
                edit.queue_slot = Some(shift_slot(slot, i, i + 1));
            }
            ok
        }
        KeyAction::QueueEdit(i) => {
            if edit.queue_slot == Some(i) {
                false
            } else if let Some(idx) = idle_index(pending, i) {
                if let Some(slot) = edit.queue_slot.take() {
                    let draft = edit.input.trim().to_string();
                    if !draft.is_empty() {
                        set_idle_text(pending, slot, draft);
                    }
                }
                edit.set_input(pending[idx].text().to_string());
                edit.queue_slot = Some(i);
                true
            } else {
                false
            }
        }
        KeyAction::QueueDrop(i) => {
            if edit.queue_slot == Some(i) {
                edit.queue_slot = None;
                edit.set_input(String::new());
            } else if let Some(slot) = edit.queue_slot
                && slot > i
            {
                edit.queue_slot = Some(slot - 1);
            }
            idle_index(pending, i).is_some_and(|idx| {
                pending.remove(idx);
                true
            })
        }
        KeyAction::QueueMove { from, to } => {
            let ok = move_idle(pending, from, to);
            if ok && let Some(slot) = edit.queue_slot {
                edit.queue_slot = Some(shift_slot(slot, from, to));
            }
            ok
        }
        _ => false,
    };
    if changed {
        tx.set_idle(idle_mailbox(pending, edit.queue_slot));
        ui.set_queue_edit(edit.queue_slot);
        sync_queue(ui, pending)?;
        if matches!(
            *action,
            KeyAction::QueueEdit(_) | KeyAction::QueueSteer(_) | KeyAction::QueueDrop(_)
        ) {
            ui.set_input(&edit.input, edit.cursor)?;
        }
        if let KeyAction::QueueMove { to, .. } = *action {
            ui.begin_queue_drag(to);
        }
        match *action {
            KeyAction::QueueUp(i) => ui.highlight_queue(i - 1)?,
            KeyAction::QueueDown(i) => ui.highlight_queue(i + 1)?,
            KeyAction::QueueMove { to, .. } => ui.highlight_queue(to)?,
            _ => {}
        }
    }
    Ok(changed)
}

fn refresh_bar(ui: &mut Ui, status: &Status) -> Result<()> {
    let usage = status.usage;
    let inn = usage.input_tokens;
    let out = usage.output_tokens;
    let cached = usage.cached_tokens;
    let reasoning = usage.reasoning_tokens;
    let last = usage.last_input_tokens;
    let hit = if inn == 0 {
        0.0
    } else {
        (cached.min(inn) as f64 / inn as f64) * 100.0
    };
    let cost = estimate_cost(inn, cached, out);
    let ctx = if last == 0 {
        0.0
    } else {
        (last as f64 / CONTEXT_WINDOW as f64) * 100.0
    };
    ui.set_bar(Bar {
        workspace: short_path(&status.workspace),
        branch: status.branch.clone(),
        pull: status.pull.clone(),
        input: fmt_compact(inn),
        output: fmt_compact(out),
        reasoning: fmt_compact(reasoning),
        cache_hit: format!("{hit:.1}%"),
        cost: format!("${cost:.3}"),
        context: format!("{ctx:.1}%/{}", fmt_window(CONTEXT_WINDOW)),
        model: status.model.clone(),
        effort: status.effort.clone(),
    })
}

fn replay(ui: &mut Ui, session: &Session) -> Result<()> {
    let mut runs: Vec<ToolRun> = Vec::new();
    let mut pending: Vec<Call> = Vec::new();
    let flush = |ui: &mut Ui, runs: &mut Vec<ToolRun>| -> Result<()> {
        if runs.is_empty() {
            return Ok(());
        }
        paint(
            ui,
            &LogLine::Tools {
                runs: std::mem::take(runs),
            },
        )
    };
    let take_args = |pending: &mut Vec<Call>, id: &str| {
        if let Some(i) = pending.iter().position(|c| c.id == id) {
            pending.remove(i).args
        } else {
            empty_args()
        }
    };
    for e in &session.entries {
        match e {
            Entry::User { text } => {
                flush(ui, &mut runs)?;
                pending.clear();
                paint(ui, &LogLine::User(text.clone()))?;
            }
            Entry::Assistant {
                text,
                thinking: _,
                calls,
            } => {
                // Saved reasoning is history, not a live turn — don't reopen the thinking box.
                if !text.is_empty() {
                    flush(ui, &mut runs)?;
                    paint(ui, &LogLine::Text(text.clone()))?;
                }
                pending.clone_from(calls);
            }
            Entry::Tool(t) if t.content == "not run" => {
                let _ = take_args(&mut pending, &t.id);
            }
            Entry::Tool(t) if t.is_error => {
                let args = take_args(&mut pending, &t.id);
                runs.push(tool_fail(&t.name, &args, &t.content));
            }
            Entry::Tool(t) => {
                let args = take_args(&mut pending, &t.id);
                runs.push(tool_ok(&t.name, &args));
            }
        }
    }
    flush(ui, &mut runs)
}

fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(T[(a >> 2) as usize] as char);
        out.push(T[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(T[(c & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn copy_text(text: &str) {
    if text.is_empty() {
        return;
    }
    // Keep the system clipboard alive: on X11, dropping it loses the copy.
    if copy_system(text) {
        return;
    }
    // OSC 52 for remote/tmux when no local clipboard is available.
    copy_osc52(text);
}

fn copy_osc52(text: &str) {
    let payload = b64(text.as_bytes());
    let seq = if env::var_os("TMUX").is_some() {
        format!("\x1bPtmux;\x1b\x1b]52;c;{payload}\x07\x1b\\")
    } else {
        format!("\x1b]52;c;{payload}\x1b\\")
    };
    let mut out = io::stdout();
    let _ = write!(out, "{seq}");
    let _ = out.flush();
}

fn clipboard_slot() -> &'static Mutex<Option<arboard::Clipboard>> {
    static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    CLIPBOARD.get_or_init(|| Mutex::new(None))
}

fn copy_system(text: &str) -> bool {
    for _ in 0..2 {
        if copy_system_once(text).is_ok() {
            return true;
        }
        if let Ok(mut slot) = clipboard_slot().lock() {
            *slot = None;
        }
    }
    false
}

fn copy_system_once(text: &str) -> Result<()> {
    let mut slot = clipboard_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        *slot = Some(arboard::Clipboard::new()?);
    }
    let cb = slot.as_mut().context("clipboard")?;
    cb.set_text(text.to_string())?;
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
    {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        let _ = cb
            .set()
            .clipboard(LinuxClipboardKind::Primary)
            .text(text.to_string());
    }
    Ok(())
}

fn apply_ui(ui: &mut Ui, edit: &LineEdit, action: KeyAction) -> Result<KeyAction> {
    match action {
        KeyAction::Skip => {
            if !ui.asking() {
                ui.set_input(&edit.input, edit.cursor)?;
            }
            Ok(KeyAction::Skip)
        }
        KeyAction::Scroll(n) => {
            ui.scroll_by(n)?;
            Ok(KeyAction::Skip)
        }
        KeyAction::ScrollEnd => {
            ui.scroll_end()?;
            Ok(KeyAction::Skip)
        }
        KeyAction::ClickTools(x, y) => {
            if !ui.click_tools(x, y)? {
                ui.select_start(x, y)?;
            }
            Ok(KeyAction::Skip)
        }
        KeyAction::SelectDrag(x, y) => {
            ui.select_drag(x, y)?;
            Ok(KeyAction::Skip)
        }
        KeyAction::SelectEnd(x, y) => {
            if let Some(text) = ui.select_end(x, y)? {
                copy_text(&text);
                ui.flash_copied()?;
            }
            Ok(KeyAction::Skip)
        }
        KeyAction::Copy => {
            if let Some(text) = ui.selected_text() {
                copy_text(&text);
                ui.flash_copied()?;
            }
            Ok(KeyAction::Skip)
        }
        other => Ok(other),
    }
}

fn handle_resize(ui: &mut Ui, ev: &CEvent) -> Result<bool> {
    if let CEvent::Resize(cols, rows) = *ev {
        ui.resize(cols, rows)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

async fn run_tui(
    agent: &mut Agent,
    tx: MailboxTx,
    log_rx: Receiver<LogLine>,
) -> Result<()> {
    let mut ui = Ui::start()?;
    let mut events = spawn_events();
    let mut edit = LineEdit::from_session(&agent.session);
    let mut pending: Vec<Pending> = Vec::new();
    let mut status = Status {
        workspace: agent.workspace.clone(),
        model: agent.model.clone(),
        effort: agent.effort.clone(),
        branch: git_branch(&agent.workspace),
        pull: Some(resolve_home(&agent.workspace)),
        git_at: Some(Instant::now()),
        usage: agent.session.usage,
    };
    tui::set_git(git_dir(&agent.workspace).is_some());
    tui::set_origin(
        git_dir(&agent.workspace)
            .as_ref()
            .is_some_and(|p| git_has_origin(p)),
    );
    refresh_bar(&mut ui, &status)?;
    ui.batch(|ui| {
        if !agent.session.entries.is_empty() {
            paint(
                ui,
                &LogLine::Dim(format!(
                    "(resumed {}, {} entries)",
                    agent.session.path.display(),
                    agent.session.entries.len()
                )),
            )?;
        }
        replay(ui, &agent.session)
    })?;
    ui.set_working(false)?;
    ui.set_input(&edit.input, edit.cursor)?;
    let mut tick = tokio::time::interval(Duration::from_millis(80));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                flush_log(&mut ui, &mut status, &mut pending, &log_rx)?;
                sync_branch(&mut ui, &mut status)?;
                ui.tick()?;
            }
            ev = events.recv() => {
                let Some(ev) = ev else { return Ok(()); };
                if handle_resize(&mut ui, &ev)? {
                    continue;
                }
                let action = on_event(&mut ui, &mut edit, ev);
                if apply_queue(&tx, &mut ui, &mut edit, &mut pending, &action)? {
                    continue;
                }
                match apply_ui(&mut ui, &edit, action)? {
                    KeyAction::Quit => return Ok(()),
                    KeyAction::Abort => return Ok(()),
                    KeyAction::AskSubmit => {
                        let Some((template, origin)) = ui.take_ask() else {
                            ui.close_ask()?;
                            continue;
                        };
                        ui.close_ask()?;
                        let Some(text) = crate::config::fill_action(
                            &template,
                            status.pull.as_deref(),
                            status.branch.as_deref(),
                            Some(&origin),
                        ) else {
                            continue;
                        };
                        take_idle_slot(&mut pending, edit.queue_slot.take());
                        ui.set_queue_edit(None);
                        sync_queue(&mut ui, &pending)?;
                        ui.set_working(true)?;
                        paint(&mut ui, &LogLine::User(text.clone()))?;
                        if run_turn(
                            &tx,
                            &log_rx,
                            &mut ui,
                            &mut edit,
                            &mut status,
                            &mut pending,
                            &mut events,
                            &mut tick,
                            prompt(agent, text),
                        )
                        .await?
                        {
                            return Ok(());
                        }
                        refresh_bar(&mut ui, &status)?;
                        ui.set_working(false)?;
                        ui.set_input(&edit.input, edit.cursor)?;
                    }
                    action @ (KeyAction::Action(_)
                    | KeyAction::Confirm
                    | KeyAction::Interrupt) => {
                        if let Some(draft) = action_ask(&action, &status) {
                            ui.open_ask(
                                action_title(&action, &status),
                                draft,
                                "paste git@… or https://… then Enter",
                                "git@github.com:org/repo.git",
                            )?;
                            continue;
                        }
                        let Some(text) = action_prompt(&action, &status)
                            .or_else(|| take_composer(&mut edit))
                        else {
                            continue;
                        };
                        take_idle_slot(&mut pending, edit.queue_slot.take());
                        ui.set_queue_edit(None);
                        sync_queue(&mut ui, &pending)?;
                        ui.set_input(&edit.input, edit.cursor)?;
                        ui.set_working(true)?;
                        paint(&mut ui, &LogLine::User(text.clone()))?;
                        if run_turn(
                            &tx,
                            &log_rx,
                            &mut ui,
                            &mut edit,
                            &mut status,
                            &mut pending,
                            &mut events,
                            &mut tick,
                            prompt(agent, text),
                        )
                        .await?
                        {
                            return Ok(());
                        }
                        refresh_bar(&mut ui, &status)?;
                        ui.set_working(false)?;
                        ui.set_input(&edit.input, edit.cursor)?;
                    }
                    _ => {}
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
    tx: &MailboxTx,
    log_rx: &Receiver<LogLine>,
    ui: &mut Ui,
    edit: &mut LineEdit,
    status: &mut Status,
    pending: &mut Vec<Pending>,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<CEvent>,
    tick: &mut tokio::time::Interval,
    fut: impl std::future::Future<Output = Result<()>>,
) -> Result<bool> {
    tokio::pin!(fut);
    ui.set_working(true)?;
    loop {
        tokio::select! {
            result = &mut fut => {
                result?;
                flush_log(ui, status, pending, log_rx)?;
                return Ok(false);
            }
            _ = tick.tick() => {
                flush_log(ui, status, pending, log_rx)?;
                sync_branch(ui, status)?;
                ui.tick()?;
            }
            ev = events.recv() => {
                let Some(ev) = ev else { return Ok(true); };
                if handle_resize(ui, &ev)? {
                    continue;
                }
                let action = on_event(ui, edit, ev);
                if apply_queue(tx, ui, edit, pending, &action)? {
                    continue;
                }
                match apply_ui(ui, edit, action)? {
                    KeyAction::Quit => {
                        tx.abort();
                        return Ok(true);
                    }
                    KeyAction::AskSubmit => {
                        if let Some((template, origin)) = ui.take_ask() {
                            ui.close_ask()?;
                            if let Some(text) = crate::config::fill_action(
                                &template,
                                status.pull.as_deref(),
                                status.branch.as_deref(),
                                Some(&origin),
                            ) {
                                enqueue(tx, pending, text, &mut None);
                                sync_queue(ui, pending)?;
                            }
                        } else {
                            ui.close_ask()?;
                        }
                    }
                    action @ KeyAction::Action(_) => {
                        if let Some(draft) = action_ask(&action, status) {
                            ui.open_ask(
                                action_title(&action, status),
                                draft,
                                "paste git@… or https://… then Enter",
                                "git@github.com:org/repo.git",
                            )?;
                        } else if let Some(text) = action_prompt(&action, status) {
                            enqueue(tx, pending, text, &mut None);
                            sync_queue(ui, pending)?;
                        }
                    }
                    KeyAction::Abort => {
                        take_idle_slot(pending, edit.queue_slot.take());
                        restore_pending(edit, pending);
                        ui.set_queue_edit(None);
                        sync_queue(ui, pending)?;
                        ui.set_input(&edit.input, edit.cursor)?;
                        tx.abort();
                        (&mut fut).await?;
                        flush_log(ui, status, pending, log_rx)?;
                        return Ok(false);
                    }
                    KeyAction::Confirm => {
                        if let Some(text) = take_composer(edit) {
                            enqueue(tx, pending, text, &mut edit.queue_slot);
                            ui.set_queue_edit(None);
                            sync_queue(ui, pending)?;
                            ui.set_input(&edit.input, edit.cursor)?;
                        }
                    }
                    KeyAction::Interrupt => {
                        if let Some(text) = take_composer(edit) {
                            take_idle_slot(pending, edit.queue_slot.take());
                            pending.push(Pending::Interrupt(text.clone()));
                            tx.interrupt(text);
                            ui.set_queue_edit(None);
                            sync_queue(ui, pending)?;
                            ui.set_input(&edit.input, edit.cursor)?;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn restore_pending(edit: &mut LineEdit, pending: &mut Vec<Pending>) {
    edit.queue_slot = None;
    if pending.is_empty() {
        return;
    }
    let restored = pending
        .drain(..)
        .map(|p| p.text().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    if edit.input.is_empty() {
        edit.set_input(restored);
    } else {
        edit.set_input(format!("{}\n{restored}", edit.input));
    }
}

#[derive(Parser)]
#[command(name = "fun", version, about = "Fun coding agent")]
struct Cli {
    #[arg(long, global = true)]
    dir: Option<PathBuf>,
    #[arg(long, global = true)]
    new: bool,
    #[arg(long, global = true, value_name = "PATH")]
    session: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in with xAI device flow
    Login,
    /// Delete stored xAI tokens
    Logout,
    /// List sessions for this workspace
    ListSessions,
    /// One-shot prompt (any other first word is treated as the prompt)
    #[command(external_subcommand)]
    Prompt(Vec<String>),
}

fn workspace(cli: &Cli) -> Result<PathBuf> {
    match &cli.dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create --dir {}", dir.display()))?;
            dir.canonicalize()
                .with_context(|| format!("--dir {}", dir.display()))
        }
        None => env::current_dir().context("current directory"),
    }
}

fn open_session(cli: &Cli, workspace: &Path) -> Result<Session> {
    if let Some(path) = &cli.session {
        if !path.exists() {
            bail!("session not found: {}", path.display());
        }
        return Session::load(path.clone());
    }
    if cli.new {
        return Session::create(workspace);
    }
    Session::open_latest(workspace)?.map_or_else(|| Session::create(workspace), Ok)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::load();
    tui::set_palette(cfg.palette);
    tui::set_actions(cfg.actions);
    tui::set_home(cfg.home);
    match &cli.command {
        Some(Command::Login) => return login().await,
        Some(Command::Logout) => return logout().await,
        Some(Command::ListSessions) => {
            list_sessions(&workspace(&cli)?)?;
            return Ok(());
        }
        _ => {}
    }
    let asked = match &cli.command {
        Some(Command::Prompt(args)) => args.clone(),
        _ => Vec::new(),
    };
    let workspace = workspace(&cli)?;

    let (tx, mb) = mailbox();
    let (log_tx, log_rx) = mpsc::channel();
    let interactive = asked.is_empty();
    let (provider, model) = Grok::from_env().await?;
    let session = open_session(&cli, &workspace)?;
    let mut agent = Agent::new(
        workspace,
        model,
        provider,
        get_tools(),
        session,
        mb,
        log_tx,
    );

    if !interactive {
        let printer = std::thread::spawn(move || {
            while let Ok(line) = log_rx.recv() {
                print_log(&line);
            }
        });
        let result = prompt(&mut agent, asked.join(" ")).await;
        drop(agent);
        let _ = printer.join();
        return result;
    }
    let path = agent.session.path.clone();
    run_tui(&mut agent, tx, log_rx).await?;
    println!("(session saved to {})", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_only(texts: &[&str]) -> Vec<Pending> {
        texts
            .iter()
            .map(|t| Pending::Idle((*t).into()))
            .collect()
    }

    #[test]
    fn insert_idle_restores_original_slot() {
        let mut pending = idle_only(&["a", "c"]);
        insert_idle(&mut pending, Some(1), "b".into());
        assert_eq!(idle_texts(&pending), vec!["a", "b", "c"]);

        let mut pending = idle_only(&["b", "c"]);
        insert_idle(&mut pending, Some(0), "a".into());
        assert_eq!(idle_texts(&pending), vec!["a", "b", "c"]);

        let mut pending = idle_only(&["a", "b"]);
        insert_idle(&mut pending, Some(2), "c".into());
        assert_eq!(idle_texts(&pending), vec!["a", "b", "c"]);
    }

    #[test]
    fn insert_idle_keeps_display_order_with_other_kinds() {
        let mut pending = vec![
            Pending::Interrupt("stop".into()),
            Pending::Idle("a".into()),
            Pending::Steer("nudge".into()),
            Pending::Idle("c".into()),
        ];
        insert_idle(&mut pending, Some(1), "b".into());
        assert_eq!(idle_texts(&pending), vec!["a", "b", "c"]);
    }

    #[test]
    fn insert_idle_appends_without_slot() {
        let mut pending = idle_only(&["a"]);
        insert_idle(&mut pending, None, "b".into());
        assert_eq!(idle_texts(&pending), vec!["a", "b"]);
    }

    #[test]
    fn shift_slot_follows_moved_item() {
        assert_eq!(shift_slot(1, 1, 0), 0);
        assert_eq!(shift_slot(0, 1, 0), 1);
        assert_eq!(shift_slot(2, 0, 2), 1);
        assert_eq!(shift_slot(1, 0, 2), 0);
        assert_eq!(shift_slot(3, 1, 2), 3);
    }

    #[test]
    fn set_idle_text_updates_display_slot() {
        let mut pending = idle_only(&["a", "b", "c"]);
        assert!(set_idle_text(&mut pending, 1, "B".into()));
        assert_eq!(idle_texts(&pending), vec!["a", "B", "c"]);
    }

    #[test]
    fn steer_promotes_idle_item() {
        let mut pending = idle_only(&["a", "b", "c"]);
        assert_eq!(idle_index(&pending, 1), Some(1));
        let text = pending.remove(1).text().to_string();
        pending.push(Pending::Steer(text));
        assert_eq!(idle_texts(&pending), vec!["a", "c"]);
        assert_eq!(steer_list(&pending), vec!["b"]);
    }

    #[test]
    fn branch_from_head_reads_named_ref() {
        assert_eq!(
            branch_from_head("ref: refs/heads/master\n").as_deref(),
            Some("master")
        );
        assert_eq!(
            branch_from_head("ref: refs/heads/fix/demo-clicks-and-binary-read\n")
                .as_deref(),
            Some("fix/demo-clicks-and-binary-read")
        );
        assert_eq!(branch_from_head("948352c..."), None);
        assert_eq!(branch_from_head("ref: refs/tags/v1"), None);
        assert_eq!(
            branch_from_symref("ref: refs/remotes/origin/main\n").as_deref(),
            Some("main")
        );
        assert_eq!(
            pick_home_branch(&["main", "master"], Some("main"), "master"),
            "master"
        );
        assert_eq!(
            pick_home_branch(&["main"], Some("main"), "master"),
            "main"
        );
        assert_eq!(
            pick_home_branch(&["dev"], Some("main"), "master"),
            "dev"
        );
        assert_eq!(
            pick_home_branch(&[], Some("main"), "master"),
            "main"
        );
        assert_eq!(pick_home_branch(&[], None, "dev"), "dev");
    }

    #[test]
    fn commit_chip_uses_current_branch() {
        tui::set_actions(crate::config::Action::defaults());
        tui::set_origin(true);
        let on_home = Status {
            workspace: PathBuf::from("."),
            model: String::new(),
            effort: String::new(),
            branch: Some("master".into()),
            pull: Some("master".into()),
            git_at: None,
            usage: Usage::default(),
        };
        assert_eq!(
            action_prompt(&KeyAction::Action(1), &on_home).as_deref(),
            Some("commit and push")
        );
        let other = Status {
            branch: Some("feat".into()),
            ..on_home
        };
        assert_eq!(
            action_prompt(&KeyAction::Action(1), &other).as_deref(),
            Some("commit to feat and push")
        );
        assert_eq!(
            action_prompt(&KeyAction::Action(0), &other).as_deref(),
            Some("checkout and pull master")
        );
        let no_git = Status {
            branch: None,
            ..other
        };
        tui::set_origin(false);
        assert_eq!(action_prompt(&KeyAction::Action(1), &no_git), None);
        assert_eq!(
            action_ask(&KeyAction::Action(1), &no_git).as_deref(),
            Some(
                "init git on {home} if needed, add origin {origin}, then commit and push"
            )
        );
        assert_eq!(
            crate::config::fill_action(
                action_ask(&KeyAction::Action(1), &no_git).as_deref().unwrap(),
                no_git.pull.as_deref(),
                no_git.branch.as_deref(),
                Some("git@github.com:swimming-bookstore/connect-agent.git"),
            )
            .as_deref(),
            Some(
                "init git on master if needed, add origin git@github.com:swimming-bookstore/connect-agent.git, then commit and push"
            )
        );
    }
}
