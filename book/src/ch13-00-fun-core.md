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
| `vertex` | Streaming complete against Google Cloud Vertex AI |
| `provider` | `Grok` or `Vertex` behind one complete API |
| `config` | Palette, actions, auth path, home branch |

`provider-grok` holds the xAI OAuth tokens. `fun-core::grok` wraps it
for chat completions and maps “not logged in” to `fun login`.
`fun-core::vertex` calls Vertex with Application Default Credentials
(`gcloud auth application-default login`, plus `GOOGLE_CLOUD_PROJECT`
and `GOOGLE_CLOUD_LOCATION`). There is no Fun-side Vertex login. The same
project and location can be set as `google_cloud_project` and
`google_cloud_location` in `~/.config/fun/config.json`; environment variables
win. A Gemini model id selects Vertex. Click the model name in the TUI, or set
`FUN_CODING_AGENT_MODEL=gemini-4-argon`.
