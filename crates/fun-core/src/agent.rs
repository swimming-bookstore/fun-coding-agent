use crate::grok::Grok;
use crate::prune::{PRUNE_MIN_CANDIDATES, parse_drop_ids, prune_inspect_note};
use crate::session::{Call, Entry, Image, Session, ToolResult, Usage};
use crate::tool::{Abort, Tool, clip_utf8, execute_tool, is_abort};
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

const MAX_TOOL_ROUNDS: usize = 200;

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
const PRUNE_SYSTEM: &str = "\
You prune a coding-agent transcript. Reply with JSON only: {\"drop\":[ids]}.\n\
Live and Keep messages must never appear in drop. User messages are Keep.\n\
A Ledger of previous keep/drop turns is bookkeeping — the next send is last keep plus later messages, minus later drop.\n\
Drop only the candidate ids you name (tool pairs are filled in). Do not treat a low id as “drop everything after”.\n\
Prefer dead-end tool noise (failed bash, superseded dumps) and fat write/edit bodies a restore stub can replace.\n\
Drop: repeated or superseded tool output, dead-end commands, old plans, chatter, old tool-less conclusions that later user lines replaced, and write/edit calls whose full file body is no longer needed.\n\
Do not drop user goals, later constraints, or the previous turn’s latest read — the agent will otherwise redo or delete finished work.\n\
Dropped latest writes/edits still reach the next payload as compact path stubs, so a later review can see that the file exists.\n\
Keep: every user message, the previous turn’s conclusion, remaining errors, and anything the live messages still need.\n\
If nothing should go, return {\"drop\":[]}. No tools. No extra text.";

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
    provider: Grok,
    tools: Vec<Tool>,
    pub session: Session,
    mailbox: Mailbox,
    log: Sender<LogLine>,
    /// Live-tail index of a prune already tried this turn. Skip until that tail moves.
    prune_stuck: Option<usize>,
}

impl Agent {
    pub fn new(
        workspace: PathBuf,
        model: String,
        provider: Grok,
        tools: Vec<Tool>,
        session: Session,
        mailbox: Mailbox,
        log: Sender<LogLine>,
    ) -> Self {
        Self {
            workspace,
            model,
            effort: provider.effort.clone(),
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
             User messages may include images (png, jpeg, gif, webp). Look at them when the user asks about a picture.\n\
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
        a.emit(LogLine::Text("(aborted)".into()));
    }
    result
}

async fn agent_loop(a: &mut Agent) -> Result<()> {
    let mut tool_rounds = 0usize;
    loop {
        if a.mailbox.cancelled() {
            break;
        }
        if inject_interrupt(a)? {
            a.mailbox.clear_abort();
            tool_rounds = 0;
            continue;
        }
        match maybe_prune(a).await {
            Ok(()) => {}
            Err(e) if is_abort(&e) => {
                if a.mailbox.cancelled() {
                    break;
                }
                if inject_interrupt(a)? {
                    a.mailbox.clear_abort();
                    tool_rounds = 0;
                    continue;
                }
                break;
            }
            Err(e) => return Err(e),
        }
        if a.mailbox.cancelled() {
            break;
        }
        if inject_interrupt(a)? {
            a.mailbox.clear_abort();
            tool_rounds = 0;
            continue;
        }
        let system = a.system();
        let abort = a.mailbox.abort.clone();
        let model_entries = a.session.model_entries();
        let reply = match a
            .provider
            .complete(
                &a.model,
                &system,
                &model_entries,
                &a.tools,
                &abort,
                |d| a.emit(LogLine::Delta(d.into())),
                |d| a.emit(LogLine::Think(d.into())),
            )
            .await
        {
            Ok(r) => r,
            Err(e) if is_abort(&e) => {
                if a.mailbox.cancelled() {
                    break;
                }
                if inject_interrupt(a)? {
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
        if inject_interrupt(a)? {
            finish_skipped_tools(a, &calls, 0)?;
            a.mailbox.clear_abort();
            tool_rounds = 0;
            continue;
        }
        if inject_steer(a)? {
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
        if inject_interrupt(a)? {
            a.mailbox.clear_abort();
            tool_rounds = 0;
            continue;
        }
        if inject_steer(a)? {
            tool_rounds = 0;
            continue;
        }
        if !calls.is_empty() {
            continue;
        }
        if inject_idle(a)? {
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
        if inject_interrupt(a)? {
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
                    if inject_interrupt(a)? {
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
        }))?;
        if inject_interrupt(a)? {
            finish_skipped_tools(a, calls, i + 1)?;
            a.mailbox.clear_abort();
            return Ok(ToolFlow::Continue);
        }
        if inject_steer(a)? {
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

fn inject_interrupt(a: &mut Agent) -> Result<bool> {
    let text = a.mailbox.take_interrupt();
    inject_user(a, text)
}

fn inject_steer(a: &mut Agent) -> Result<bool> {
    let text = a.mailbox.take_steer();
    inject_user(a, text)
}

fn inject_idle(a: &mut Agent) -> Result<bool> {
    let text = a.mailbox.take_idle();
    inject_user(a, text)
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

async fn maybe_prune(a: &mut Agent) -> Result<()> {
    // Only at the start of a user turn. Mid-turn the live tail already locks
    // this turn's tools, so pruning after each tool just nibbles leftovers.
    if !matches!(a.session.entries.last(), Some(Entry::User { .. })) {
        return Ok(());
    }
    if a.session.should_prune().is_none() {
        return Ok(());
    }
    let live_from = a.session.prune_view().live_from();
    if a.prune_stuck == Some(live_from) {
        return Ok(());
    }
    match run_prune(a).await {
        Ok((added, _)) if added.is_empty() => {
            a.prune_stuck = Some(live_from);
            a.emit(LogLine::Dim("(nothing to drop)".into()));
        }
        Ok((added, restored)) => {
            a.prune_stuck = Some(live_from);
            emit_prune_note(a, added, restored);
        }
        Err(e) if is_abort(&e) => return Err(e),
        Err(e) => {
            a.prune_stuck = Some(live_from);
            a.emit(LogLine::Dim(format!("(prune skipped: {e})")));
        }
    }
    Ok(())
}

async fn run_prune(a: &mut Agent) -> Result<(Vec<usize>, Vec<usize>)> {
    let (ids, listing) = a.session.prune_view().listing();
    if ids.len() < PRUNE_MIN_CANDIDATES {
        return Ok((Vec::new(), Vec::new()));
    }
    let abort = a.mailbox.abort.clone();
    let prompt = format!(
        "Workspace: {}\nDrop only candidate ids that no longer matter given the live messages.\n\n{listing}",
        a.workspace.display()
    );
    let reply = a
        .provider
        .complete_with_effort(
            &a.model,
            PRUNE_SYSTEM,
            &[Entry::User {
                text: prompt,
                images: Vec::new(),
            }],
            &[],
            &abort,
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
    let drop = parse_drop_ids(&reply.text, &ids);
    if drop.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let hidden = a.session.prune_view().apply(&drop);
    if hidden.difference(&a.session.hidden).next().is_none() {
        return Ok((Vec::new(), Vec::new()));
    }
    let (added, restored) = a.session.save_hidden(hidden)?;
    if added.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    // Hidden rows are gone from the next payload; don't keep the pre-prune size.
    a.session.usage.last_input_tokens = 0;
    a.session.save_usage(a.session.usage)?;
    Ok((added, restored))
}

fn emit_prune_note(a: &mut Agent, added: Vec<usize>, restored: Vec<usize>) {
    let n = added.len();
    let r = restored.len();
    let lines = prune_inspect_note(&a.session.entries, added, restored);
    a.emit(LogLine::Pruned { n, lines });
    let text = if r == 0 {
        format!("(dropped {n} messages from context)")
    } else {
        format!("(dropped {n} messages from context, restored {r})")
    };
    a.emit(LogLine::Dim(text));
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
        LogLine::Think(_)
        | LogLine::User(_)
        | LogLine::Dim(_)
        | LogLine::Usage(_)
        | LogLine::Pruned { .. } => {}
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
