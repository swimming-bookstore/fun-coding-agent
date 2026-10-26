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
{"type":"user","text":"what is this","images":[{"media":"png","data":"…","name":"shot.png"}]}
{"type":"assistant","text":"I’ll read…","thinking":"…","calls":[…]}
{"type":"tool","id":"…","name":"read","content":"…","is_error":false}
{"type":"prune","hidden":[3,4],"dropped":2,"added":[3,4],"keep":[0,1,2,5,6],"restored":[]}
{"type":"usage","input_tokens":…,"output_tokens":…,"cached_tokens":…,"reasoning_tokens":…,"last_input_tokens":0}
```

Entries you care about as a reader:

| `type` | Role |
| --- | --- |
| `header` | Once, at the top. Workspace cwd, id, created time. |
| `user` | A prompt. Optional `images` (png / jpeg / gif / webp, base64). Index = number of entries before this line. |
| `assistant` | Text, optional thinking, optional `calls`. |
| `tool` | One result, matched to a call by `id`. |
| `prune` | Ledger update. Not an index. |
| `usage` | Running totals. `last_input_tokens` is the last *real* complete, not a prune call. |

## Resume

On resume, Fun reloads the file, including any older prune ledger.
New turns do not hide messages. When the context is fat, the next user
line may start a **new chat** instead: the previous file stays where it
is, the screen clears, and only that line is sent. See
[New chat, not a hidden transcript](ch15-00-pruning.md).

The TUI notes a resumed file:

```text
(resumed …, N entries, M pruned from context)
```

`M` is history from an older hide. A chat started by the new-chat
decision has no such line until you resume something that still has one.

`--session PATH` opens a specific file (error if missing). `--new`
creates a new id in the same workspace folder. The file is not written
until the first message. Quitting an empty chat, including a header-only
file from an older build, removes it instead of leaving a blank session.

Deleting a jsonl is how you throw a session away. Fun does not garbage
collect.
