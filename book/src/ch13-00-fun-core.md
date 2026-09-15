# fun-core

`fun-core` is the library behind the `fun` binary. The TUI is a thin
front-end: it paints `LogLine`s, edits the composer, and sends
interrupt / steer / idle / abort through a mailbox.

`fun-core` is not a binary. This page maps the source if you open it
while reading a session file.

| Module | Role |
| --- | --- |
| `agent` | Loop, [mailbox](ch12-01-mailbox.md), `maybe_prune`, logging |
| `tool` | `read` / `write` / `edit` / `bash`, abort, timeouts |
| `session` | jsonl, usage, hidden set, ledger load/save |
| `prune` | Buckets, listing, apply, restore stubs |
| `grok` | Streaming complete against xAI |
| `config` | Palette, actions, auth path, home branch |

`provider-grok` holds the OAuth tokens. `fun-core::grok` wraps it for
chat completions and maps “not logged in” to `fun login`.
