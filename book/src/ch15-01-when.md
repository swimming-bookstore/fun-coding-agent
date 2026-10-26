# When it runs

The decision runs **once per user message**, before that message is
stored. That includes a composer send and a queued interrupt, steer, or
idle line that is about to become a user entry.

It does not run in the middle of a tool round. This turn’s tools are
already the live task. After a stay, Fun will not ask again until the
next user message moves the live tail.

```text
1. You send, or a queued line is taken
2. Gate                transcript non-empty? fat? not already tried this live tail?
3. New chat?           low effort, no tools — or skip and stay
4. Paint the user line before the decision. If new, that copy is dropped, the screen clears, then the same line is reprinted with the saved-path hint under it
5. If stay             prune: hide named candidates
6. Complete            tools on, slim payload if a stay hid something
```

An abort during either call is an abort of the turn. No new file is
started, no partial ledger line is written, and the queue is left as it
was.

## Gate

All of these must hold or Fun does not call the model:

| Check | Rule |
| --- | --- |
| There is a previous chat | An empty transcript is already a new chat. |
| Context is fat | Last *real* input tokens ≥ **20 000** **or** remaining visible chars ≥ **32 000**, and at least **two** older messages could have been candidates. |
| Once per live tail | After a stay (or a failed decision), Fun will not ask again until the next user message moves the live tail. |

A small session never reaches the fat gate. Decision and prune tokens
add to lifetime usage. They do **not** become `last_input_tokens`.
After a successful hide, Fun sets that counter to **0** so the next real
turn measures the slim payload.

If the new-chat call errors, Fun stays and still tries to prune. If the
prune call errors, the turn continues with the unpruned payload.

## Why the queue stays

The useful part of the queue is that interrupt, steer, and idle still
send, in that order. Neither a new chat nor a prune discards them or
rewrites them. Prune can only hide messages already in the transcript.
The line being judged is not stored yet, so it cannot be a drop id.
