# Applying a drop

The model’s list is a suggestion. Fun hardens it so the wire stays
valid. This page is the apply pipeline, in order.

1. **Intersect with candidates.** Digit runs in the reply are kept only
   if they are in this turn’s candidate set. JSON `{"drop":[…]}` is
   preferred, not required — extra prose is ignored.
2. **Expand tool pairs.** Dropping an assistant tool-call also hides
   matching tool results (and the reverse), so the wire never has a
   lone function call. Pairing stops at the live tail.
3. **Do not suffix-wipe.** Only the named candidates (plus pairs) are
   hidden. A low id must not wipe later edits.
4. **Never store user ids** in hidden, even if an older jsonl prune
   line still lists them.
5. **Reject locks.** Live, previous-turn reads, and previous-turn
   conclusions are stripped even if the model named them. Latest
   writes/edits may drop; they come back as [restore stubs](ch15-09-restore.md).
6. **Restore broken pairs.** If one side of a call/result pair would
   remain visible, both stay visible.
7. **Append to jsonl.** Transcript rows are not rewritten. Fun appends
   the full hidden set, this prune’s `added` (drop), `keep`, and
   `restored`.
8. **Reset live context size.** `last_input_tokens` is set to 0 so the
   next fat check measures the slim payload.

## Example

Live is 5–6. Keep is user 0 plus read 1–2. Candidates are failed tests
3–4. Reply:

```json
{ "drop": [3] }
```

Fun pairs 3 with 4. Payload: 0, 1, 2, 5, 6.

If the model replies `{ "drop": [0, 3, 5] }`, ids 0 and 5 are stripped
(user / live). Only 3–4 hide.

If the model replies with an empty list or no valid ids, nothing hides.
The latch still engages so Fun does not immediately retry.
