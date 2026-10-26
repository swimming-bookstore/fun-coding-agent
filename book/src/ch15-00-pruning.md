# New chat, or prune

A long coding session fills the model context with file dumps, failed
commands, and plans that no longer matter. Dropping those messages on
every turn was the wrong default: a follow-up still needs the live work,
and the queue of sends you already typed should not be thrown away.

Fun asks one question first, only when the payload is fat: **is this new
user message a new chat?**

- **New chat.** The previous session file is already on disk. Fun clears
  the screen and sends **only** the new user message. Queued sends stay
  queued; they are not dropped and they are not rewritten.
- **Stay.** The screen stays. Fun then prunes: named old messages are
  hidden from the next API send. The transcript on disk and on screen is
  not deleted. The new user line and the queue are not droppable.

```text
fat context + new user line
        │
        ▼
   different task?  ──yes──►  save previous session, clear the screen,
        │                     send this line alone
        no
        ▼
   prune old noise from the next send, then complete
```

This is not a composer command. It runs when a user turn starts and the
live payload is fat. Whether that line is a new chat is the model's
decision, not a phrase match. Details: [When it runs](ch15-01-when.md).

## What the model is asked

Two low-effort calls, no tools. The new user message is not in the
transcript yet.

1. **New chat?** A short task brief plus the new line. Expect `{"new":true}` or
   `{"new":false}`. Only an explicit `false` stays. A missing or unparseable
   answer starts a new chat, so a thought-only reply cannot fall through to prune.
2. **If staying, what to hide?** The same listing. Expect
   `{"drop":[ids]}`. Only candidate ids hide. See
   [Applying a drop](ch15-08-apply.md).

A failed new-chat call stays and still tries the prune. A failed prune
continues with the unpruned payload. The turn still happens.

`false` means the new line continues the current task: a follow-up, a
correction, a tighter constraint, or an answer to the assistant. `true`
means a different task, including an explicit request to start over. The
call sees the user lines and the latest conclusion, not the prune listing,
so old tool dumps cannot talk it into staying.

## What you see

A new chat drops the line painted before the decision, clears the transcript, then reprints that line with the hint under it:

```text
you
the message that started this chat

(new chat — previous session saved to ~/.local/share/fun/sessions/…/….jsonl)
```

A stay leaves the transcript. The new line is painted, then the dim drop line under it:

| Line | Meaning |
| --- | --- |
| `(dropped N messages from context)` | N ids newly hidden from the next send |
| `(dropped N messages from context, restored R)` | Same, plus R write/edit stubs |
| `(nothing to drop)` | The model returned an empty list (or nothing valid) |
| `(prune skipped: …)` | The hide call failed; the full payload still goes |
| `(new chat skipped: …)` | The decision call failed; Fun stayed and still tried to prune |

Click a drop line to inspect ids. The overlay does not open by itself.

## What you do not see

- No spinner. Both calls are a pause before the real reply.
- The queue, steer strip, and interrupt strip do not clear.
- A new chat does not also prune the previous file. That file is already
  saved as it was.
