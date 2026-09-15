# Tools

Fun exposes four tools to Grok. That is the whole surface: there is no
search tool and no browser. If the model needs a directory listing, it
`read`s a directory. If it needs to run tests, it `bash`es.

The system prompt tells the model:

- who it is (Fun coding agent)
- the four names
- read a file before editing it
- prefer `edit` for small changes
- paths are relative to the workspace unless absolute
- `read` / `write` / `edit` may use paths outside the workspace
- bash times out after 30s unless `timeout` is passed (max 600)
- verify with tools before claiming done
- keep replies short
- the workspace path

You do not edit that prompt. Changing behavior is changing the
workspace, the session, or the model/effort env vars.

## Paths

`read`, `write`, and `edit` resolve a path like this:

1. Empty path is an error.
2. Absolute paths stay absolute (after cleaning `.` / `..`).
3. Relative paths join the workspace, then clean.
4. If the path exists, Fun uses the canonical file.
5. If it does not, Fun canonicalizes the parent when it can, so `write`
   can create a new file next to real directories.

`bash` always starts with **current directory = workspace**. The
command itself is not jailed; `cd /tmp && pwd` works. There is no
sandbox. There is no TTY.

## Abort and timeout

Tools run with an abort flag. Esc, interrupt, and adopt set it.

| Tool | Timeout | Abort |
| --- | --- | --- |
| `read` / `write` / `edit` | 30s | Checked around the work; an Esc mid-read should not still write |
| `bash` | 30s default, max 600s | Process group killed, then reaped |

After **200** tool rounds Fun stops and records skipped tool results so
the transcript stays paired. You will see
`(stopped after too many tool rounds)`.

The next four pages are the JSON the model actually sends.
