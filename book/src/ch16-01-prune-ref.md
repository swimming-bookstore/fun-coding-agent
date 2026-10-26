# New-chat gate, then prune

Hard numbers, for when you are staring at a session and wondering why
Fun started a new chat or hid old messages instead.

| Name | Value |
| --- | --- |
| Fat token threshold | 20 000 last *real* input tokens |
| Fat char threshold | 32 000 visible chars in the live payload |
| Chars per token (estimate) | 4 |
| Minimum candidates | 2 |
| Listing cap | ~80k characters |
| Decision effort | `low`, no tools |
| New-chat reply | `{"new":true}` or `{"new":false}` — anything else stays |
| Stay reply | `{"drop":[ids]}` — only candidate ids hide |
| How often | Once per user message, before that message is stored |
| Queue | Interrupt, steer, and idle stay queued. They are not dropped. |
| Restore | Latest write/edit per path, compact stub, max 40 paths |
| Tool round cap (agent) | 200 |

```text
new chat  = save previous session, clear the screen, paint this user line, then the saved-path hint under it, send only it
stay      = hide named candidates, then send last keep ∪ later messages − drops
```

| File | Role |
| --- | --- |
| `crates/fun-core/src/agent.rs` | `maybe_new_chat`, then `prune_if_staying` |
| `crates/fun-core/src/session.rs` | jsonl, ledger, `Session::fork` |
| `crates/fun-core/src/prune.rs` | Buckets, listing, apply, restore |

These constants are compiled in. There is no config key to change the
fat threshold.
