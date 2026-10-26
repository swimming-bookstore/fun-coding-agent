mod tui;
mod ui;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use crossterm::event::{
    self, Event as CEvent, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use fun_core::agent::{
    Agent, LogLine, MailboxTx, ToolRun, UserTurn, mailbox, print_log, prompt_user, tool_fail,
    tool_ok,
};
use fun_core::grok::{self, login as grok_login, logout as grok_logout};
use fun_core::provider::Provider;
use fun_core::session::{
    Call, Entry, Image, Session, Usage, empty_args, list_sessions, load_media,
    looks_like_media_path, media_label, user_display,
};
use fun_core::tool::get_tools;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};
use tui::{Bar, ComposerAtom, QueueHit, Queued, Ui};

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
    Hover(u16, u16),
    QueueSteer(usize),
    QueueUp(usize),
    QueueDown(usize),
    QueueEdit(usize),
    QueueDrop(usize),
    QueueMove { from: usize, to: usize },
    ImageError(String),
    DropChip(usize),
    DropImage(usize),
    Action(usize),
    AskSubmit,
    OpenPrune(Vec<String>),
    ClosePrune,
    OpenModels,
    CloseModels,
    PickModel(String),
}

struct LineEdit {
    atoms: Vec<ComposerAtom>,
    chips: Vec<String>,
    images: Vec<Image>,
    cursor: usize,
    history: Vec<String>,
    idx: usize,
    draft: String,
    queue_slot: Option<usize>,
}

const PASTE_CHIP_CHARS: usize = 120;
const PASTE_CHIP_LINES: usize = 3;

impl LineEdit {
    fn from_session(session: &Session) -> Self {
        let history: Vec<String> = session
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::User { text, .. } if !text.is_empty() => Some(text.clone()),
                _ => None,
            })
            .collect();
        let idx = history.len();
        Self {
            atoms: Vec::new(),
            chips: Vec::new(),
            images: Vec::new(),
            cursor: 0,
            history,
            idx,
            draft: String::new(),
            queue_slot: None,
        }
    }

    fn set_input(&mut self, text: String) {
        self.chips.clear();
        self.images.clear();
        self.atoms = text.chars().map(ComposerAtom::Char).collect();
        self.cursor = self.atoms.len();
    }

    fn load_turn(&mut self, turn: UserTurn) {
        self.set_input(turn.text);
        for image in turn.images {
            let label = image.chip_label();
            self.images.push(image);
            self.atoms.push(ComposerAtom::Chip {
                index: self.images.len() - 1,
                label,
                image: true,
            });
        }
        self.cursor = self.atoms.len();
    }

    fn remember(&mut self, text: &str) {
        self.history.push(text.to_string());
        self.idx = self.history.len();
        self.draft.clear();
    }

    fn insert(&mut self, ch: char) {
        self.atoms.insert(self.cursor, ComposerAtom::Char(ch));
        self.cursor += 1;
    }

    fn paste(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let cleaned: String = s
            .chars()
            .filter(|c| *c == '\n' || !c.is_control())
            .collect();
        if cleaned.is_empty() {
            return;
        }
        if is_large_paste(&cleaned) {
            let index = self.chips.len();
            self.chips.push(cleaned.clone());
            self.atoms.insert(
                self.cursor,
                ComposerAtom::Chip {
                    index,
                    label: paste_chip_label(&cleaned),
                    image: false,
                },
            );
            self.cursor += 1;
            return;
        }
        for c in cleaned.chars() {
            self.insert(c);
        }
    }

    fn attach_image(&mut self, path: &Path) -> Result<()> {
        let mut image = load_media(path)?;
        let label = media_label(&path.display().to_string(), image.kind());
        image.name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(image.kind())
            .to_string();
        let index = self.images.len();
        self.images.push(image);
        self.atoms.insert(
            self.cursor,
            ComposerAtom::Chip {
                index,
                label,
                image: true,
            },
        );
        self.cursor += 1;
        Ok(())
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        self.remove_at(self.cursor);
    }

    fn delete(&mut self) {
        if self.cursor >= self.atoms.len() {
            return;
        }
        self.remove_at(self.cursor);
    }

    fn kill_word(&mut self) {
        let from = word_left_atoms(&self.atoms, self.cursor);
        while self.cursor > from {
            self.backspace();
        }
    }

    fn drop_chip(&mut self, index: usize) {
        self.drop_marked(index, false);
    }

    fn drop_image(&mut self, index: usize) {
        self.drop_marked(index, true);
    }

    fn drop_marked(&mut self, index: usize, image: bool) {
        let removed_before = self
            .atoms
            .iter()
            .take(self.cursor)
            .filter(|atom| {
                matches!(
                    atom,
                    ComposerAtom::Chip {
                        index: n,
                        image: kind,
                        ..
                    } if *n == index && *kind == image
                )
            })
            .count();
        self.atoms.retain(|atom| match atom {
            ComposerAtom::Chip {
                index: n,
                image: kind,
                ..
            } => !(*n == index && *kind == image),
            _ => true,
        });
        if image {
            self.images = remaining_images(&self.atoms, &self.images);
        } else {
            self.chips = remaining_chips(&self.atoms, &self.chips);
        }
        reindex_chips(&mut self.atoms);
        self.cursor = self
            .cursor
            .saturating_sub(removed_before)
            .min(self.atoms.len());
    }

    fn remove_at(&mut self, i: usize) {
        match self.atoms.get(i) {
            Some(ComposerAtom::Chip { index, image, .. }) => {
                if *image {
                    self.drop_image(*index);
                } else {
                    self.drop_chip(*index);
                }
            }
            Some(ComposerAtom::Char(_)) => {
                self.atoms.remove(i);
            }
            None => {}
        }
    }

    fn text(&self) -> String {
        let mut out = String::new();
        for atom in &self.atoms {
            match atom {
                ComposerAtom::Char(c) => out.push(*c),
                ComposerAtom::Chip { index, image, .. } if !image => {
                    if let Some(body) = self.chips.get(*index) {
                        if !out.is_empty() && !out.ends_with('\n') && !body.starts_with('\n') {
                            out.push('\n');
                        }
                        out.push_str(body);
                        if !body.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                }
                ComposerAtom::Chip { .. } => {}
            }
        }
        out
    }

    #[cfg(test)]
    fn preview(&self) -> String {
        let mut out = String::new();
        for atom in &self.atoms {
            match atom {
                ComposerAtom::Char(c) => out.push(*c),
                ComposerAtom::Chip { label, .. } => {
                    if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                        out.push(' ');
                    }
                    out.push_str(label);
                    out.push(' ');
                }
            }
        }
        out
    }

    fn take_images(&mut self) -> Vec<Image> {
        let images = std::mem::take(&mut self.images);
        self.atoms
            .retain(|atom| !matches!(atom, ComposerAtom::Chip { image: true, .. }));
        reindex_chips(&mut self.atoms);
        if self.cursor > self.atoms.len() {
            self.cursor = self.atoms.len();
        }
        images
    }

    fn is_empty(&self) -> bool {
        self.atoms.is_empty()
    }

    fn clear(&mut self) {
        self.atoms.clear();
        self.chips.clear();
        self.images.clear();
        self.cursor = 0;
    }
}

fn is_large_paste(s: &str) -> bool {
    s.chars().count() >= PASTE_CHIP_CHARS || s.lines().count() > PASTE_CHIP_LINES
}

fn paste_chip_label(s: &str) -> String {
    let lines = s.lines().count().max(1);
    let chars = s.chars().count();
    if lines > 1 {
        format!("paste {lines} lines")
    } else {
        format!("paste {chars} chars")
    }
}

fn clipboard_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

fn looks_like_image_path(path: &str) -> bool {
    looks_like_media_path(path)
}

fn word_left_atoms(atoms: &[ComposerAtom], mut i: usize) -> usize {
    i = i.min(atoms.len());
    if i == 0 {
        return 0;
    }
    i -= 1;
    if matches!(atoms[i], ComposerAtom::Chip { .. }) {
        return i;
    }
    while i > 0 && matches!(atoms[i], ComposerAtom::Char(c) if c.is_whitespace()) {
        i -= 1;
    }
    while i > 0 {
        match atoms[i.saturating_sub(1)] {
            ComposerAtom::Char(c) if c.is_whitespace() => break,
            ComposerAtom::Chip { .. } => break,
            _ => i -= 1,
        }
    }
    i
}

fn on_event(ui: &mut Ui, edit: &mut LineEdit, ev: CEvent) -> KeyAction {
    if ui.asking() {
        return on_ask(ui, ev);
    }
    if ui.picking_model() {
        return on_models(ui, ev);
    }
    if ui.pruning() {
        return on_prune(ui, ev);
    }
    if let CEvent::Paste(s) = ev {
        edit.paste(&s);
        return KeyAction::Skip;
    }
    if let CEvent::Mouse(mouse) = ev {
        return on_mouse(ui, mouse);
    }
    on_key(edit, ev)
}

fn on_models(ui: &mut Ui, ev: CEvent) -> KeyAction {
    if let CEvent::Mouse(mouse) = ev {
        return match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => ui
                .model_pick(mouse.column, mouse.row)
                .map(KeyAction::PickModel)
                .unwrap_or(KeyAction::CloseModels),
            _ => KeyAction::Skip,
        };
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
        KeyCode::Esc => KeyAction::CloseModels,
        _ => KeyAction::Skip,
    }
}

fn on_prune(ui: &mut Ui, ev: CEvent) -> KeyAction {
    if let CEvent::Mouse(mouse) = ev {
        return match mouse.kind {
            MouseEventKind::ScrollUp => {
                let _ = ui.prune_scroll(-3);
                KeyAction::Skip
            }
            MouseEventKind::ScrollDown => {
                let _ = ui.prune_scroll(3);
                KeyAction::Skip
            }
            MouseEventKind::Down(MouseButton::Left) => KeyAction::ClosePrune,
            _ => KeyAction::Skip,
        };
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
        KeyCode::Esc => KeyAction::ClosePrune,
        KeyCode::Up | KeyCode::PageUp => {
            let _ = ui.prune_scroll(if matches!(key.code, KeyCode::PageUp) {
                -8
            } else {
                -1
            });
            KeyAction::Skip
        }
        KeyCode::Down | KeyCode::PageDown => {
            let _ = ui.prune_scroll(if matches!(key.code, KeyCode::PageDown) {
                8
            } else {
                1
            });
            KeyAction::Skip
        }
        _ => KeyAction::Skip,
    }
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
        MouseEventKind::Down(MouseButton::Left) => {
            if ui.model_clicked(mouse.column, mouse.row) {
                return KeyAction::OpenModels;
            }
            if let Some((index, image)) = ui.chip_hit(mouse.column, mouse.row) {
                return if image {
                    KeyAction::DropImage(index)
                } else {
                    KeyAction::DropChip(index)
                };
            }
            match ui.action_hit(mouse.column, mouse.row) {
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
                    None => {
                        if let Some(lines) = ui.click_prune_note(mouse.column, mouse.row) {
                            KeyAction::OpenPrune(lines)
                        } else {
                            KeyAction::ClickTools(mouse.column, mouse.row)
                        }
                    }
                },
            }
        }
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
                KeyAction::Hover(mouse.column, mouse.row)
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
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'C'))
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
            KeyCode::Char('v') => {
                if let Some(text) = clipboard_text() {
                    let path = text.trim().trim_matches('"').trim();
                    if looks_like_image_path(path) && Path::new(path).is_file() {
                        if let Err(err) = edit.attach_image(Path::new(path)) {
                            return KeyAction::ImageError(err.to_string());
                        }
                    } else {
                        edit.paste(&text);
                    }
                }
                return KeyAction::Skip;
            }
            KeyCode::Char('a') => {
                edit.cursor = 0;
                return KeyAction::Skip;
            }
            KeyCode::Char('u') => {
                edit.atoms.drain(..edit.cursor);
                edit.chips = remaining_chips(&edit.atoms, &edit.chips);
                edit.images = remaining_images(&edit.atoms, &edit.images);
                reindex_chips(&mut edit.atoms);
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
            if edit.cursor > 0 {
                edit.cursor -= 1;
            }
            KeyAction::Skip
        }
        KeyCode::Right => {
            if edit.cursor < edit.atoms.len() {
                edit.cursor += 1;
            }
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
                edit.draft = edit.text();
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
    let mut hold = ui.take_new_chat();
    let mut inspect = ui.take_inspect();
    // opened is per log line, not per paint call. A User that arrives in the
    // same call as NewChat (replay) still gets the hint. A User painted on an
    // earlier call, before the decision, does not: that hold is still unset.
    let mut opened = false;
    let shown = shown_for(line, &mut inspect, &mut hold, &mut opened);
    ui.hold_new_chat_opt(hold);
    ui.hold_inspect(inspect);
    apply_shown(ui, shown)
}

fn apply_log(
    ui: &mut Ui,
    status: &mut Status,
    pending: &mut Vec<Pending>,
    line: LogLine,
) -> Result<()> {
    match &line {
        LogLine::Usage(usage) => {
            status.usage = *usage;
            refresh_bar(ui, status)?;
        }
        _ => {}
    }
    // Same painter as replay. Text, Dim, and Tools go through shown_for so a
    // live turn does not drop the reply or the saved-path hint.
    let before = ui.transcript_len();
    paint(ui, &line)?;
    match &line {
        LogLine::Delta(t) => ui.delta(t)?,
        LogLine::Think(t) => ui.think(t)?,
        LogLine::End => ui.end_stream()?,
        LogLine::Tools { runs } => ui.tools(runs.clone())?,
        _ => {}
    }
    // A new-chat reprint is the same text. Only the first paint, which grew
    // the transcript, consumes the queued copy. The reprint must not drop
    // a later queue item that happens to say the same thing.
    if let LogLine::User(text) = &line
        && ui.transcript_len() > before
        && let Some(i) = pending
            .iter()
            .position(|p| p.turn().display() == *text || p.text() == text)
    {
        pending.remove(i);
    }
    Ok(())
}

/// What a log line puts on screen, in order. A new chat clears, then the
/// next user line, then the saved-path hint under that line. A stay paints
/// the user line, then the drop note. The drop listing is attached to the
/// note, not painted as its own row.
///
/// `opened` is set when this line itself cleared the transcript. The hint
/// is attached only then, so an early paint of the same message (before the
/// decision) does not consume the saved path or sit in the chat about to
/// be wiped.
fn shown_for(
    line: &LogLine,
    inspect: &mut Option<Vec<String>>,
    new_chat: &mut Option<String>,
    opened: &mut bool,
) -> Vec<Shown> {
    match line {
        LogLine::NewChat { previous, user } => {
            *new_chat = Some(previous.display().to_string());
            *opened = true;
            let mut out = vec![Shown::Clear];
            // Replay and any caller that still fills `user` paints the opening
            // line in this same event. A live turn leaves it empty and reprints
            // with the next User log, which consumes the held path.
            if !user.trim().is_empty() {
                out.push(Shown::User(user.clone()));
                if let Some(path) = new_chat.take() {
                    out.push(Shown::Note {
                        text: format!("(new chat — previous session saved to {path})"),
                        inspect: None,
                    });
                    *opened = false;
                }
            }
            out
        }
        LogLine::Pruned { lines, .. } => {
            *inspect = Some(lines.clone());
            Vec::new()
        }
        LogLine::User(text) => {
            let mut out = vec![Shown::User(text.clone())];
            if *opened && let Some(path) = new_chat.take() {
                out.push(Shown::Note {
                    text: format!("(new chat — previous session saved to {path})"),
                    inspect: None,
                });
                *opened = false;
            }
            out
        }
        LogLine::Dim(text) => {
            let attached = if text.contains("dropped ") && text.contains("from context") {
                inspect.take()
            } else {
                None
            };
            vec![Shown::Note {
                text: text.clone(),
                inspect: attached,
            }]
        }
        LogLine::Text(t) => vec![Shown::Md(t.clone())],
        // Live turns paint Tools in apply_log after this returns empty.
        // Replay must not use paint() for tool rows — see replay().
        LogLine::Delta(_) | LogLine::Think(_) | LogLine::End | LogLine::Tools { .. } | LogLine::Usage(_) => {
            Vec::new()
        }
    }
}

fn apply_shown(ui: &mut Ui, shown: Vec<Shown>) -> Result<()> {
    let mut cleared = false;
    let mut painted = false;
    for item in shown {
        match item {
            Shown::Clear => {
                // The line painted before the decision belongs to the old
                // chat. Drop that trailing copy, then wipe. The saved path
                // lives in pending_new_chat, not in the transcript, so this
                // cannot eat the hint.
                ui.drop_trailing_user();
                ui.clear_transcript()?;
                cleared = true;
                painted = false;
            }
            Shown::User(text) => {
                ui.user(&text)?;
                painted = true;
            }
            Shown::Note { text, inspect } => {
                ui.note_inspect(&text, inspect)?;
                painted = true;
            }
            Shown::Md(text) => {
                ui.markdown(&text)?;
                painted = true;
            }
        }
    }
    // A clear does not redraw. With nothing after it the empty-body hint
    // ("ask anything about this workspace") never comes back.
    if cleared && !painted {
        ui.redraw_transcript()?;
    }
    Ok(())
}

#[derive(Debug)]
enum Shown {
    Clear,
    User(String),
    Note {
        text: String,
        inspect: Option<Vec<String>>,
    },
    Md(String),
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

fn git_packed_refs(git: &Path) -> Option<String> {
    fs::read_to_string(git.join("packed-refs")).ok()
}

fn packed_ref_exists(packed: &str, rel: &str) -> bool {
    let want = format!(" {rel}");
    packed.lines().any(|line| {
        let line = line.trim();
        !line.is_empty()
            && !line.starts_with('#')
            && !line.starts_with('^')
            && line.ends_with(&want)
    })
}

fn packed_peel(packed: &str, rel: &str) -> Option<String> {
    let suffix = format!(" {rel}");
    let mut lines = packed.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with('^') || !line.ends_with(&suffix) {
            continue;
        }
        if let Some(next) = lines.next() {
            let next = next.trim();
            if let Some(target) = next.strip_prefix('^') {
                return Some(target.trim().to_string());
            }
        }
        return None;
    }
    None
}

fn git_origin_head(git: &Path) -> Option<String> {
    if let Ok(text) = fs::read_to_string(git.join("refs/remotes/origin/HEAD"))
        && let Some(name) = branch_from_symref(&text)
    {
        return Some(name);
    }
    let packed = git_packed_refs(git)?;
    if let Some(target) = packed_peel(&packed, "refs/remotes/origin/HEAD") {
        return branch_from_symref(&format!("ref: {target}"))
            .or_else(|| target.rsplit('/').next().map(str::to_string));
    }
    None
}

fn git_ref_exists(git: &Path, rel: &str) -> bool {
    if git.join(rel).is_file() {
        return true;
    }
    git_packed_refs(git).is_some_and(|text| packed_ref_exists(&text, rel))
}

fn git_has_origin(git: &Path) -> bool {
    git.join("refs/remotes/origin").is_dir()
        || git.join("refs/remotes/origin/HEAD").is_file()
        || git_packed_refs(git).is_some_and(|text| {
            packed_ref_exists(&text, "refs/remotes/origin/HEAD")
                || text.lines().any(|line| {
                    let line = line.trim();
                    !line.starts_with('#') && line.contains(" refs/remotes/origin/")
                })
        })
        || fs::read_to_string(git.join("config"))
            .ok()
            .is_some_and(|text| {
                text.lines().any(|line| {
                    let line = line.trim();
                    line.eq_ignore_ascii_case("[remote \"origin\"]")
                })
            })
}

fn pick_home_branch(locals: &[&str], origin_head: Option<&str>, preferred: &str) -> String {
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

/// USD per million tokens. `input_tokens_above` is the prompt size that selects
/// the tier; the whole request uses that tier, same as pi's `calculateCost`.
struct Tier {
    input_tokens_above: u64,
    input_per_m: f64,
    cache_read_per_m: f64,
    output_per_m: f64,
}

struct Rates {
    input_per_m: f64,
    cache_read_per_m: f64,
    output_per_m: f64,
    tiers: &'static [Tier],
    window: u64,
}

fn rates_for(model: &str) -> Rates {
    let m = model.to_ascii_lowercase();
    // grok-4.7 catalog card. Above 200k prompt tokens the higher tier applies
    // to the whole request.
    if m.contains("grok") {
        return Rates {
            input_per_m: 2.0,
            cache_read_per_m: 0.5,
            output_per_m: 6.0,
            tiers: &[Tier {
                input_tokens_above: 200_000,
                input_per_m: 4.0,
                cache_read_per_m: 1.0,
                output_per_m: 12.0,
            }],
            window: 500_000,
        };
    }
    // Gemini 4 Argon is not in the catalog yet. Vertex list price, per 1M
    // tokens: $2.50 / $0.25 / $15 up to 200k prompt tokens, then $4.50 / $0.45
    // / $27 for the whole request.
    if m.contains("argon") || m.contains("gemini-4") {
        return Rates {
            input_per_m: 2.50,
            cache_read_per_m: 0.25,
            output_per_m: 15.0,
            tiers: &[Tier {
                input_tokens_above: 200_000,
                input_per_m: 4.50,
                cache_read_per_m: 0.45,
                output_per_m: 27.0,
            }],
            window: 1_000_000,
        };
    }
    // Public Gemini API prices from pi's google catalog (USD / 1M).
    if m.contains("2.5-flash") {
        return gemini_rates(0.30, 0.03, 2.50);
    }
    if m.contains("2.5-pro") || m.contains("2.5") {
        return gemini_rates(1.25, 0.125, 10.0);
    }
    if m.contains("3.1-pro") || m.contains("3-pro") {
        return gemini_rates(2.0, 0.2, 12.0);
    }
    if m.contains("3.5-flash") || m.contains("3.6-flash") || m.contains("3.7") || m.contains("3.8")
    {
        return gemini_rates(1.50, 0.15, 9.0);
    }
    if m.contains("flash") {
        return gemini_rates(0.50, 0.05, 3.0);
    }
    gemini_rates(2.0, 0.2, 12.0)
}

fn gemini_rates(input_per_m: f64, cache_read_per_m: f64, output_per_m: f64) -> Rates {
    Rates {
        input_per_m,
        cache_read_per_m,
        output_per_m,
        tiers: &[],
        window: 1_048_576,
    }
}

/// `input` is uncached prompt tokens. `cached` is cache-read tokens, billed on
/// their own rate and not subtracted from `input`. Output already includes
/// reasoning tokens.
#[cfg(test)]
fn estimate_cost(input: u64, cached: u64, output: u64, rates: &Rates) -> f64 {
    estimate_cost_for_prompt(input, cached, output, input.saturating_add(cached), rates)
}

fn estimate_cost_for_prompt(
    input: u64,
    cached: u64,
    output: u64,
    prompt: u64,
    rates: &Rates,
) -> f64 {
    let mut input_per_m = rates.input_per_m;
    let mut cache_read_per_m = rates.cache_read_per_m;
    let mut output_per_m = rates.output_per_m;
    let mut matched = 0u64;
    for tier in rates.tiers {
        if prompt > tier.input_tokens_above && tier.input_tokens_above >= matched {
            input_per_m = tier.input_per_m;
            cache_read_per_m = tier.cache_read_per_m;
            output_per_m = tier.output_per_m;
            matched = tier.input_tokens_above;
        }
    }
    (input as f64 / 1_000_000.0) * input_per_m
        + (cached as f64 / 1_000_000.0) * cache_read_per_m
        + (output as f64 / 1_000_000.0) * output_per_m
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
    pruned: Vec<String>,
}

enum Pending {
    Interrupt(UserTurn),
    Steer(UserTurn),
    Idle(UserTurn),
}

impl Pending {
    fn turn(&self) -> &UserTurn {
        match self {
            Self::Interrupt(t) | Self::Steer(t) | Self::Idle(t) => t,
        }
    }

    fn text(&self) -> &str {
        &self.turn().text
    }
}

fn idle_turns(pending: &[Pending]) -> Vec<UserTurn> {
    pending
        .iter()
        .filter_map(|p| match p {
            Pending::Idle(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

fn idle_texts(pending: &[Pending]) -> Vec<String> {
    idle_turns(pending)
        .into_iter()
        .map(|t| t.display())
        .collect()
}

fn idle_queue(pending: &[Pending]) -> Vec<Queued> {
    idle_texts(pending)
        .into_iter()
        .map(|text| Queued { text })
        .collect()
}

fn idle_mailbox(pending: &[Pending], skip: Option<usize>) -> Vec<UserTurn> {
    idle_turns(pending)
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
            Pending::Steer(t) => Some(t.display()),
            _ => None,
        })
        .collect()
}

fn interrupt_list(pending: &[Pending]) -> Vec<String> {
    pending
        .iter()
        .filter_map(|p| match p {
            Pending::Interrupt(t) => Some(t.display()),
            _ => None,
        })
        .collect()
}

fn sync_queue(ui: &mut Ui, pending: &[Pending]) -> Result<()> {
    ui.set_interrupts(interrupt_list(pending))?;
    ui.set_steers(steer_list(pending))?;
    ui.set_queue(idle_queue(pending))
}

fn remaining_chips(atoms: &[ComposerAtom], chips: &[String]) -> Vec<String> {
    atoms
        .iter()
        .filter_map(|atom| match atom {
            ComposerAtom::Chip {
                index,
                image: false,
                ..
            } => chips.get(*index).cloned(),
            _ => None,
        })
        .collect()
}

fn remaining_images(atoms: &[ComposerAtom], images: &[Image]) -> Vec<Image> {
    atoms
        .iter()
        .filter_map(|atom| match atom {
            ComposerAtom::Chip {
                index, image: true, ..
            } => images.get(*index).cloned(),
            _ => None,
        })
        .collect()
}

fn reindex_chips(atoms: &mut [ComposerAtom]) {
    let mut paste = 0usize;
    let mut image = 0usize;
    for atom in atoms {
        if let ComposerAtom::Chip {
            index, image: kind, ..
        } = atom
        {
            if *kind {
                *index = image;
                image += 1;
            } else {
                *index = paste;
                paste += 1;
            }
        }
    }
}

fn take_composer(edit: &mut LineEdit) -> Option<UserTurn> {
    let images = edit.take_images();
    let text = edit.text().trim().to_string();
    if text.is_empty() && images.is_empty() {
        return None;
    }
    edit.clear();
    let remembered = if text.is_empty() {
        match images.len() {
            1 => images[0].chip_label(),
            n => format!("{n} files"),
        }
    } else {
        text.clone()
    };
    edit.remember(&remembered);
    Some(UserTurn { text, images })
}

fn start_prompt(
    agent: &mut Agent,
    turn: UserTurn,
) -> impl std::future::Future<Output = Result<()>> + '_ {
    prompt_user(agent, turn)
}

fn composer_turn(edit: &LineEdit) -> UserTurn {
    UserTurn {
        text: edit.text().trim().to_string(),
        images: edit.images.clone(),
    }
}

fn enqueue(tx: &MailboxTx, pending: &mut Vec<Pending>, turn: UserTurn, slot: &mut Option<usize>) {
    if let Some(slot) = slot.take() {
        if !set_idle_text(pending, slot, turn.clone()) {
            insert_idle(pending, Some(slot), turn);
        }
        tx.set_idle(idle_turns(pending));
    } else {
        pending.push(Pending::Idle(turn.clone()));
        tx.idle(turn);
    }
}

fn insert_idle(pending: &mut Vec<Pending>, slot: Option<usize>, turn: UserTurn) {
    let Some(display) = slot else {
        pending.push(Pending::Idle(turn));
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
    pending.insert(at, Pending::Idle(turn));
}

fn set_idle_text(pending: &mut [Pending], display: usize, turn: UserTurn) -> bool {
    let Some(idx) = idle_index(pending, display) else {
        return false;
    };
    pending[idx] = Pending::Idle(turn);
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

fn paint_composer(ui: &mut Ui, edit: &LineEdit) -> Result<()> {
    ui.set_composer(edit.atoms.clone(), edit.cursor)
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
                let turn = pending.remove(idx).turn().clone();
                pending.push(Pending::Steer(turn.clone()));
                tx.steer(turn);
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
                    let draft = composer_turn(edit);
                    if !draft.text.trim().is_empty() || !draft.images.is_empty() {
                        set_idle_text(pending, slot, draft);
                    }
                }
                edit.load_turn(pending[idx].turn().clone());
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
            paint_composer(ui, edit)?;
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
    // Cache reads are split out of the prompt for pricing. The hit rate is
    // still cached / full prompt.
    let prompt = inn.saturating_add(cached);
    let hit = if prompt == 0 {
        0.0
    } else {
        (cached as f64 / prompt as f64) * 100.0
    };
    let rates = rates_for(&status.model);
    let tier_prompt = if last > 0 { last } else { prompt };
    let cost = estimate_cost_for_prompt(inn, cached, out, tier_prompt, &rates);
    let ctx = if last == 0 {
        0.0
    } else {
        (last as f64 / rates.window as f64) * 100.0
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
        context: format!("{ctx:.1}%/{}", fmt_window(rates.window)),
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
        // paint() does not draw Tools. Replay must call ui.tools or every
        // tool row vanishes on reopen.
        ui.tools(std::mem::take(runs))
    };
    let take_args = |pending: &mut Vec<Call>, id: &str| {
        if let Some(i) = pending.iter().position(|c| c.id == id) {
            pending.remove(i).args
        } else {
            empty_args()
        }
    };
    let mut notes = session.prune_notes.iter().peekable();
    for (i, e) in session.entries.iter().enumerate() {
        // A prune recorded at this index belongs under this entry. Paint it
        // after the entry so the drop note sits under the user line that
        // triggered it, not under the previous reply.
        match e {
            Entry::User { text, images } => {
                flush(ui, &mut runs)?;
                pending.clear();
                // Live paint uses user_display, which trims. Replay must strip the
                // same edge or a stored line with trailing space reprints long and
                // the drop note no longer sits under that line.
                paint(ui, &LogLine::User(user_display(text.trim(), images)))?;
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
        while notes.peek().is_some_and(|(at, _)| *at == i) {
            paint_prune_note(ui, session, &mut runs, notes.next().expect("peeked"))?;
        }
    }
    while notes.peek().is_some() {
        paint_prune_note(ui, session, &mut runs, notes.next().expect("peeked"))?;
    }
    flush(ui, &mut runs)
}

fn paint_prune_note(
    ui: &mut Ui,
    session: &Session,
    runs: &mut Vec<ToolRun>,
    note: &(usize, Vec<usize>),
) -> Result<()> {
    let ids = &note.1;
    if !runs.is_empty() {
        ui.tools(std::mem::take(runs))?;
    }
    let n = ids.len();
    let restored: Vec<usize> = ids
        .iter()
        .copied()
        .filter(|i| session.restored.contains(i))
        .collect();
    let r = restored.len();
    let text = if r == 0 {
        format!("(dropped {n} messages from context)")
    } else {
        format!("(dropped {n} messages from context, restored {r})")
    };
    ui.note_inspect(
        &text,
        Some(fun_core::session::prune_inspect_note(
            &session.entries,
            ids.iter().copied(),
            restored,
        )),
    )
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

thread_local! {
    static CLIPBOARD: std::cell::RefCell<Option<arboard::Clipboard>> =
        const { std::cell::RefCell::new(None) };
}

fn copy_system(text: &str) -> bool {
    CLIPBOARD.with(|slot| {
        for _ in 0..2 {
            if copy_system_once(&mut slot.borrow_mut(), text).is_ok() {
                return true;
            }
            *slot.borrow_mut() = None;
        }
        false
    })
}

fn copy_system_once(slot: &mut Option<arboard::Clipboard>, text: &str) -> Result<()> {
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

fn apply_ui(
    ui: &mut Ui,
    edit: &LineEdit,
    action: KeyAction,
    _status: &Status,
) -> Result<KeyAction> {
    match action {
        KeyAction::Skip => {
            if !ui.asking() && !ui.pruning() && !ui.picking_model() {
                paint_composer(ui, edit)?;
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
            if !ui.open_clicked_link(x, y) && !ui.click_tools(x, y)? {
                ui.select_start(x, y)?;
            }
            Ok(KeyAction::Skip)
        }
        KeyAction::Hover(x, y) => {
            ui.hover_at(x, y)?;
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
        KeyAction::DropChip(_) | KeyAction::DropImage(_) | KeyAction::ImageError(_) => {
            if !ui.asking() && !ui.pruning() && !ui.picking_model() {
                paint_composer(ui, edit)?;
            }
            Ok(KeyAction::Skip)
        }
        KeyAction::OpenPrune(lines) => {
            ui.open_prune(lines)?;
            Ok(KeyAction::Skip)
        }
        KeyAction::ClosePrune => {
            ui.close_prune()?;
            Ok(KeyAction::Skip)
        }
        KeyAction::OpenModels => {
            ui.open_model_picker()?;
            Ok(KeyAction::Skip)
        }
        KeyAction::CloseModels => {
            ui.close_model_picker()?;
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

async fn run_tui(agent: &mut Agent, tx: MailboxTx, log_rx: Receiver<LogLine>) -> Result<()> {
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
        pruned: fun_core::session::prune_inspect(&agent.session.entries, &agent.session.hidden),
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
            let n = agent.session.entries.len();
            let dropped = status.pruned.len();
            let msg = if dropped == 0 {
                format!("(resumed {}, {n} entries)", agent.session.path.display())
            } else {
                format!(
                    "(resumed {}, {n} entries, {dropped} pruned from context)",
                    agent.session.path.display()
                )
            };
            ui.note_inspect(
                &msg,
                if dropped == 0 {
                    None
                } else {
                    Some(status.pruned.clone())
                },
            )?;
        }
        replay(ui, &agent.session)
    })?;
    ui.set_working(false)?;
    paint_composer(&mut ui, &edit)?;
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
                if let KeyAction::DropChip(i) = action {
                    edit.drop_chip(i);
                }
                if let KeyAction::DropImage(i) = action {
                    edit.drop_image(i);
                }
                if let KeyAction::ImageError(ref err) = action {
                    let _ = ui.note(&format!("(image: {err})"));
                }
                if apply_queue(&tx, &mut ui, &mut edit, &mut pending, &action)? {
                    continue;
                }
                match apply_ui(&mut ui, &edit, action, &status)? {
                    KeyAction::Quit => return Ok(()),
                    KeyAction::Abort => return Ok(()),
                    KeyAction::PickModel(id) => {
                        ui.close_model_picker()?;
                        match switch_model(agent, &id).await {
                            Ok(()) => {
                                status.model = agent.model.clone();
                                status.effort = agent.effort.clone();
                                let _ = ui.note(&format!("(model {})", agent.model));
                            }
                            Err(err) => {
                                let _ = ui.note(&format!("(model: {err:#})"));
                            }
                        }
                        refresh_bar(&mut ui, &status)?;
                        paint_composer(&mut ui, &edit)?;
                    }
                    KeyAction::AskSubmit => {
                        let Some((template, origin)) = ui.take_ask() else {
                            ui.close_ask()?;
                            continue;
                        };
                        ui.close_ask()?;
                        let Some(text) = fun_core::config::fill_action(
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
                        // Not painted yet. A new chat clears the transcript; painting
                        // now would leave this line in the previous chat.
                        if run_turn(
                            &mut TurnLoop {
                                tx: &tx,
                                log_rx: &log_rx,
                                ui: &mut ui,
                                edit: &mut edit,
                                status: &mut status,
                                pending: &mut pending,
                                events: &mut events,
                                tick: &mut tick,
                            },
                            start_prompt(agent, UserTurn::text(text)),
                        )
                        .await?
                        {
                            return Ok(());
                        }
                        refresh_bar(&mut ui, &status)?;
                        ui.set_working(false)?;
                        paint_composer(&mut ui, &edit)?;
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
                        let Some(turn) = action_prompt(&action, &status)
                            .map(UserTurn::text)
                            .or_else(|| take_composer(&mut edit))
                        else {
                            continue;
                        };
                        take_idle_slot(&mut pending, edit.queue_slot.take());
                        ui.set_queue_edit(None);
                        sync_queue(&mut ui, &pending)?;
                        paint_composer(&mut ui, &edit)?;
                        ui.set_working(true)?;
                        // Same as AskSubmit: the User log paints this line after the
                        // new-chat decision, so a clear cannot leave it behind.
                        if run_turn(
                            &mut TurnLoop {
                                tx: &tx,
                                log_rx: &log_rx,
                                ui: &mut ui,
                                edit: &mut edit,
                                status: &mut status,
                                pending: &mut pending,
                                events: &mut events,
                                tick: &mut tick,
                            },
                            start_prompt(agent, turn),
                        )
                        .await?
                        {
                            return Ok(());
                        }
                        refresh_bar(&mut ui, &status)?;
                        ui.set_working(false)?;
                        paint_composer(&mut ui, &edit)?;
                    }
                    _ => {}
                }
            }
        }
    }
}

struct TurnLoop<'a> {
    tx: &'a MailboxTx,
    log_rx: &'a Receiver<LogLine>,
    ui: &'a mut Ui,
    edit: &'a mut LineEdit,
    status: &'a mut Status,
    pending: &'a mut Vec<Pending>,
    events: &'a mut tokio::sync::mpsc::UnboundedReceiver<CEvent>,
    tick: &'a mut tokio::time::Interval,
}

async fn run_turn(
    loop_: &mut TurnLoop<'_>,
    fut: impl std::future::Future<Output = Result<()>>,
) -> Result<bool> {
    let TurnLoop {
        tx,
        log_rx,
        ui,
        edit,
        status,
        pending,
        events,
        tick,
    } = loop_;
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
                if let KeyAction::DropChip(i) = action {
                    edit.drop_chip(i);
                }
                if let KeyAction::DropImage(i) = action {
                    edit.drop_image(i);
                }
                if let KeyAction::ImageError(ref err) = action {
                    let _ = ui.note(&format!("(image: {err})"));
                }
                if apply_queue(tx, ui, edit, pending, &action)? {
                    continue;
                }
                match apply_ui(ui, edit, action, status)? {
                    KeyAction::Quit => {
                        tx.abort();
                        return Ok(true);
                    }
                    KeyAction::AskSubmit => {
                        if let Some((template, origin)) = ui.take_ask() {
                            ui.close_ask()?;
                            if let Some(text) = fun_core::config::fill_action(
                                &template,
                                status.pull.as_deref(),
                                status.branch.as_deref(),
                                Some(&origin),
                            ) {
                                enqueue(tx, pending, UserTurn::text(text), &mut None);
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
                            enqueue(tx, pending, UserTurn::text(text), &mut None);
                            sync_queue(ui, pending)?;
                        }
                    }
                    KeyAction::Abort => {
                        take_idle_slot(pending, edit.queue_slot.take());
                        restore_pending(edit, pending);
                        ui.set_queue_edit(None);
                        sync_queue(ui, pending)?;
                        paint_composer(ui, edit)?;
                        tx.abort();
                    }
                    KeyAction::Confirm => {
                        if let Some(turn) = take_composer(edit) {
                            enqueue(tx, pending, turn, &mut edit.queue_slot);
                            ui.set_queue_edit(None);
                            sync_queue(ui, pending)?;
                            paint_composer(ui, edit)?;
                        }
                    }
                    KeyAction::Interrupt => {
                        if let Some(turn) = take_composer(edit) {
                            take_idle_slot(pending, edit.queue_slot.take());
                            pending.push(Pending::Interrupt(turn.clone()));
                            tx.interrupt(turn);
                            ui.set_queue_edit(None);
                            sync_queue(ui, pending)?;
                            paint_composer(ui, edit)?;
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
        .map(|p| p.turn().display())
        .collect::<Vec<_>>()
        .join("\n");
    if edit.is_empty() {
        edit.set_input(restored);
    } else {
        edit.set_input(format!("{}\n{restored}", edit.text()));
    }
}

#[cfg(test)]
mod cost_tests {
    use super::{estimate_cost, rates_for};

    #[test]
    fn cost_follows_catalog_formula() {
        let grok = rates_for("grok-4.7");
        // 100 uncached + 50 cache-read + 20 output, under the 200k tier.
        let low = estimate_cost(100, 50, 20, &grok);
        let expect = 100.0 / 1e6 * 2.0 + 50.0 / 1e6 * 0.5 + 20.0 / 1e6 * 6.0;
        assert!((low - expect).abs() < 1e-12);
        // Above 200k prompt tokens the higher tier covers the whole request.
        let high = estimate_cost(200_000, 1, 10, &grok);
        let expect = 200_000.0 / 1e6 * 4.0 + 1.0 / 1e6 * 1.0 + 10.0 / 1e6 * 12.0;
        assert!((high - expect).abs() < 1e-12);

        let argon = rates_for("gemini-4-argon");
        let low = estimate_cost(100_000, 0, 0, &argon);
        assert!((low - 0.25).abs() < 1e-12);
        let cached = estimate_cost(0, 100_000, 0, &argon);
        assert!((cached - 0.025).abs() < 1e-12);
        let high = estimate_cost(200_000, 1, 1_000_000, &argon);
        let expect = 200_000.0 / 1e6 * 4.50 + 1.0 / 1e6 * 0.45 + 27.0;
        assert!((high - expect).abs() < 1e-9);
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
    /// Attach an image, audio, video, PDF, or text file (repeatable, headless).
    /// Vertex reads all of these. xAI still only sends images.
    #[arg(long = "image", visible_alias = "file", global = true, value_name = "PATH")]
    images: Vec<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in. `fun login` is xAI. Vertex uses Application Default Credentials, not this command.
    Login {
        #[arg(value_name = "PROVIDER")]
        provider: Option<String>,
    },
    /// Delete stored tokens. `fun logout` deletes the xAI token file.
    Logout {
        #[arg(value_name = "PROVIDER")]
        provider: Option<String>,
    },
    /// List sessions for this workspace
    ListSessions,
    /// One-shot prompt (any other first word is treated as the prompt)
    #[command(external_subcommand)]
    Prompt(Vec<String>),
}

async fn login_cmd(provider: Option<&str>) -> Result<()> {
    match provider.unwrap_or("xai").trim().to_ascii_lowercase().as_str() {
        "" | "xai" | "grok" => grok_login().await,
        other => bail!("unknown provider {other} (try xai). Vertex uses gcloud application-default login."),
    }
}

async fn logout_cmd(provider: Option<&str>) -> Result<()> {
    match provider.unwrap_or("xai").trim().to_ascii_lowercase().as_str() {
        "" | "xai" | "grok" => grok_logout().await,
        other => bail!("unknown provider {other} (try xai)"),
    }
}

async fn open_provider(model: &str) -> Result<(Provider, String)> {
    let effort = grok::reasoning_effort();
    let provider = Provider::open(model, &effort).await?;
    Ok((provider, model.to_string()))
}

async fn switch_model(agent: &mut Agent, model: &str) -> Result<()> {
    if agent.model == model {
        return Ok(());
    }
    let effort = agent.effort.clone();
    let provider = Provider::open(model, &effort).await?;
    agent.set_model(model.to_string(), provider);
    Ok(())
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
    let cfg = fun_core::config::load();
    tui::set_palette(cfg.palette);
    tui::set_actions(cfg.actions);
    tui::set_home(cfg.home);
    if let Some(path) = cfg.auth {
        provider_grok::set_auth_path(path);
    }
    match &cli.command {
        Some(Command::Login { provider }) => return login_cmd(provider.as_deref()).await,
        Some(Command::Logout { provider }) => return logout_cmd(provider.as_deref()).await,
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
    let model = std::env::var("FUN_CODING_AGENT_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "grok-4.7".into());
    let (provider, model) = open_provider(&model).await?;
    let session = open_session(&cli, &workspace)?;
    let mut agent = Agent::new(workspace, model, provider, get_tools(), session, mb, log_tx);

    if !interactive {
        let printer = std::thread::spawn(move || {
            while let Ok(line) = log_rx.recv() {
                print_log(&line);
            }
        });
        let mut images = Vec::new();
        for path in &cli.images {
            images.push(load_media(path).with_context(|| format!("--image {}", path.display()))?);
        }
        let mut turn = UserTurn::text(asked.join(" "));
        turn.images = images;
        let result = prompt_user(&mut agent, turn).await;
        drop(agent);
        let _ = printer.join();
        return result;
    }
    let path = agent.session.path.clone();
    run_tui(&mut agent, tx, log_rx).await?;
    if agent.session.discard_if_empty()? {
        println!("(empty session discarded)");
    } else {
        println!("(session saved to {})", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle_only(texts: &[&str]) -> Vec<Pending> {
        texts
            .iter()
            .map(|t| Pending::Idle(UserTurn::text(*t)))
            .collect()
    }

    #[test]
    fn paste_keeps_newlines() {
        let mut edit = LineEdit {
            atoms: Vec::new(),
            chips: Vec::new(),
            images: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            idx: 0,
            draft: String::new(),
            queue_slot: None,
        };
        edit.paste("hello\r\nworld\n!");
        assert_eq!(edit.text(), "hello\nworld\n!");
        edit.paste("next");
        assert_eq!(edit.text(), "hello\nworld\n!next");
        edit.set_input(String::new());
        edit.paste("one\ntwo");
        assert_eq!(edit.text(), "one\ntwo");
        let text = take_composer(&mut edit).unwrap();
        assert_eq!(text.text, "one\ntwo");
        assert!(edit.is_empty());
    }

    #[test]
    fn large_paste_becomes_chip() {
        let mut edit = LineEdit {
            atoms: Vec::new(),
            chips: Vec::new(),
            images: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            idx: 0,
            draft: String::new(),
            queue_slot: None,
        };
        edit.paste("hi ");
        edit.paste("line1\nline2\nline3\nline4");
        assert_eq!(edit.chips.len(), 1);
        assert!(matches!(
            edit.atoms.last(),
            Some(ComposerAtom::Chip { index: 0, .. })
        ));
        assert!(edit.preview().contains("paste 4 lines"));
        assert_eq!(edit.text(), "hi \nline1\nline2\nline3\nline4\n");
        edit.drop_chip(0);
        assert!(edit.chips.is_empty());
        assert_eq!(edit.text(), "hi ");
        let sent = take_composer(&mut edit).unwrap();
        assert_eq!(sent.text, "hi");
    }

    #[test]
    fn insert_idle_restores_original_slot() {
        let mut pending = idle_only(&["a", "c"]);
        insert_idle(&mut pending, Some(1), UserTurn::text("b"));
        assert_eq!(idle_texts(&pending), vec!["a", "b", "c"]);

        let mut pending = idle_only(&["b", "c"]);
        insert_idle(&mut pending, Some(0), UserTurn::text("a"));
        assert_eq!(idle_texts(&pending), vec!["a", "b", "c"]);

        let mut pending = idle_only(&["a", "b"]);
        insert_idle(&mut pending, Some(2), UserTurn::text("c"));
        assert_eq!(idle_texts(&pending), vec!["a", "b", "c"]);
    }

    #[test]
    fn insert_idle_keeps_display_order_with_other_kinds() {
        let mut pending = vec![
            Pending::Interrupt(UserTurn::text("stop")),
            Pending::Idle(UserTurn::text("a")),
            Pending::Steer(UserTurn::text("nudge")),
            Pending::Idle(UserTurn::text("c")),
        ];
        insert_idle(&mut pending, Some(1), UserTurn::text("b"));
        assert_eq!(idle_texts(&pending), vec!["a", "b", "c"]);
    }

    #[test]
    fn insert_idle_appends_without_slot() {
        let mut pending = idle_only(&["a"]);
        insert_idle(&mut pending, None, UserTurn::text("b"));
        assert_eq!(idle_texts(&pending), vec!["a", "b"]);
    }

    #[test]
    fn new_chat_paints_clear_then_message_then_hint() {
        let previous = PathBuf::from("/tmp/old.jsonl");
        let mut hold = None;
        let mut inspect = None;
        let mut opened = false;
        let cleared = shown_for(
            &LogLine::NewChat {
                previous: previous.clone(),
                user: String::new(),
            },
            &mut inspect,
            &mut hold,
            &mut opened,
        );
        assert_eq!(cleared.len(), 1);
        assert!(matches!(cleared[0], Shown::Clear));
        assert!(hold.as_deref().unwrap().ends_with("old.jsonl"));

        let opened_line = shown_for(
            &LogLine::User("why is the login form broken".into()),
            &mut inspect,
            &mut hold,
            &mut opened,
        );
        assert!(hold.is_none(), "hint must be consumed by the opening line");

        // An early paint, before the clear, must not eat the saved path.
        let mut early_hold = Some("/tmp/old.jsonl".into());
        let mut early_opened = false;
        let early = shown_for(
            &LogLine::User("why is the login form broken".into()),
            &mut inspect,
            &mut early_hold,
            &mut early_opened,
        );
        assert!(
            matches!(early.as_slice(), [Shown::User(_)]),
            "early paint is the message only"
        );
        assert!(early_hold.is_some(), "saved path waits for the reprint");
        match &opened_line[..] {
            [Shown::User(text), Shown::Note { text: note, inspect: None }] => {
                assert_eq!(text, "why is the login form broken");
                assert!(note.contains("new chat — previous session saved"));
                assert!(note.contains("old.jsonl"), "{note}");
            }
            other => panic!("opening line then hint, got {other:?}"),
        }
    }

    #[test]
    fn shown_for_does_not_draw_tool_rows() {
        // Replay used to send Tools through paint(). That returns nothing
        // here, so every tool row vanished on reopen. Replay calls ui.tools.
        let mut hold = None;
        let mut inspect = None;
        let mut opened = false;
        let shown = shown_for(
            &LogLine::Tools {
                runs: vec![tool_ok("bash", &empty_args())],
            },
            &mut inspect,
            &mut hold,
            &mut opened,
        );
        assert!(shown.is_empty());
    }

    #[test]
    fn stay_paints_message_then_drop_note() {
        let mut hold = None;
        let mut inspect = None;
        let mut opened = false;
        let user = shown_for(
            &LogLine::User("and the order seems switched".into()),
            &mut inspect,
            &mut hold,
            &mut opened,
        );
        assert!(matches!(user.as_slice(), [Shown::User(_)]));
        let listing = vec!["[3] assistant tool dump".into()];
        let hidden = shown_for(
            &LogLine::Pruned {
                n: 38,
                lines: listing.clone(),
            },
            &mut inspect,
            &mut hold,
            &mut opened,
        );
        assert!(hidden.is_empty(), "the listing is not its own row");
        let note = shown_for(
            &LogLine::Dim("(dropped 38 messages from context, restored 5)".into()),
            &mut inspect,
            &mut hold,
            &mut opened,
        );
        match &note[..] {
            [Shown::Note { text, inspect: Some(lines) }] => {
                assert!(text.starts_with("(dropped 38"));
                assert_eq!(lines, &listing);
            }
            other => panic!("drop note under the message, got {other:?}"),
        }
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
        assert!(set_idle_text(&mut pending, 1, UserTurn::text("B")));
        assert_eq!(idle_texts(&pending), vec!["a", "B", "c"]);
    }

    #[test]
    fn steer_promotes_idle_item() {
        let mut pending = idle_only(&["a", "b", "c"]);
        assert_eq!(idle_index(&pending, 1), Some(1));
        let text = pending.remove(1).turn().clone();
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
            branch_from_head("ref: refs/heads/fix/demo-clicks-and-binary-read\n").as_deref(),
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
        assert_eq!(pick_home_branch(&["main"], Some("main"), "master"), "main");
        assert_eq!(pick_home_branch(&["dev"], Some("main"), "master"), "dev");
        assert_eq!(pick_home_branch(&[], Some("main"), "master"), "main");
        assert_eq!(pick_home_branch(&[], None, "dev"), "dev");
        let packed = "# pack-refs with: peeled fully-peeled sorted\n\
                      1111111111111111111111111111111111111111 refs/heads/master\n\
                      2222222222222222222222222222222222222222 refs/remotes/origin/HEAD\n\
                      ^refs/remotes/origin/master\n";
        assert!(packed_ref_exists(packed, "refs/heads/master"));
        assert!(!packed_ref_exists(packed, "refs/heads/main"));
        assert_eq!(
            packed_peel(packed, "refs/remotes/origin/HEAD").as_deref(),
            Some("refs/remotes/origin/master")
        );
    }

    #[test]
    fn commit_chip_opens_new_branch() {
        tui::set_actions(fun_core::config::Action::defaults());
        tui::set_origin(true);
        let on_home = Status {
            workspace: PathBuf::from("."),
            model: String::new(),
            effort: String::new(),
            branch: Some("master".into()),
            pull: Some("master".into()),
            git_at: None,
            usage: Usage::default(),
            pruned: Vec::new(),
        };
        assert_eq!(
            action_prompt(&KeyAction::Action(1), &on_home).as_deref(),
            Some("commit and push a new branch")
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
            Some("init git on {home} if needed, add origin {origin}, then commit and push")
        );
        assert_eq!(
            fun_core::config::fill_action(
                action_ask(&KeyAction::Action(1), &no_git)
                    .as_deref()
                    .unwrap(),
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
