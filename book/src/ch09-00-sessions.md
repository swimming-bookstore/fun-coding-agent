# Sessions

A session is one jsonl file. Header, entries, usage, and prune records
share that file. A prune line is **metadata**, not a chat entry, so
indexes stay stable when Fun hides messages.

## Where files live

```text
~/.local/share/fun/sessions/<encoded-cwd>/<id>.jsonl
```

The folder name is a sanitized workspace path plus a short digest, so
two directories that look similar after replacing `/` still do not
collide. `XDG_DATA_HOME` moves the `fun` data root.

`fun` without `--new` opens the latest jsonl for this workspace, by
mtime. `fun list-sessions` prints them.

## What a file looks like

```json
{"type":"header","version":1,"id":"…","cwd":"/home/you/proj","createdAt":…}
{"type":"user","text":"fix the tests"}
{"type":"assistant","text":"I’ll read…","thinking":"…","calls":[…]}
{"type":"tool","id":"…","name":"read","content":"…","is_error":false}
{"type":"prune","hidden":[3,4],"dropped":2,"added":[3,4],"keep":[0,1,2,5,6],"restored":[]}
{"type":"usage","input_tokens":…,"output_tokens":…,"cached_tokens":…,"reasoning_tokens":…,"last_input_tokens":0}
```

Entries you care about as a reader:

| `type` | Role |
| --- | --- |
| `header` | Once, at the top. Workspace cwd, id, created time. |
| `user` | A prompt. Index = number of entries before this line. |
| `assistant` | Text, optional thinking, optional `calls`. |
| `tool` | One result, matched to a call by `id`. |
| `prune` | Ledger update. Not an index. |
| `usage` | Running totals. `last_input_tokens` is the last *real* complete, not a prune call. |

## Resume

On resume, Fun reloads entries, hidden ids, and the keep/drop ledger.
User messages are always sent again, even if an older prune line listed
them. The TUI notes:

```text
(resumed …, N entries, M pruned from context)
```

`--session PATH` opens a specific file (error if missing). `--new`
creates a new id in the same workspace folder.

Deleting a jsonl is how you throw a session away. Fun does not garbage
collect.
