# Headless prompts

Any extra words after `fun` that are not `login`, `logout`, or
`list-sessions` are a prompt. Fun still uses a session (latest,
`--new`, or `--session`) and still writes jsonl. There is no TUI: the
process prints the stream to the terminal and exits when the turn is
done.

```sh
fun "fix the tests"
fun --image shot.png "what is wrong in this screenshot"
fun --new "add a failing test first"
fun --dir ~/src/my-project --new "run cargo test and fix failures"
```

The agent loop is the same as the TUI: prune at the start of the user
turn if the gate fires, tools, 200-round cap. You cannot queue or
steer; there is no composer. Ctrl+C kills the process.

Prune inspect lives in the TUI. Headless logs stay quiet about prune
events. If you need to see which indexes were hidden, open the same
session with `fun` (no extra words) and click the dim drop line, or
read the `{"type":"prune",…}` lines in the jsonl.

Usage still updates in the session file. The next TUI open will show
those totals on the status line.
