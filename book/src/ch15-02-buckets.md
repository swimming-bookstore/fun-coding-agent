# The four buckets

Every index lands in **exactly one** bucket for *this* pass. That is
how Fun decides what Grok may name right now. Whether the message is
already **in** or **out** is a score calculated from previous keep and
drop — the [keep/drop ledger](ch15-07-ledger.md), shown first as
`## Ledger`. Classification is done once per prune pass and reused for
the listing, the fat gate, and apply. Hidden rows (the union of every
previous drop) scored **out**; they are not candidates again.

| Bucket | Meaning | May drop? |
| --- | --- | --- |
| **Live** | Newest messages: current user turn, at least two. | No |
| **Keep** | Users, previous-turn reads and conclusions, latest writes and edits. | No |
| **Candidate** | Visible, before the live tail, not Keep. | Yes |
| **Hidden** | Already off the wire. | Already gone |

The listing the prune model sees, in order:

```text
## Ledger     previous keep/drop (omitted if this is the first prune)
## Live       do not drop — current task
## Keep       do not drop — users, latest mutations, previous-turn reads
## Candidates may drop
```

It may only name Candidates. Digits that fall in Live or Keep are
ignored at apply time, even if the model writes them. Ids already on a
ledger **drop** line are Hidden, so they are not listed again.

Each line is clipped so the listing itself cannot blow the prune
context: user and assistant ~240 characters, tool results ~200,
assistant-with-tools ~120. The whole listing stops around 80k
characters.

The next four pages are the buckets in isolation. After that, the
ledger is how those decisions persist. **Users never hide.**
