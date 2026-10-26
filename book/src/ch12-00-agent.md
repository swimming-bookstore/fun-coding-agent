# The agent

Everything you type becomes a user entry, then this loop. The TUI and
headless mode share it. While a turn runs, the TUI talks to this loop
through a [mailbox](ch12-01-mailbox.md) — not a mutex.

```text
prompt() may start a new chat, then appends the user line
        │
        ▼
loop:
  1. drain interrupt → maybe new chat, else prune, then inject, restart
  2. Grok complete   → stream thinking + text, tools enabled
  3. save usage + assistant entry
  4. interrupt?      → skip remaining tools, inject, restart
  5. steer?          → skip remaining tools, inject, restart
  6. run tools       → or stop at 200 rounds
  7. interrupt / steer again
  8. if tools ran, loop for the follow-up complete
  9. idle queue      → next prompt, or return to the TUI
```

**Esc** and adopt abort: the loop exits, pending queue items are
discarded, and `(aborted)` is painted on its own line if you cancelled.

**Ctrl+Enter** interrupts with text: abort flag, then inject, then the
loop continues (it does not return to idle first). A fat context can
still start a new chat on that injected line, or prune if it stays. The
queue is not cleared.

An abort during the new-chat decision or the stay prune is treated like
an abort of the real turn. Fun will not start a file it did not finish
deciding on, and it will not apply a partial drop.

## What the model sees

Each complete sends:

- the system prompt (identity, tools, workspace path)
- the session payload: a new chat has only the new user line; a stay
  sends the ledger (last keep, plus later messages, minus later drops)
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
say so in the user line — that line is not stored until after prune, so
it cannot be dropped.
