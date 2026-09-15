# Command line

```text
fun [--dir DIR] [--new] [--session PATH] [COMMAND]
```

Flags apply to every command, including `login` and `list-sessions`.
Sessions are scoped to `--dir`, or to the current directory if you omit
it.

## Flags

| Flag | Meaning |
| --- | --- |
| `--dir DIR` | Workspace root. Created if missing, then canonicalized. Tools and session lookup use this path. |
| `--new` | Create a session instead of opening the latest jsonl for this workspace. |
| `--session PATH` | Load that jsonl. Error if the file is missing. |

`--new` and `--session` together: `--session` wins if the file exists;
you do not create a second file by accident.

## Commands

| Command | Meaning |
| --- | --- |
| `fun` | Interactive TUI. |
| `fun login` | xAI device flow. Writes the token file. |
| `fun logout` | Delete the token file. |
| `fun list-sessions` | Print session files for this workspace, newest first. |
| `fun <words…>` | Headless prompt. Any first word that is not a known command is the start of the prompt. |

Known commands are only `login`, `logout`, and `list-sessions`. This
is valid:

```sh
fun fix the tests
fun "fix the tests"
fun --new add a failing test first
```

The prompt is the remaining arguments, joined with spaces. Quotes are
for the shell, not for Fun.

## Examples

Resume the latest session in the current directory:

```sh
fun
```

Fresh session, same directory:

```sh
fun --new
```

Work on another tree without `cd`:

```sh
fun --dir ~/src/my-project
```

One-shot in that tree:

```sh
fun --dir ~/src/my-project --new "fix the tests"
```

See what you already have:

```sh
fun list-sessions
```

Headless behavior — same agent, no TUI — is
[Headless prompts](ch07-00-headless.md).
