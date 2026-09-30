# Introduction

A coding agent is a loop. You send a task. A model replies with text,
or with tool calls. The tools run. Their results go back to the model.
That continues until the model is done talking, you abort, or a safety
cap fires.

Fun is that loop, bound to a **workspace directory**. The workspace is
the current directory when you launch `fun`, or the path you pass with
`--dir`. Relative paths in tools are relative to that directory.
Sessions are stored per workspace, so two checkouts of the same project
do not share a transcript.

## What you get

- A full-screen **terminal UI** when you run `fun` with no extra words.
- A **headless** one-shot when you pass a prompt on the command line.
- Four tools the model can call: `read`, `write`, `edit`, `bash`.
- A **session file** on disk that outlives the process. Resume is
  opening the latest jsonl for this workspace.
- **Context pruning** when the live payload is fat. Fun hides named
  messages from the next API call. It does not summarize the
  transcript into new prose.

There is one model provider: xAI Grok. Tokens stay on this machine.

## Two views of the same session

A session is a jsonl list of **entries**: user, assistant, tool. Every
entry has a **0-based index**. Indexes never move. Pruning never
deletes a row from the file or from the TUI.

There are two views of that list:

| View | What it contains |
| --- | --- |
| Disk and TUI | Every entry, forever. This is what you scroll. |
| Model payload | The subset Fun sends on the next complete. |

The payload is folded from a keep/drop **ledger**. That ledger **is**
the score: a message is **in** or **out** from previous keep and drop,
not from a 0–1 rank. User messages always go. Dropped tool noise
leaves holes on purpose:

```text
goal → (dropped bash dump) → later edit → live tail
```

If you need that to be precise, [Context pruning](ch15-00-pruning.md)
is the chapter. You can use Fun without reading it. You will notice
pruning as a dim line:

```text
(dropped 4 messages from context)
```

Click that line in the TUI to see which indexes left the payload.

## A turn, in one picture

```text
you type “fix the tests”
        │
        ▼
  user entry appended to jsonl
        │
        ▼
  maybe prune (quiet, start of a user turn only)
        │
        ▼
  Grok streams thinking, then text
        │
        ▼
  tool calls? ──yes──► read / write / edit / bash ──► loop
        │
       no
        ▼
  idle, or the next queued prompt
```

While that is happening you can still type. Enter **queues** a follow-up
for after this turn. Ctrl+Enter **interrupts**: tools stop, your text
becomes the next user message, the loop restarts. Esc **aborts**
without sending text.

That distinction — queue vs steer vs interrupt — is
[Queue, steer, interrupt](ch06-02-queue.md). It is the main thing that
is not obvious from a blank composer.

## Defaults

| Thing | Default |
| --- | --- |
| Binary | `fun` |
| Model | `grok-4.7` |
| Reasoning effort | `medium` (prune calls always use `low`) |
| Config | `~/.config/fun/config.json` |
| Auth | `~/.local/share/fun/auth.json` |
| Sessions | `~/.local/share/fun/sessions/` |
| Tool round cap | 200 |

Environment variables and config keys are listed later. You can ignore
them until you want a different model, palette, or token file.
