use crate::provider::{CompleteReq, Provider};
use crate::session::{Call, Entry, Image, Session, ToolResult, Usage};
use crate::tool::{Abort, Tool, clip_utf8, execute_tool, is_abort};
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

const MAX_TOOL_ROUNDS: usize = 200;
/// Grok 4.7 sometimes locks onto one sentence and streams it forever. Stop
/// the turn once the same phrase has been emitted this many times so a queued
/// message can start instead of draining credits.
const REPEAT_MIN_UNIT: usize = 24;
const REPEAT_MIN_SPAN: usize = 160;
const REPEAT_HITS: usize = 6;
/// A stuck stream can also be nothing but newlines (or other whitespace).
/// Ordinary wrapping never emits this many in a row.
const REPEAT_WS_RUN: usize = 48;
/// Paraphrase loops never copy a phrase exactly. After this much text with no
/// tool call, a tail that keeps restating itself is the same stuck stream.
const REPEAT_PARA_SPAN: usize = 900;
/// Cap how much of a stuck stream is kept. The detector only needs the tail,
/// and a multi-megabyte buffer is what makes the next turn expensive.
const REPEAT_BUF_MAX: usize = 12_000;
const LOOP_NOTE: &str = "\n(stopped: model output was repeating)\n";

/// Text plus optional images for one user injection.
#[derive(Clone, Debug)]
pub struct UserTurn {
    pub text: String,
    pub images: Vec<Image>,
}

impl UserTurn {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            images: Vec::new(),
        }
    }

    pub fn display(&self) -> String {
        crate::session::user_display(&self.text, &self.images)
    }
}
const NEW_CHAT_SYSTEM: &str = "\
You decide whether the next user message continues this chat or starts a different task. Reply with JSON only: {\"new\":true} or {\"new\":false}.\n\
{\"new\":true} saves this chat and sends only the new message. {\"new\":false} stays and then prunes old noise.\n\
The queued send after this decision is the new user message itself. Do not drop or rewrite it.\n\
true: a different task, a new bug, a new project, or a request that does not need this transcript.\n\
false: a follow-up, a correction, a tighter constraint, or an answer to the assistant. Also false when the new line would be wrong without the recent transcript.\n\
Same words as the transcript are not enough to stay. If it is a new task, true.\n\
No tools. Put the JSON in the reply itself, not only in thinking. No extra text.";

const PRUNE_SYSTEM: &str = "\
You prune a coding-agent transcript. Reply with JSON only: {\"drop\":[ids]}.\n\
The user stayed in this chat. Hide old noise from the next model send. The transcript on screen and on disk stays.\n\
Drop only ids in the Candidates section. Never drop Live or Keep.\n\
Do not drop the new user message. It is not in the listing yet, and the queue after it stays queued.\n\
A low id hides that message only, not everything after it.\n\
A compact restore stub is a reminder of a hidden write or edit, not proof the file exists. Do not treat it as the file body.\n\
No tools. Put the JSON in the reply itself, not only in thinking. No extra text.";

#[derive(Clone)]
pub struct ToolRun {
    pub name: String,
    pub args: String,
    pub detail: String,
    pub is_error: bool,
}

impl ToolRun {
    pub fn call_line(&self) -> String {
        format!("{}({})", self.name, self.args)
    }
}

pub enum LogLine {
    User(String),
    Text(String),
    Delta(String),
    Think(String),
    End,
    Tools { runs: Vec<ToolRun> },
    Dim(String),
    Usage(Usage),
    /// Context was fat enough that this user line started a new chat.
    /// `previous` is the session file that was saved before the screen cleared.
    /// `user` is the line that opened the chat, so the TUI can paint it after
    /// wiping the old transcript.
    NewChat { previous: PathBuf, user: String },
    /// Stay path: these ids were hidden from the next send. The screen keeps them.
    Pruned { n: usize, lines: Vec<String> },
}

enum MailCmd {
    Interrupt(UserTurn),
    Steer(UserTurn),
    Idle(UserTurn),
    SetIdle(Vec<UserTurn>),
    Adopt {
        session: Session,
        workspace: PathBuf,
    },
}

#[derive(Clone)]
pub struct MailboxTx {
    cmd: Sender<MailCmd>,
    abort: Arc<Abort>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

pub struct Mailbox {
    cmd: Receiver<MailCmd>,
    abort: Arc<Abort>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    interrupt: Vec<UserTurn>,
    steer: Vec<UserTurn>,
    idle: Vec<UserTurn>,
    adopt: Option<(Session, PathBuf)>,
}

pub fn mailbox() -> (MailboxTx, Mailbox) {
    let (cmd, rx) = mpsc::channel();
    let abort = Abort::new();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    (
        MailboxTx {
            cmd,
            abort: abort.clone(),
            cancel: cancel.clone(),
        },
        Mailbox {
            cmd: rx,
            abort,
            cancel,
            interrupt: Vec::new(),
            steer: Vec::new(),
            idle: Vec::new(),
            adopt: None,
        },
    )
}

impl MailboxTx {
    pub fn interrupt(&self, turn: UserTurn) {
        let _ = self.cmd.send(MailCmd::Interrupt(turn));
        self.abort.abort();
    }
    pub fn steer(&self, turn: UserTurn) {
        let _ = self.cmd.send(MailCmd::Steer(turn));
    }
    pub fn idle(&self, turn: UserTurn) {
        let _ = self.cmd.send(MailCmd::Idle(turn));
    }
    pub fn set_idle(&self, items: Vec<UserTurn>) {
        let _ = self.cmd.send(MailCmd::SetIdle(items));
    }
    pub fn adopt(&self, session: Session, workspace: PathBuf) {
        let _ = self.cmd.send(MailCmd::Adopt { session, workspace });
        self.abort.abort();
    }
    pub fn abort(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.abort.abort();
    }
}

impl Mailbox {
    fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn clear_abort(&self) {
        self.abort.clear();
    }
    fn clear_cancel(&self) {
        self.cancel
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.abort.clear();
    }
    fn drain(&mut self) {
        while let Ok(cmd) = self.cmd.try_recv() {
            match cmd {
                MailCmd::Interrupt(text) => self.interrupt.push(text),
                MailCmd::Steer(text) => self.steer.push(text),
                MailCmd::Idle(text) => self.idle.push(text),
                MailCmd::SetIdle(items) => self.idle = items,
                MailCmd::Adopt { session, workspace } => self.adopt = Some((session, workspace)),
            }
        }
    }
    fn take_interrupt(&mut self) -> Option<UserTurn> {
        self.drain();
        if self.interrupt.is_empty() {
            None
        } else {
            Some(self.interrupt.remove(0))
        }
    }
    fn take_steer(&mut self) -> Option<UserTurn> {
        self.drain();
        if self.steer.is_empty() {
            None
        } else {
            Some(self.steer.remove(0))
        }
    }
    fn take_idle(&mut self) -> Option<UserTurn> {
        self.drain();
        if self.idle.is_empty() {
            None
        } else {
            Some(self.idle.remove(0))
        }
    }
    fn take_adopt(&mut self) -> Option<(Session, PathBuf)> {
        self.drain();
        self.adopt.take()
    }
    fn discard(&mut self) {
        self.drain();
        self.interrupt.clear();
        self.steer.clear();
        self.idle.clear();
        self.adopt = None;
    }
}

pub struct Agent {
    pub workspace: PathBuf,
    pub model: String,
    pub effort: String,
    provider: Provider,
    tools: Vec<Tool>,
    pub session: Session,
    mailbox: Mailbox,
    log: Sender<LogLine>,
    /// Live-tail index of a new-chat decision already tried this turn. Skip until that tail moves.
    prune_stuck: Option<usize>,
}

impl Agent {
    pub fn set_model(&mut self, model: String, provider: Provider) {
        self.model = model;
        self.effort = provider.effort().to_string();
        self.provider = provider;
    }

    pub fn new(
        workspace: PathBuf,
        model: String,
        provider: Provider,
        tools: Vec<Tool>,
        session: Session,
        mailbox: Mailbox,
        log: Sender<LogLine>,
    ) -> Self {
        Self {
            workspace,
            model,
            effort: provider.effort().to_string(),
            provider,
            tools,
            session,
            mailbox,
            log,
            prune_stuck: None,
        }
    }

    fn emit(&self, line: LogLine) {
        let _ = self.log.send(line);
    }

    fn system(&self) -> String {
        format!(
            "You are Fun coding agent. Tools: read, write, edit, bash.\n\
             Read a file before editing it. Prefer edit for small changes.\n\
             Paths are relative to this workspace unless absolute. read, write, and edit may use paths outside the workspace.\n\
             User messages may include images, audio, video, PDF, and text files. Look at them when the user asks about an attachment. On Vertex, Gemini 4 Argon accepts those inputs.\n\
             bash times out after 30s; pass timeout (seconds, max 600) for longer commands. Verify with tools before claiming done. Keep replies short.\n\
             workspace: {}",
            self.workspace.display()
        )
    }
}

pub async fn prompt(a: &mut Agent, text: String) -> Result<()> {
    prompt_user(a, UserTurn::text(text)).await
}

pub async fn prompt_user(a: &mut Agent, turn: UserTurn) -> Result<()> {
    a.mailbox.clear_cancel();
    maybe_new_chat(a, &turn).await?;
    a.session.add(Entry::User {
        text: turn.text,
        images: turn.images,
    })?;
    let result = match agent_loop(a).await {
        Err(e) if is_abort(&e) => Ok(()),
        other => other,
    };
    if let Some((session, workspace)) = a.mailbox.take_adopt() {
        a.session = session;
        a.workspace = workspace;
        a.mailbox.clear_cancel();
        a.mailbox.clear_abort();
        return Ok(());
    }
    if a.mailbox.cancelled() {
        a.mailbox.discard();
        // End the stream first so the note is its own line, not glued to the reply.
        a.emit(LogLine::End);
        a.emit(LogLine::Dim("(aborted)".into()));
    }
    result
}

async fn agent_loop(a: &mut Agent) -> Result<()> {
    let mut tool_rounds = 0usize;
    loop {
        if a.mailbox.cancelled() {
            break;
        }
        if inject_interrupt_new(a).await? {
            a.mailbox.clear_abort();
            tool_rounds = 0;
            continue;
        }
        let system = a.system();
        let abort = a.mailbox.abort.clone();
        let model_entries = a.session.model_entries();
        let wire = crate::provider::wire_model(&a.model).to_string();
        let repeat = std::cell::RefCell::new(RepeatGuard::new());
        let reply = match a
            .provider
            .complete(
                CompleteReq {
                    model: &wire,
                    system: &system,
                    entries: &model_entries,
                    tools: &a.tools,
                    abort: &abort,
                },
                |d| {
                    if repeat.borrow_mut().push(d) {
                        abort.abort();
                    }
                    a.emit(LogLine::Delta(d.into()));
                },
                |d| {
                    if repeat.borrow_mut().push(d) {
                        abort.abort();
                    }
                    a.emit(LogLine::Think(d.into()));
                },
            )
            .await
        {
            Ok(r) => r,
            Err(e) if is_abort(&e) => {
                if repeat.borrow().tripped() {
                    finish_repeat(a, &repeat.borrow());
                    if inject_idle_new(a).await? {
                        tool_rounds = 0;
                        continue;
                    }
                    break;
                }
                if a.mailbox.cancelled() {
                    break;
                }
                if inject_interrupt_new(a).await? {
                    a.mailbox.clear_abort();
                    a.emit(LogLine::End);
                    tool_rounds = 0;
                    continue;
                }
                break;
            }
            Err(e) => return Err(e),
        };
        if a.mailbox.cancelled() {
            break;
        }
        a.emit(LogLine::End);
        if repeat.borrow().tripped() {
            finish_repeat(a, &repeat.borrow());
            if inject_idle_new(a).await? {
                tool_rounds = 0;
                continue;
            }
            break;
        }
        a.session.usage.add(
            reply.input_tokens,
            reply.output_tokens,
            reply.cached_tokens,
            reply.reasoning_tokens,
        );
        a.session.save_usage(a.session.usage)?;
        a.emit(LogLine::Usage(a.session.usage));
        let calls = if reply.truncated {
            Vec::new()
        } else {
            reply.calls
        };
        if reply.truncated {
            a.emit(LogLine::Text("\n(truncated; tool calls dropped)\n".into()));
        }
        a.session.add(Entry::Assistant {
            text: reply.text,
            thinking: reply.thinking,
            calls: calls.clone(),
        })?;
        if inject_interrupt_new(a).await? {
            finish_skipped_tools(a, &calls, 0)?;
            a.mailbox.clear_abort();
            tool_rounds = 0;
            continue;
        }
        if inject_steer_new(a).await? {
            finish_skipped_tools(a, &calls, 0)?;
            tool_rounds = 0;
            continue;
        }
        if !calls.is_empty() {
            if tool_rounds >= MAX_TOOL_ROUNDS {
                a.emit(LogLine::Text(
                    "\n(stopped after too many tool rounds)\n".into(),
                ));
                finish_skipped_tools(a, &calls, 0)?;
                break;
            }
            tool_rounds += 1;
            match run_tools(a, &calls).await? {
                ToolFlow::Continue => {
                    tool_rounds = 0;
                    continue;
                }
                ToolFlow::Stop => break,
                ToolFlow::Next => {}
            }
        }
        if inject_interrupt_new(a).await? {
            a.mailbox.clear_abort();
            tool_rounds = 0;
            continue;
        }
        if inject_steer_new(a).await? {
            tool_rounds = 0;
            continue;
        }
        if !calls.is_empty() {
            continue;
        }
        if inject_idle_new(a).await? {
            tool_rounds = 0;
            continue;
        }
        break;
    }
    Ok(())
}

enum ToolFlow {
    Next,
    Continue,
    Stop,
}

async fn run_tools(a: &mut Agent, calls: &[Call]) -> Result<ToolFlow> {
    for (i, c) in calls.iter().enumerate() {
        if a.mailbox.cancelled() {
            finish_skipped_tools(a, calls, i)?;
            return Ok(ToolFlow::Stop);
        }
        if inject_interrupt_new(a).await? {
            finish_skipped_tools(a, calls, i)?;
            a.mailbox.clear_abort();
            return Ok(ToolFlow::Continue);
        }
        let (content, is_error) = if let Some(tool) = a.tools.iter().find(|t| t.name == c.name) {
            match execute_tool(tool, &a.workspace, &c.args, &a.mailbox.abort).await {
                Ok(s) => (s, false),
                Err(e) if is_abort(&e) => {
                    finish_skipped_tools(a, calls, i)?;
                    if a.mailbox.cancelled() {
                        return Ok(ToolFlow::Stop);
                    }
                    if inject_interrupt_new(a).await? {
                        a.mailbox.clear_abort();
                        return Ok(ToolFlow::Continue);
                    }
                    return Ok(ToolFlow::Stop);
                }
                Err(e) => (format!("{e:#}"), true),
            }
        } else {
            (format!("unknown tool: {}", c.name), true)
        };
        if is_error {
            emit_tools(a, &[tool_fail(&c.name, &c.args, &content)]);
        } else {
            emit_tools(a, &[tool_ok(&c.name, &c.args)]);
        }
        a.session.add(Entry::Tool(ToolResult {
            id: c.id.clone(),
            name: c.name.clone(),
            content,
            is_error,
            restored: false,
        }))?;
        if inject_interrupt_new(a).await? {
            finish_skipped_tools(a, calls, i + 1)?;
            a.mailbox.clear_abort();
            return Ok(ToolFlow::Continue);
        }
        if inject_steer_new(a).await? {
            finish_skipped_tools(a, calls, i + 1)?;
            return Ok(ToolFlow::Continue);
        }
    }
    Ok(ToolFlow::Next)
}

fn emit_tools(a: &Agent, runs: &[ToolRun]) {
    if runs.is_empty() {
        return;
    }
    a.emit(LogLine::Tools {
        runs: runs.to_vec(),
    });
}

/// Rolling tail of streamed text and thinking. Trips when one phrase keeps
/// repeating, or when the stream is nothing but newlines.
struct RepeatGuard {
    buf: String,
    /// Consecutive whitespace chars at the end of the stream.
    ws_run: usize,
    tripped: bool,
}

impl RepeatGuard {
    fn new() -> Self {
        Self {
            buf: String::new(),
            ws_run: 0,
            tripped: false,
        }
    }

    fn tripped(&self) -> bool {
        self.tripped
    }

    fn text(&self) -> &str {
        &self.buf
    }

    /// Returns true the first time the stream looks stuck.
    fn push(&mut self, delta: &str) -> bool {
        if self.tripped || delta.is_empty() {
            return false;
        }
        self.buf.push_str(delta);
        if self.buf.len() > REPEAT_BUF_MAX {
            let mut start = self.buf.len() - REPEAT_BUF_MAX;
            while start < self.buf.len() && !self.buf.is_char_boundary(start) {
                start += 1;
            }
            self.buf.replace_range(..start, "");
        }
        if delta.chars().all(|c| c.is_whitespace()) {
            self.ws_run = self.ws_run.saturating_add(delta.chars().count());
        } else {
            let tail = delta
                .chars()
                .rev()
                .take_while(|c| c.is_whitespace())
                .count();
            self.ws_run = tail;
        }
        if self.ws_run >= REPEAT_WS_RUN || output_is_repeating(&self.buf) {
            self.tripped = true;
            return true;
        }
        false
    }
}

fn finish_repeat(a: &mut Agent, repeat: &RepeatGuard) {
    a.mailbox.clear_abort();
    a.emit(LogLine::End);
    a.emit(LogLine::Text(LOOP_NOTE.into()));
    let text = format!("{}{LOOP_NOTE}", repeat.text());
    if a.session
        .add(Entry::Assistant {
            text,
            thinking: String::new(),
            calls: Vec::new(),
        })
        .is_err()
    {
        return;
    }
}

/// True when the tail is the same phrase copied over itself, or a long
/// monologue that keeps restating one plan.
///
/// Grok 4.7's stuck stream is sometimes one sentence copied exactly, and
/// sometimes the same plan paraphrased (the ~build tar monologue). A unit has
/// to be long enough that ordinary code or lists do not trip the exact check.
fn output_is_repeating(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n < REPEAT_MIN_SPAN {
        return false;
    }
    let max_unit = (n / REPEAT_HITS).min(400);
    if max_unit >= REPEAT_MIN_UNIT {
        let tail = n.min(max_unit.saturating_mul(REPEAT_HITS).saturating_add(max_unit));
        let start = n - tail;
        for unit in REPEAT_MIN_UNIT..=max_unit {
            if unit_repeats(&chars[start..], unit) {
                return true;
            }
        }
    }
    n >= REPEAT_PARA_SPAN && paraphrase_loop(&chars)
}

fn unit_repeats(chars: &[char], unit: usize) -> bool {
    if unit < REPEAT_MIN_UNIT || chars.len() < unit * REPEAT_HITS {
        return false;
    }
    let end = chars.len();
    let pat_start = end - unit;
    // Blank lines and indented code are not a stuck phrase. A pure newline
    // flood is handled by the whitespace run on RepeatGuard.
    if chars[pat_start..end].iter().all(|c| c.is_whitespace()) {
        return false;
    }
    let mut hits = 1usize;
    let mut at = pat_start;
    while at >= unit {
        let prev = at - unit;
        if chars[prev..at] != chars[pat_start..end] {
            break;
        }
        hits += 1;
        at = prev;
        if hits >= REPEAT_HITS {
            return true;
        }
    }
    false
}

/// True when the tail keeps restating itself without copying a phrase exactly.
///
/// The ~build failure is a stream of sentences that share most of their words
/// ("I'll skip the padding and keep reading") while changing one or two.
/// A real answer also repeats words, so this only looks at a long tail, and
/// only trips when most of its sentences are near-copies of a recent one.
fn paraphrase_loop(chars: &[char]) -> bool {
    let n = chars.len();
    if n < REPEAT_PARA_SPAN {
        return false;
    }
    let text: String = chars[n - REPEAT_PARA_SPAN..].iter().collect();
    let words: Vec<Vec<String>> = sentences(&text).iter().map(|s| content_words(s)).collect();
    let mut usable = 0usize;
    let mut near = 0usize;
    for i in 0..words.len() {
        if words[i].len() < 6 {
            continue;
        }
        usable += 1;
        let mut best = 0usize;
        let from = i.saturating_sub(6);
        for prev in &words[from..i] {
            if prev.len() < 6 {
                continue;
            }
            best = best.max(word_overlap(&words[i], prev));
        }
        // 60% of the union. The stuck tar plan clears this on every other
        // sentence. A step that names a new field stays under it.
        if best >= 60 {
            near += 1;
        }
    }
    usable >= 8 && near * 5 >= usable * 3
}

fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            if start < i {
                let s = text[start..i].trim();
                if !s.is_empty() {
                    out.push(s);
                }
            }
            start = i + 1;
            i += 1;
            continue;
        }
        if matches!(bytes[i], b'.' | b'!' | b'?') {
            let boundary = i + 1 == bytes.len()
                || bytes[i + 1].is_ascii_whitespace()
                || bytes[i + 1] == b'"';
            if boundary && start < i {
                let s = text[start..i].trim();
                if !s.is_empty() {
                    out.push(s);
                }
                start = i + 1;
            }
        }
        i += 1;
    }
    if start < text.len() {
        let s = text[start..].trim();
        if !s.is_empty() {
            out.push(s);
        }
    }
    out
}

fn content_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(|w| w.to_ascii_lowercase())
        .collect()
}

/// Overlap of two word bags, as a percent of the union. Order does not matter,
/// so a paraphrase that moves a clause still counts.
fn word_overlap(a: &[String], b: &[String]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let mut shared = 0usize;
    let mut used = vec![false; b.len()];
    for w in a {
        if let Some(j) = b.iter().enumerate().position(|(j, x)| !used[j] && x == w) {
            used[j] = true;
            shared += 1;
        }
    }
    let union = a.len() + b.len() - shared;
    if union == 0 {
        return 0;
    }
    shared * 100 / union
}

async fn inject_interrupt_new(a: &mut Agent) -> Result<bool> {
    let text = a.mailbox.take_interrupt();
    inject_user_new(a, text).await
}

async fn inject_steer_new(a: &mut Agent) -> Result<bool> {
    let text = a.mailbox.take_steer();
    inject_user_new(a, text).await
}

async fn inject_idle_new(a: &mut Agent) -> Result<bool> {
    let text = a.mailbox.take_idle();
    inject_user_new(a, text).await
}

fn inject_user(a: &mut Agent, turn: Option<UserTurn>) -> Result<bool> {
    let Some(turn) = turn else {
        return Ok(false);
    };
    a.emit(LogLine::User(turn.display()));
    a.session.add(Entry::User {
        text: turn.text,
        images: turn.images,
    })?;
    Ok(true)
}

async fn inject_user_new(a: &mut Agent, turn: Option<UserTurn>) -> Result<bool> {
    let Some(turn) = turn else {
        return Ok(false);
    };
    // Same gate as a composer send. The queue itself is not dropped.
    maybe_new_chat(a, &turn).await?;
    inject_user(a, Some(turn))
}

/// Decide before the user line is stored. A new chat saves the previous
/// session and sends only this turn. Stay then prunes old noise from the
/// next send. Either way the queue is left alone.
async fn maybe_new_chat(a: &mut Agent, turn: &UserTurn) -> Result<()> {
    if a.session.entries.is_empty() {
        return Ok(());
    }
    // "new chat" is a request, not a follow-up. Do not wait until the
    // payload is fat, and do not send it to the coding model to greet.
    if wants_new_chat(&turn.text) {
        return start_new_chat(a, turn);
    }
    if a.session.should_prune().is_none() {
        return Ok(());
    }
    let live_from = a.session.prune_view().live_from();
    if a.prune_stuck == Some(live_from) {
        return Ok(());
    }
    match ask_new_chat(a, turn).await {
        Ok(false) => {
            prune_if_staying(a).await?;
            a.prune_stuck = Some(live_from);
        }
        Ok(true) => start_new_chat(a, turn)?,
        Err(e) if is_abort(&e) => return Err(e),
        Err(e) => {
            a.emit(LogLine::Dim(format!("(new chat skipped: {e})")));
            prune_if_staying(a).await?;
            a.prune_stuck = Some(live_from);
        }
    }
    Ok(())
}

fn start_new_chat(a: &mut Agent, turn: &UserTurn) -> Result<()> {
    let previous = a.session.fork()?;
    a.prune_stuck = None;
    a.emit(LogLine::NewChat {
        previous,
        user: turn.display(),
    });
    Ok(())
}

/// The user asked to start over. Not a model decision: a short session never
/// reaches the fat gate, so this has to fork on the words themselves.
///
/// A long message that merely mentions the feature ("the new chat gate is
/// broken") is a task in this chat, not a request to leave it.
fn wants_new_chat(text: &str) -> bool {
    let t = text.trim().to_ascii_lowercase();
    if t.is_empty() || t.len() > 80 || t.matches('\n').count() > 2 {
        return false;
    }
    const PHRASES: &[&str] = &[
        "new chat",
        "new session",
        "fresh chat",
        "start over",
        "start a new chat",
        "start new chat",
    ];
    PHRASES.iter().any(|p| t.contains(p))
}

async fn ask_new_chat(a: &mut Agent, turn: &UserTurn) -> Result<bool> {
    let prompt = format!(
        "Workspace: {}\n\n## Current task\n{}\n\n## New user message\n{}\n\nDoes the new message continue Current task, or is it a different task?\n",
        a.workspace.display(),
        task_brief(&a.session.entries),
        turn.display()
    );
    let reply = decision_call(a, NEW_CHAT_SYSTEM, &prompt, &turn.images).await?;
    // Gemini 4 Argon on low effort often puts the JSON only in thinking.
    // An empty answer must not look like a stay.
    let text = format!("{}\n{}", reply.text, reply.thinking);
    Ok(parse_new_chat(&text))
}

/// User lines and the latest assistant conclusion. Tool dumps make every
/// follow-up look like the live task, so the new-chat call never sees them.
fn task_brief(entries: &[Entry]) -> String {
    let mut users: Vec<&str> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::User { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .filter(|t| !t.trim().is_empty())
        .collect();
    if users.len() > 4 {
        users.drain(..users.len() - 4);
    }
    let mut out = String::new();
    for (n, text) in users.iter().enumerate() {
        out.push_str(&format!(
            "user {}: {}\n",
            n + 1,
            clip_utf8(text, 400)
        ));
    }
    if let Some(text) = entries.iter().rev().find_map(|e| match e {
        Entry::Assistant { text, calls, .. } if calls.is_empty() && !text.trim().is_empty() => {
            Some(text.as_str())
        }
        _ => None,
    }) {
        out.push_str(&format!("assistant: {}\n", clip_utf8(text, 400)));
    }
    if out.is_empty() {
        out.push_str("(no task yet)\n");
    }
    out
}

/// Stay path only. Hide named candidates from the next send. The new user
/// line is not stored yet, so it cannot be dropped. The screen is not cleared.
async fn prune_if_staying(a: &mut Agent) -> Result<()> {
    if a.session.should_prune().is_none() {
        return Ok(());
    }
    let view = a.session.prune_view();
    let (candidates, listing) = view.listing();
    if candidates.len() < 2 {
        return Ok(());
    }
    let prompt = format!(
        "Workspace: {}\nHide old noise. The new user message is not listed and must stay.\n\n{listing}\n",
        a.workspace.display()
    );
    let reply = match decision_call(a, PRUNE_SYSTEM, &prompt, &[]).await {
        Ok(reply) => {
            // Same model: the drop list can be thinking-only. Prefer the
            // channel that actually parses so a thought-only reply still hides.
            if crate::prune::parse_drop_ids(&reply.text, &candidates).is_empty()
                && !crate::prune::parse_drop_ids(&reply.thinking, &candidates).is_empty()
            {
                reply.thinking
            } else {
                reply.text
            }
        }
        Err(e) if is_abort(&e) => return Err(e),
        Err(e) => {
            a.emit(LogLine::Dim(format!("(prune skipped: {e})")));
            return Ok(());
        }
    };
    let drop = crate::prune::parse_drop_ids(&reply, &candidates);
    let hidden = a.session.prune_view().apply(&drop);
    let (added, restored) = a.session.save_hidden(hidden)?;
    if !added.is_empty() {
        a.session.usage.last_input_tokens = 0;
        a.session.save_usage(a.session.usage)?;
        a.emit(LogLine::Usage(a.session.usage));
    }
    let n = added.len();
    let lines = crate::prune::prune_inspect_note(
        &a.session.entries,
        added.iter().copied(),
        restored.iter().copied(),
    );
    let note = if n == 0 {
        "(nothing to drop)".to_string()
    } else if restored.is_empty() {
        format!("(dropped {n} messages from context)")
    } else {
        format!(
            "(dropped {n} messages from context, restored {})",
            restored.len()
        )
    };
    a.emit(LogLine::Pruned { n, lines });
    a.emit(LogLine::Dim(note));
    Ok(())
}

async fn decision_call(
    a: &mut Agent,
    system: &str,
    prompt: &str,
    images: &[crate::session::Image],
) -> Result<crate::grok::Reply> {
    let abort = a.mailbox.abort.clone();
    let wire = crate::provider::wire_model(&a.model).to_string();
    let reply = a
        .provider
        .complete_with_effort(
            CompleteReq {
                model: &wire,
                system,
                entries: &[Entry::User {
                    text: prompt.to_string(),
                    images: images.to_vec(),
                }],
                tools: &[],
                abort: &abort,
            },
            "low",
            |_| {},
            |_| {},
        )
        .await?;
    a.session.usage.add_prune(
        reply.input_tokens,
        reply.output_tokens,
        reply.cached_tokens,
        reply.reasoning_tokens,
    );
    a.session.save_usage(a.session.usage)?;
    a.emit(LogLine::Usage(a.session.usage));
    Ok(reply)
}

/// `{"new":false}` stays. `{"new":true}` starts a chat. A missing answer also
/// starts a chat: low-effort models often think the decision and leave the
/// reply empty, and an empty reply used to fall through to prune every time.
pub fn parse_new_chat(text: &str) -> bool {
    let trimmed = text.trim();
    let slice = trimmed
        .find('{')
        .and_then(|i| trimmed[i..].rfind('}').map(|j| &trimmed[i..i + j + 1]));
    let Some(slice) = slice else {
        return true;
    };
    match serde_json::from_str::<serde_json::Value>(slice)
        .ok()
        .and_then(|v| v.get("new").and_then(|n| n.as_bool()))
    {
        Some(v) => v,
        None => true,
    }
}

fn finish_skipped_tools(a: &mut Agent, calls: &[Call], from: usize) -> Result<()> {
    let rest = &calls[from..];
    if rest.is_empty() {
        return Ok(());
    }
    for c in rest {
        a.session.add(Entry::Tool(ToolResult {
            id: c.id.clone(),
            name: c.name.clone(),
            content: "not run".into(),
            is_error: true,
            restored: false,
        }))?;
    }
    Ok(())
}

pub fn title_case(name: &str) -> String {
    let mut c = name.chars();
    c.next().map_or_else(String::new, |ch| {
        ch.to_uppercase().collect::<String>() + c.as_str()
    })
}

pub fn ellipsize(s: &str, max: usize) -> String {
    let clip = clip_utf8(s, max);
    if clip.len() < s.len() {
        format!("{clip}…")
    } else {
        s.to_string()
    }
}

pub fn args_repr(v: &Value) -> String {
    let s = if let Some(map) = v.as_object() {
        map.iter()
            .map(|(k, val)| match val {
                Value::String(s) => format!("{k}={s:?}"),
                other => format!("{k}={other}"),
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        v.to_string()
    };
    ellipsize(&s, 100)
}

pub fn tool_ok(name: &str, args: &Value) -> ToolRun {
    ToolRun {
        name: title_case(name),
        args: args_repr(args),
        detail: String::new(),
        is_error: false,
    }
}

pub fn tool_fail(name: &str, args: &Value, content: &str) -> ToolRun {
    let body = content.trim();
    let body = if body.is_empty() { "failed" } else { body };
    ToolRun {
        name: title_case(name),
        args: args_repr(args),
        detail: ellipsize(body, 400),
        is_error: true,
    }
}

pub fn tool_counts(runs: &[ToolRun]) -> (usize, usize) {
    let mut ok = 0usize;
    let mut fail = 0usize;
    for r in runs {
        if r.is_error {
            fail += 1;
        } else {
            ok += 1;
        }
    }
    (ok, fail)
}

fn count_label(n: usize, singular: &str, plural: &str) -> String {
    if n == 1 {
        format!("1 {singular}")
    } else {
        format!("{n} {plural}")
    }
}

pub fn tool_summary(ok: usize, fail: usize) -> String {
    match (ok, fail) {
        (0, 0) => String::new(),
        (n, 0) => format!("{} succeeded", count_label(n, "tool", "tools")),
        (0, n) => format!("{} failed", count_label(n, "tool", "tools")),
        (n, f) => format!(
            "{} succeeded  {} failed",
            count_label(n, "tool", "tools"),
            count_label(f, "tool", "tools")
        ),
    }
}

pub fn print_log(line: &LogLine) {
    use std::io::{self, Write};
    match line {
        LogLine::Text(t) => {
            if t.trim() == "(aborted)" {
                println!();
            }
            print!("{t}");
            if !t.ends_with('\n') {
                println!();
            }
            let _ = io::stdout().flush();
        }
        LogLine::Delta(t) => {
            print!("{t}");
            let _ = io::stdout().flush();
        }
        LogLine::Think(_) | LogLine::User(_) | LogLine::Usage(_) | LogLine::NewChat { .. } | LogLine::Pruned { .. } => {}
        LogLine::Dim(t) => {
            if t.trim() == "(aborted)" {
                println!("\n(aborted)");
                let _ = io::stdout().flush();
            }
        }
        LogLine::End => println!(),
        LogLine::Tools { runs } => {
            let (ok, fail) = tool_counts(runs);
            let summary = tool_summary(ok, fail);
            if !summary.is_empty() {
                eprintln!("  {summary}");
            }
            if !summary.is_empty() && fail > 0 {
                eprintln!();
            }
            for r in runs {
                if !r.is_error {
                    continue;
                }
                eprintln!("  ⏺ {}", r.call_line());
                for line in r.detail.lines() {
                    eprintln!("    {line}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupt_then_steer_then_idle() {
        let (tx, mut mb) = mailbox();
        tx.idle(UserTurn::text("i1"));
        tx.steer(UserTurn::text("s1"));
        tx.interrupt(UserTurn::text("x1"));
        tx.steer(UserTurn::text("s2"));
        assert_eq!(
            mb.take_interrupt().as_ref().map(|t| t.text.as_str()),
            Some("x1")
        );
        assert_eq!(
            mb.take_steer().as_ref().map(|t| t.text.as_str()),
            Some("s1")
        );
        assert_eq!(
            mb.take_steer().as_ref().map(|t| t.text.as_str()),
            Some("s2")
        );
        assert_eq!(mb.take_idle().as_ref().map(|t| t.text.as_str()), Some("i1"));
        assert!(mb.take_interrupt().is_none());
        tx.steer(UserTurn::text("s3"));
        tx.idle(UserTurn::text("i2"));
        tx.interrupt(UserTurn::text("x2"));
        assert_eq!(mb.take_idle().as_ref().map(|t| t.text.as_str()), Some("i2"));
        assert_eq!(
            mb.take_interrupt().as_ref().map(|t| t.text.as_str()),
            Some("x2")
        );
        assert_eq!(
            mb.take_steer().as_ref().map(|t| t.text.as_str()),
            Some("s3")
        );
    }

    #[test]
    fn set_idle_replaces_queue() {
        let (tx, mut mb) = mailbox();
        tx.idle(UserTurn::text("a"));
        tx.idle(UserTurn::text("b"));
        tx.set_idle(vec![UserTurn::text("b"), UserTurn::text("c")]);
        assert_eq!(mb.take_idle().as_ref().map(|t| t.text.as_str()), Some("b"));
        assert_eq!(mb.take_idle().as_ref().map(|t| t.text.as_str()), Some("c"));
        assert!(mb.take_idle().is_none());
    }

    #[test]
    fn discard_clears_all_queues() {
        let (tx, mut mb) = mailbox();
        tx.interrupt(UserTurn::text("x"));
        tx.steer(UserTurn::text("s"));
        tx.idle(UserTurn::text("i"));
        mb.discard();
        assert!(mb.take_interrupt().is_none());
        assert!(mb.take_steer().is_none());
        assert!(mb.take_idle().is_none());
    }

    #[test]
    fn abort_survives_context() {
        use crate::tool::aborted;
        let e = aborted().context("during complete");
        assert!(is_abort(&e));
        assert!(!is_abort(&anyhow::anyhow!("aborted")));
    }

    #[test]
    fn new_chat_json_is_a_bool_not_a_drop_list() {
        assert!(parse_new_chat(r#"{"new":true}"#));
        assert!(parse_new_chat("sure\n{\"new\": true}\n"));
        assert!(!parse_new_chat(r#"{"new":false}"#));
        // A drop list or empty reply is not a stay. Stay must be explicit.
        assert!(parse_new_chat(r#"{"drop":[1,2]}"#));
        assert!(parse_new_chat("start a new chat"));
        assert!(parse_new_chat(""));
        assert!(parse_new_chat("thinking… {\"new\":true}"));
    }

    #[test]
    fn explicit_new_chat_forks_without_waiting_for_a_fat_session() {
        assert!(wants_new_chat("i need new chat"));
        assert!(wants_new_chat("say hi with a new chat"));
        assert!(wants_new_chat("  NEW SESSION please "));
        assert!(wants_new_chat("start over"));
        assert!(!wants_new_chat("hi"));
        assert!(!wants_new_chat(
            "the new chat gate is broken, fix the session fork and explain why a short transcript stayed"
        ));
        assert!(!wants_new_chat(""));
    }

    #[test]
    fn new_chat_prompt_keeps_the_queue() {
        assert!(NEW_CHAT_SYSTEM.contains("stays and then prunes"));
        assert!(NEW_CHAT_SYSTEM.contains("Do not drop or rewrite it"));
        assert!(NEW_CHAT_SYSTEM.contains("different task"));
        assert!(NEW_CHAT_SYSTEM.contains("not enough to stay"));
        assert!(NEW_CHAT_SYSTEM.contains("not only in thinking"));
    }

    #[test]
    fn prune_prompt_does_not_treat_a_stub_as_the_file() {
        assert!(!PRUNE_SYSTEM.contains("can see that the file exists"));
        assert!(PRUNE_SYSTEM.contains("not proof the file exists"));
        assert!(PRUNE_SYSTEM.contains("Do not drop the new user message"));
        assert!(PRUNE_SYSTEM.contains("queue after it stays queued"));
    }

    #[test]
    fn repeating_output_trips_only_on_a_stuck_phrase() {
        let phrase = "The signature tar ends with zero blocks before the index tar. I'll skip those and continue. ";
        let stuck = phrase.repeat(8);
        assert!(output_is_repeating(&stuck), "stuck stream should trip");
        assert!(!output_is_repeating(&phrase.repeat(2)), "two copies is not enough");
        let varied = "Read the file, then edit the call site, then run the tests and report what failed in the workspace before claiming the fix is done. ";
        assert!(!output_is_repeating(&varied.repeat(3)));
        assert!(!output_is_repeating(&format!(
            "{varied}{varied} extra detail that is not a copy"
        )));
        let mut g = RepeatGuard::new();
        assert!(!g.push(&phrase.repeat(3)));
        assert!(g.push(&phrase.repeat(4)));
        assert!(!g.push(&phrase.repeat(2)), "already tripped");
        assert!(
            !output_is_repeating(&"\n".repeat(200)),
            "newlines are not a phrase; the whitespace run catches them"
        );
        let mut blank = RepeatGuard::new();
        assert!(!blank.push("Still working through the tar headers."));
        assert!(!blank.push(&"\n".repeat(REPEAT_WS_RUN - 1)));
        assert!(blank.push("\n"), "a long newline run should stop the turn");
        let mut spaces = RepeatGuard::new();
        assert!(spaces.push(&" ".repeat(REPEAT_WS_RUN)));
        let mut wrapped = RepeatGuard::new();
        assert!(!wrapped.push("line one\n\nline two\n"));
        let line_a = "The signature tar ends, then padding, then the index. I'll skip the padding and keep reading. ";
        let line_b = "The signature stream ends with zero blocks before the index tar. I'll skip those and continue. ";
        let mut para = String::new();
        for i in 0..12 {
            if i % 2 == 0 {
                para.push_str(line_a);
            } else {
                para.push_str(line_b);
            }
        }
        assert!(
            para.chars().count() >= REPEAT_PARA_SPAN,
            "fixture should be long enough to judge"
        );
        assert!(
            output_is_repeating(&para),
            "a restated plan with no tool call should trip"
        );
        assert!(
            !output_is_repeating(&para.chars().take(400).collect::<String>()),
            "a short plan is not a loop"
        );
        let mut steps = String::new();
        let verbs = [
            "reads", "checks", "skips", "writes", "parses", "copies", "verifies", "drops",
            "opens", "closes",
        ];
        let nouns = [
            "signature", "index", "payload", "header", "trailer", "padding", "member", "table",
            "entry", "block",
        ];
        for i in 0..30 {
            steps.push_str(&format!(
                "Step {i} {} the {} at offset {} before moving to the next record. ",
                verbs[i % verbs.len()],
                nouns[i % nouns.len()],
                i * 512
            ));
        }
        assert!(
            !output_is_repeating(&steps),
            "a step that names a new field is not a loop"
        );
        let mut code = String::new();
        for i in 0..40 {
            code.push_str(&format!("fn item_{i}() -> usize {{ {i} }}\n"));
        }
        assert!(
            !output_is_repeating(&code),
            "a code listing is not a loop"
        );
    }

    #[test]
    fn ellipsize_and_snippet() {
        use serde_json::json;
        assert_eq!(ellipsize("abcd", 4), "abcd");
        assert_eq!(ellipsize("abcde", 4), "abcd…");
        assert_eq!(title_case("bash"), "Bash");
        assert_eq!(
            args_repr(&json!({"path": "../secret"})),
            "path=\"../secret\""
        );
        let read = tool_fail(
            "read",
            &json!({"path": "../secret"}),
            "path escapes workspace: ../secret",
        );
        assert_eq!(read.call_line(), "Read(path=\"../secret\")");
        assert!(read.is_error);
        assert_eq!(read.detail, "path escapes workspace: ../secret");
        let bash = tool_ok("bash", &json!({"cmd": "ls"}));
        assert_eq!(bash.call_line(), "Bash(cmd=\"ls\")");
        assert!(!bash.is_error);
        assert_eq!(tool_counts(&[bash.clone(), read.clone()]), (1, 1));
        assert_eq!(tool_summary(1, 0), "1 tool succeeded");
        assert_eq!(tool_summary(4, 0), "4 tools succeeded");
        assert_eq!(tool_summary(5, 0), "5 tools succeeded");
        assert_eq!(tool_summary(0, 0), "");
        assert_eq!(tool_summary(0, 1), "1 tool failed");
        assert_eq!(tool_summary(0, 2), "2 tools failed");
        assert_eq!(tool_summary(7, 1), "7 tools succeeded  1 tool failed");
    }
}
