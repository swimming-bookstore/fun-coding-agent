# Mailbox

The TUI and the agent loop do not share a mutex. While Grok streams or
a tool runs, you still type. Those keystrokes have to reach the loop
**without stopping it**. That channel is the **mailbox**.

```text
TUI thread                         agent loop
  Enter / Ctrl+Enter / Esc
  click send now / reorder
  adopt another session
        │
        ▼
   MailboxTx.send  ──────── mpsc ────────►  Mailbox.drain
   (+ abort flag on interrupt / Esc / adopt)
```

Headless mode still builds a mailbox. Nothing sends on it unless you
Ctrl+C the process.

## Two halves

| Type | Who holds it | Role |
| --- | --- | --- |
| `MailboxTx` | TUI (cloneable) | Push a command. Interrupt / abort / adopt also set flags. |
| `Mailbox` | `Agent` | Drain into three queues plus an optional adopt. |

`mailbox()` returns the pair. The agent never locks the TUI. The TUI
never waits on the agent except for `LogLine`s on a second channel
(transcript, thinking, prune notes, usage).

## Commands

| Command | TUI trigger | What the loop does |
| --- | --- | --- |
| `Interrupt(text)` | Ctrl+Enter | Abort the stream and tools, inject `text` as a user line, restart the loop. |
| `Steer(text)` | **send now** on a queue chip | After this assistant entry is saved, skip its tools, inject `text`, loop. |
| `Idle(text)` | Enter while working | After the turn is actually done, send `text` as the next prompt. |
| `SetIdle(list)` | reorder / edit / cancel chips | Replace the idle list. Not an append. |
| `Adopt { session, workspace }` | open another session | Abort the live turn, swap session + cwd, do not inject composer text. |

Drain order is interrupt, then steer, then idle. Adopt is checked at
the top of `prompt()` so a session switch wins over leftover queue
items.

## Two flags

The mailbox carries more than text.

| Flag | Set by | Meaning |
| --- | --- | --- |
| **abort** | Interrupt, Esc, adopt | Tools poll it. Bash is killed. `read` / `write` / `edit` stop at their next check. The Grok stream stops. |
| **cancel** | Esc (`MailboxTx::abort`) | The loop should **exit** this `prompt()`, discard pending items, paint `(aborted)`. |

Interrupt sets **abort** only. The loop injects the text and continues.
Esc sets **cancel** and **abort**. Adopt sets **abort** and queues the
new session; cancel is not required because take-adopt runs first.

After a successful inject, Fun **clears abort** so the next tools are
not born already killed. Cancel is cleared at the start of the next
`prompt()`.

## Why `SetIdle` exists

Idle is a list, not a stack of one-shots. If you drag chip 2 above
chip 1, the TUI must not send “append 2” on top of a stale order. It
sends the **whole** idle list. `SetIdle` replaces `mailbox.idle`.

Edit-in-place of a queued line is the same: rewrite the slot in the
TUI’s pending vec, then `set_idle` the idle texts.

Steer is not in that list. Promoting a chip to **send now** removes it
from idle and sends `Steer(text)` separately.

## What is not the mailbox

- **LogLine** is the other direction: agent → TUI (user lines, deltas,
  thinking, tool runs, dim notes, prune inspect, usage).
- **jsonl** is the session. Mailbox items are not entries until the
  loop injects them as `User`.
- **Composer atoms** stay in the TUI until Enter / Ctrl+Enter / an
  action chip.

If you abort, `discard()` drops interrupt, steer, idle, and adopt so a
cancelled turn cannot leak into the next one.

Keys and chips: [Queue, steer, interrupt](ch06-02-queue.md). The loop
that drains this: [The agent](ch12-00-agent.md).
