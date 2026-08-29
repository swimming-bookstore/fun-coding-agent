use crate::grok::Grok;
use crate::session::{Call, Entry, Session, ToolResult, Usage};
use crate::tool::{clip_utf8, execute_tool, is_abort, Abort, Tool};
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

const MAX_TOOL_ROUNDS: usize = 200;

#[derive(Clone)]
pub(crate) struct ToolRun {
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

pub(crate) enum LogLine {
    User(String),
    Text(String),
    Delta(String),
    Think(String),
    End,
    Tools { runs: Vec<ToolRun> },
    Dim(String),
    Usage(Usage),
}

enum MailCmd {
    Interrupt(String),
    Steer(String),
    Idle(String),
    SetIdle(Vec<String>),
}

pub(crate) struct MailboxTx {
    cmd: Sender<MailCmd>,
    abort: Arc<Abort>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

pub(crate) struct Mailbox {
    cmd: Receiver<MailCmd>,
    abort: Arc<Abort>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    interrupt: Vec<String>,
    steer: Vec<String>,
    idle: Vec<String>,
}

pub(crate) fn mailbox() -> (MailboxTx, Mailbox) {
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
        },
    )
}

impl MailboxTx {
    pub fn interrupt(&self, text: String) {
        let _ = self.cmd.send(MailCmd::Interrupt(text));
        self.abort.abort();
    }
    pub fn steer(&self, text: String) {
        let _ = self.cmd.send(MailCmd::Steer(text));
    }
    pub fn idle(&self, text: String) {
        let _ = self.cmd.send(MailCmd::Idle(text));
    }
    pub fn set_idle(&self, items: Vec<String>) {
        let _ = self.cmd.send(MailCmd::SetIdle(items));
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
            }
        }
    }
    fn take_interrupt(&mut self) -> Option<String> {
        self.drain();
        if self.interrupt.is_empty() {
            None
        } else {
            Some(self.interrupt.remove(0))
        }
    }
    fn take_steer(&mut self) -> Option<String> {
        self.drain();
        if self.steer.is_empty() {
            None
        } else {
            Some(self.steer.remove(0))
        }
    }
    fn take_idle(&mut self) -> Option<String> {
        self.drain();
        if self.idle.is_empty() {
            None
        } else {
            Some(self.idle.remove(0))
        }
    }
    fn discard(&mut self) {
        self.drain();
        self.interrupt.clear();
        self.steer.clear();
        self.idle.clear();
    }
}

pub(crate) struct Agent {
    pub workspace: PathBuf,
    pub model: String,
    pub effort: String,
    provider: Grok,
    tools: Vec<Tool>,
    pub session: Session,
    mailbox: Mailbox,
    log: Sender<LogLine>,
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
        }
    }

    fn emit(&self, line: LogLine) {
        let _ = self.log.send(line);
    }

    fn system(&self) -> String {
        format!(
            "You are Fun coding agent. Tools: read, write, edit, bash.\n\
             Read a file before editing it. Prefer edit for small changes.\n\
             Verify with tools before claiming done. Keep replies short.\n\
             Paths are relative to this workspace unless absolute. read, write, and edit may use paths outside the workspace.\n\
             workspace: {}",
            self.workspace.display()
        )
    }
}

pub(crate) async fn prompt(a: &mut Agent, text: String) -> Result<()> {
    a.mailbox.clear_cancel();
    a.session.add(Entry::User { text })?;
    let result = match agent_loop(a).await {
        Err(e) if is_abort(&e) => Ok(()),
        other => other,
    };
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
        let system = a.system();
        let abort = a.mailbox.abort.clone();
        let reply = match a
            .provider
            .complete(
                &a.model,
                &system,
                &a.session.entries,
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

fn inject_user(a: &mut Agent, text: Option<String>) -> Result<bool> {
    let Some(text) = text else {
        return Ok(false);
    };
    a.emit(LogLine::User(text.clone()));
    a.session.add(Entry::User { text })?;
    Ok(true)
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

pub(crate) fn title_case(name: &str) -> String {
    let mut c = name.chars();
    c.next().map_or_else(String::new, |ch| {
        ch.to_uppercase().collect::<String>() + c.as_str()
    })
}

pub(crate) fn ellipsize(s: &str, max: usize) -> String {
    let clip = clip_utf8(s, max);
    if clip.len() < s.len() {
        format!("{clip}…")
    } else {
        s.to_string()
    }
}

pub(crate) fn args_repr(v: &Value) -> String {
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

pub(crate) fn tool_ok(name: &str, args: &Value) -> ToolRun {
    ToolRun {
        name: title_case(name),
        args: args_repr(args),
        detail: String::new(),
        is_error: false,
    }
}

pub(crate) fn tool_fail(name: &str, args: &Value, content: &str) -> ToolRun {
    let body = content.trim();
    let body = if body.is_empty() { "failed" } else { body };
    ToolRun {
        name: title_case(name),
        args: args_repr(args),
        detail: ellipsize(body, 400),
        is_error: true,
    }
}

pub(crate) fn tool_counts(runs: &[ToolRun]) -> (usize, usize) {
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

pub(crate) fn tool_summary(ok: usize, fail: usize) -> String {
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

pub(crate) fn print_log(line: &LogLine) {
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
        LogLine::Think(_) | LogLine::User(_) | LogLine::Dim(_) | LogLine::Usage(_) => {}
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
        tx.idle("i1".into());
        tx.steer("s1".into());
        tx.interrupt("x1".into());
        tx.steer("s2".into());
        assert_eq!(mb.take_interrupt().as_deref(), Some("x1"));
        assert_eq!(mb.take_steer().as_deref(), Some("s1"));
        assert_eq!(mb.take_steer().as_deref(), Some("s2"));
        assert_eq!(mb.take_idle().as_deref(), Some("i1"));
        assert!(mb.take_interrupt().is_none());
        tx.steer("s3".into());
        tx.idle("i2".into());
        tx.interrupt("x2".into());
        assert_eq!(mb.take_idle().as_deref(), Some("i2"));
        assert_eq!(mb.take_interrupt().as_deref(), Some("x2"));
        assert_eq!(mb.take_steer().as_deref(), Some("s3"));
    }

    #[test]
    fn set_idle_replaces_queue() {
        let (tx, mut mb) = mailbox();
        tx.idle("a".into());
        tx.idle("b".into());
        tx.set_idle(vec!["b".into(), "c".into()]);
        assert_eq!(mb.take_idle().as_deref(), Some("b"));
        assert_eq!(mb.take_idle().as_deref(), Some("c"));
        assert!(mb.take_idle().is_none());
    }

    #[test]
    fn discard_clears_all_queues() {
        let (tx, mut mb) = mailbox();
        tx.interrupt("x".into());
        tx.steer("s".into());
        tx.idle("i".into());
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
        assert_eq!(args_repr(&json!({"path": "../secret"})), "path=\"../secret\"");
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
