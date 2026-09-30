# First session

From a project directory, after `fun login`:

```sh
fun
```

Fun opens the **latest** session for that directory, or creates one if
this workspace has none. The screen is a transcript on top and a
composer at the bottom, marked `>`. Type a task and press Enter.

Example:

```text
> fix the failing tests in fun-core
```

## What happens

1. Your line is appended to the session as a user entry. You see it in
   the transcript immediately.
2. If the live payload is already fat (a long resumed session), Fun may
   prune first. There is no spinner for that. You may get a dim
   `(dropped N messages from context)` line.
3. A spinner starts. Grok streams **thinking** into a dim box, then
   **text** into the transcript.
4. If the model calls tools, Fun runs them in the workspace. Each run
   shows as `name(args)` plus a short result. Click the group to
   expand.
5. Fun loops until the model replies with no tools, you abort, or it
   hits 200 tool rounds.

Default model is `grok-4.7`. Default reasoning effort is `medium`.
Both can be changed; see [Environment](ch11-00-environment.md).

## Starting fresh, or elsewhere

| Flag | Meaning |
| --- | --- |
| `--new` | Create a session instead of resuming the latest for this workspace. |
| `--dir PATH` | Use another workspace. Created if missing, then canonicalized. |
| `--session PATH` | Open that jsonl. Error if the file is missing. |

```sh
fun --new
fun --dir ~/src/other-project
fun --session ~/.local/share/fun/sessions/…/….jsonl
```

`--dir` also scopes `list-sessions` and where new files are created.

## While it is working

You do not have to wait for the spinner to finish.

| You press | Fun does |
| --- | --- |
| Enter | Queue this composer text for **after** the turn |
| Ctrl+Enter | **Interrupt**: stop tools, inject the text and any attached images, restart the loop |
| Esc | **Abort**: stop without sending text. Transcript gets `(aborted)` |

The composer stays editable the whole time. See
[Queue, steer, interrupt](ch06-02-queue.md) for the third kind (steer)
and for editing queued chips.

## Leaving

Ctrl+C quits the TUI. The session file is already on disk; the next
`fun` in this directory reopens it.

Shift+Ctrl+C copies a mouse selection instead of quitting.

Next: [Command line](ch05-00-cli.md) for the full flag list, or
[Terminal UI](ch06-00-tui.md) for the screen.
