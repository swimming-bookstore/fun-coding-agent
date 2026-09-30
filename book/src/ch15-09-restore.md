# Restore stubs

Keep means “never hide this row.” Restore means “this row is still
hidden, but send a tiny reminder so a later review does not treat
finished work as dead.”

Latest successful writes and edits per path are droppable. Hiding the
fat body is the point — a 4k `write` of `login.rs` should not stay on
the wire forever. Disk stays hidden. On the **next** payload, those
rows come back as compact stubs (path only) so “final code review /
cut dead code” still sees that the file exists.

Without this, the review only had the user line plus the live tail and
deleted finished work as leftover.

```text
on disk                      on the wire
write login.rs               write path=login.rs restored=true
wrote 4k bytes               wrote 4k bytes (clipped)
bash cargo test              (gone)
test dump                    (gone)
```

| Rule | Detail |
| --- | --- |
| What | Latest successful write or edit per path, plus the assistant call that issued it |
| Stub | `{"path":"…","restored":true}` — no file body. Assistant text clipped, thinking dropped |
| Cap | 40 paths. Bash dumps never come back |
| Disk / TUI | Still hidden. The full row is what you see when you scroll |

`, restored R` on the dim line is only for newly restored writes and
edits this turn, not for a later drop of bash noise.

Restore does not unhide. The jsonl row is unchanged. Only
`model_entries()` grows a stub.
