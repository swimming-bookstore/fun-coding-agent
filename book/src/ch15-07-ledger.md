# The keep/drop ledger

Whether a message is **in** the next payload is a score. Fun does not
rank messages 0.0–1.0. The score is **in or out**, and it is calculated
from **previous keep and drop** — the ledger — not from recency or
token size.

```text
score(i) = in   if i is in last keep, or i was appended after that turn
score(i) = out  if a later drop named i
```

That is the whole function. Classify (Live / Keep / Candidate) only
decides what Grok may **newly** drop. The ledger already decided
everyone else.

## Previous keep and drop

Each prune turn appends two lists of indexes:

| List | Score it writes |
| --- | --- |
| **keep** | These ids are **in** after this turn |
| **drop** | These ids are **out** after this turn |

Grok sees every previous turn first:

```text
## Ledger (previous keep/drop — next send is keep minus later drop)
@6 keep 0,1,2,5 drop 3,4
@9 keep 0,1,2,5,8 drop 6,7 restored 1,2
```

Read a line as scores, not as prose:

- After `@6`, ids `0,1,2,5` score **in**; `3,4` score **out**.
- After `@9`, `6,7` flip to **out**; `8` is **in** (new work that
  survived). `1,2` stay **out** on disk but a stub may still go
  (`restored`).

A later prune does not re-score 0–9 from scratch. It **starts from
these lists**, then may name new **candidates** only.

The first prune has no ledger yet. Every visible non-live row is
scored by buckets alone. After that, previous keep/drop is the prior.

## Calculate the next send

```text
in  = last keep  ∪  { i | i ≥ last.at }
out = union of every drop
send = (in − out)  ∪  users  ∪  restore stubs
```

| Term | Meaning |
| --- | --- |
| `last keep` | Ids that scored **in** at the last prune |
| `i ≥ last.at` | Messages written after that prune. They start **in** |
| every `drop` | Once **out**, stay **out** (unless the row is a user) |
| users | Always **in**, even if an old drop list still names them |
| restore stubs | **Out** on disk; a path-only stand-in may still go |

Empty ledger → everything is **in** (then Hidden / users / stubs apply
as usual).

Worked fold:

```text
@6  keep 0,1,2,5  drop 3,4
    score: 0,1,2,5 in; 3,4 out

new messages 6,7,8,9 arrive  →  they start in

@9  keep 0,1,2,5,8  drop 6,7
    score: 6,7 out; 8 still in
    send:  0,1,2,5,8  ∪  {10,11,…}  −  (future drops)
```

Holes are expected: `0,1,2,5` with 3–4 **out**.

## jsonl

```json
{"type":"prune","hidden":[3,4],"dropped":2,"added":[3,4],"keep":[0,1,2,5,6],"restored":[]}
```

| Field | Score |
| --- | --- |
| `keep` | **in** after this turn |
| `added` | **out** after this turn (this drop) |
| `hidden` | union of every drop — all **out** |
| `dropped` | `added.len()` |
| `restored` | **out** rows sent as stubs |

Inspect uses `added` for the clickable line, and `hidden` for the
resume note. Reopen walks every prune line into `PruneTurn`s in order.
The score on resume **is** previous keep/drop, not a RAM ranking.

## Two readers

| Who | How they use the score |
| --- | --- |
| Prune model | Reads `## Ledger` so it does not re-name **out** ids. Names new **candidate** ids only. |
| Agent | `send_ids`: last keep ∪ later messages − later drop. That set is who is **in**. |

Live / Keep / Candidate / Hidden is the classifier for *this* pass.
Previous keep/drop is the score already on the books.

See [The four buckets](ch15-02-buckets.md) and
[Applying a drop](ch15-08-apply.md).
