# Prune constants

Hard numbers, for when you are staring at a session and wondering why
prune did or did not fire.

| Name | Value |
| --- | --- |
| Fat token threshold | 20 000 last *real* input tokens |
| Fat char threshold | 32 000 visible chars in the live payload |
| Chars per token (estimate) | 4 |
| Minimum live tail | 2 newest messages (and the current user turn) |
| Minimum candidates | 2 |
| Listing cap | ~80k characters |
| Prune model effort | `low`, no tools |
| Ledger | In/out score from previous keep and drop; next send folds that |
| How often | At most once per user turn, before the first reply |
| Restore | Latest successful write/edit per path, compact stub, max 40 paths |
| Tool round cap (agent) | 200 |

```text
next send = last keep ∪ messages after that turn − later drop
            + every user message
            + restore stubs
```

| File | Role |
| --- | --- |
| `crates/fun-core/src/prune.rs` | Buckets, listing, apply, restore |
| `crates/fun-core/src/session.rs` | jsonl, ledger |
| `crates/fun-core/src/agent.rs` | `maybe_prune` |

These constants are compiled in. There is no config key to change the
fat threshold.
