# When it runs

Prune runs **once per user turn**, inside the agent loop, **before the
first model call of that turn**. The last session entry must be the
user message you just sent (or an interrupt/steer/idle that was just
injected as a user line).

After tools, Fun does not prune again. This turn’s tool results are
already in the live tail, so a mid-turn pass could only nibble leftover
old messages — and it would race the work you are looking at.

```text
1. You send            user line appended
2. Gate                fat? ≥ 2 droppable? not already tried this live tail?
3. Prune call          low effort, no tools, listing only — or skip quietly
4. First reply         real complete with tools, on the slim payload
5. Tool rounds         no prune until the next user message
```

There is no spinner. An abort during prune is treated like an abort of
the real turn: no partial ledger write you have to unwind.

## Gate

All of these must hold or Fun does not call Grok for prune:

| Check | Rule |
| --- | --- |
| Start of a user turn | Last entry is `User`. Otherwise return. |
| Room to drop | At least **two** candidates. One lonely dump is not worth a round trip. |
| Context is fat | Last *real* input tokens ≥ **20 000** **or** remaining visible chars ≥ **32 000** (Fun estimates ~4 chars per token). |
| Once per live tail | After any attempt, Fun will not try again until the next user message (the live tail index moves). |

Prune-turn tokens add to lifetime usage on the status line. They do
**not** become `last_input_tokens`. After a successful hide, Fun sets
that counter to **0** so the next real turn measures the slim payload
instead of thinking the context is still huge.

If the prune model errors, Fun continues with the unpruned payload.
The turn still happens.

## Why “once per live tail”

Without that latch, a failed prune (model returns nothing useful, or
every named id was locked) would retry on every tool follow-up and
burn the same listing. The latch is the live-from index. A new user
line moves it.

A small session never reaches the fat gate. The only UI when prune
does run is the dim line in [Context pruning](ch15-00-pruning.md).
