# The agent

Everything you type becomes a user entry, then this loop. The TUI and
headless mode share it. While a turn runs, the TUI talks to this loop
through a [mailbox](ch12-01-mailbox.md) — not a mutex.

```text
prompt() appends the user line
        │
        ▼
loop:
  1. drain interrupt → inject as user, restart
  2. maybe_prune     → only if last entry is User and the gate fires
  3. drain interrupt again
  4. Grok complete   → stream thinking + text, tools enabled
  5. save usage + assistant entry
  6. interrupt?      → skip remaining tools, inject, restart
  7. steer?          → skip remaining tools, inject, restart
  8. run tools       → or stop at 200 rounds
  9. interrupt / steer again
 10. if tools ran, loop for the follow-up complete
 11. idle queue      → next prompt, or return to the TUI
```

**Esc** and adopt abort: the loop exits, pending queue items are
discarded, the transcript gets `(aborted)` if you cancelled.

**Ctrl+Enter** interrupts with text: abort flag, then inject, then the
loop continues (it does not return to idle first).

An abort during prune is treated like an abort of the real turn. Fun
will not apply a partial drop.

## What the model sees

Each complete sends:

- the system prompt (identity, tools, workspace path)
- `session.model_entries()` — the slim payload from the ledger
- the four tool schemas

Thinking is streamed to the TUI but the next payload does not need you
to manage it; Fun stores it on the assistant entry.

## Caps and failure

| Event | What you see |
| --- | --- |
| 200 tool rounds | `(stopped after too many tool rounds)` plus skipped tool rows |
| Stream truncated | `(truncated; tool calls dropped)` — that batch does not run |
| Not logged in | error, no session corruption |
| Tool error | `is_error` on the tool entry; the loop continues so the model can recover |

The system prompt is not configurable. If you want different behavior,
say so in the user line — that line is Keep and will survive prune.
