# Keep

Keep exists because a low candidate id used to hide **every later
index** through the live tail. That looked cache-friendly. It made the
agent forget later user constraints (“draw the assets”, “give the cuts
back”) and rebuild from the first prompt.

Keep is a lock. The prune model is told not to name these ids. Apply
strips them even if it does.

## What is locked

- Every **user** message. Hiding users is what dropped later
  constraints.
- Latest successful **write / edit per path** before the live tail, plus
  the assistant call that issued it. Naming an old bash dump must not
  wipe a later `write` of the same feature.
- Latest successful **read per path in the previous completed turn**,
  and the whole assistant tool-call group that issued it. Older unique
  reads are candidates — locking every crop forever leaves no room for
  later constraints.
- Tool-less assistant **conclusions in the previous completed turn**.
  Older conclusions are droppable so a new constraint is not
  contradicted by an earlier plan.

The previous completed turn is the span after the user message before
the live tail, up to that tail.

## Example

```text
[0] user  write an html…          keep (user)
[1] asst  read old.png            candidate
[2] tool  crop dump               candidate
[3] user  draw the assets         keep (user)
[4] asst  read new.png            keep (previous-turn read)
[5] user  give the cuts back      keep (user)
[6] asst  ok                      live
[7] user  continue                live
```

Dropping `1` may hide 1–2 only (the read pair). It must not hide 3–5.

Dead-end bash in a keep-looking range can still be a candidate if it
is not a protected read/write group. The model is told to prefer that
noise.

On resume, Fun **sanitizes** hidden so user ids are never stored there,
even if an older jsonl prune line still lists them.
