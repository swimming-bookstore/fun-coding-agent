# Context pruning

A long coding session fills the model context with file dumps, failed
commands, and plans that no longer matter. The naive fixes are all
worse than they look:

- **Send everything** until the API errors. You pay for noise, then
  hit a wall mid-task.
- **Summarize** the transcript into a new message. A summary invents
  prose the agent may treat as fact (“tests were fixed”) when they
  were not.
- **Drop everything after index N.** That looks cache-friendly. It
  made Fun forget later user constraints and rebuild from the first
  prompt.

Fun does something narrower. It **hides named messages** from the next
API payload and leaves the transcript on disk and in the TUI intact.

The record of that is a **keep/drop ledger**. Each prune turn appends
which indexes are **in** (**keep**) and which are **out** (**drop**).
That in/out score is calculated from **previous keep and drop**, then
folded: last keep, plus messages after that turn, minus later drops.
Grok sees previous turns as `## Ledger` so it does not re-score old
ids. [The keep/drop ledger](ch15-07-ledger.md) is the page for that.

Prune is not a composer command. It runs on its own when a user turn
starts and the live payload is fat. Details: [When it runs](ch15-01-when.md).

## What a prune turn is

A prune turn is bookkeeping, not a rewrite. Fun builds a listing with
four buckets (Live, Keep, Candidates, Hidden), asks Grok at **low**
effort with **no tools**, and expects:

```json
{ "drop": [3] }
```

Fun then:

1. Intersects those digits with the **candidate** set.
2. Pairs tool calls with their results so the wire never has a lone
   function call.
3. Refuses Live and Keep ids.
4. Appends a jsonl record with **keep**, **drop**, and **restored**.

The next send is:

```text
last keep  ∪  messages after that turn  −  later drop
+ every user message
+ restore stubs for latest hidden writes/edits
```

User messages always go. Holes from dropped tool noise are expected:

```text
goal → (dropped bash) → later edit → live tail
```

A low drop id must **not** mean “everything after this index.” Keep
rules exist because of that. See [Keep](ch15-04-keep.md).

## What you see

One dim line in the transcript:

| Line | Meaning |
| --- | --- |
| `(dropped N messages from context)` | N ids newly hidden this turn |
| `(dropped N messages from context, restored R)` | Same, plus R write/edit stubs on the next payload |
| `(nothing to drop)` | The model returned an empty list (or nothing valid) |
| `(prune skipped: …)` | Gate failed after Fun had already started talking about it |

Click a drop line to inspect ids. The overlay does not open by itself.

## What you do not see

- No spinner. Prune is supposed to feel like a pause before the real
  complete, not a second agent.
- No change to the transcript rows. The bash dump is still there when
  you scroll up.
- No summary message inserted as a fake user or assistant.

If a later review needs to know that `login.rs` was written, Fun may
send a **restore stub** (path only) even though the full write is still
hidden. That is [Restore stubs](ch15-09-restore.md), not a second copy
of the file on disk.
