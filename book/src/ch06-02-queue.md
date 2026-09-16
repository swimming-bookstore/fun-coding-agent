# Queue, steer, interrupt

You can type while Fun is working. Those lines are not all the same
kind of pending text. The difference is **when** the text becomes a
user entry, and **whether** the current tools are allowed to finish.

If several kinds are pending, Fun drains **interrupt**, then **steer**,
then **idle**.

## Interrupt — stop now

Fun aborts the in-flight stream and any tool that has not finished,
writes skipped tool results so the transcript stays paired, and injects
your text as a user message. The agent loop restarts from the top
(including a possible prune, because the last entry is now that user
line).

From the composer: **Ctrl+Enter**.

The interrupt strip at the top of the pending region shows these
prompts. Opening another session (adopt) also aborts the live turn,
without sending composer text.

Use interrupt when the model is on the wrong file, a `bash` is hanging
inside the timeout, or you want to change the task immediately.

## Steer — skip this batch of tools

Do not stop the current Grok complete — that assistant message is
already streaming. **Do not run the tools it just asked for.** After
the assistant entry is saved, Fun injects your text as the next user
line and loops.

Use this when you already see the plan in the stream and want to
redirect *before* `bash` or `edit` fire.

There is no composer chord for steer. Enter while working is **idle**,
not steer. To steer:

1. Queue a line with Enter (or type it first).
2. Click **send now** on that chip.

That promotes an idle item into a steer.

## Idle — the queue

Wait until this turn is actually done (no more tool calls, or you
already interrupted), then send the next prompt, in order.

From the composer while a turn is running: **Enter**.

Queued items draw as chips in the queue box:

| Control | Effect |
| --- | --- |
| send now | Promote to steer |
| edit | Load into the composer, bound to that slot |
| move up / move down | Reorder |
| cancel | Drop the item |
| drag | Reorder with the mouse |

Reordering talks to the [mailbox](ch12-01-mailbox.md) as a whole list
(`SetIdle`), so the running loop sees the new order, not a stale
snapshot.

## Abort versus interrupt

**Esc** aborts without sending text. The transcript gets `(aborted)`.
The composer is unchanged. Pending interrupt / steer / idle items are
discarded.

Ctrl+C quits the process. It is not an agent abort. The session file
is already saved; you are only leaving the TUI.

If you abort during prune, Fun treats it like an abort of the real
turn. You will not get a half-applied drop.
