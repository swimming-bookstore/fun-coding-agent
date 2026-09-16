# Preface

Fun is a coding agent for a project directory. You type a task. Fun
calls Grok, which answers with text or with tool calls. The tools
read files, write them, patch them, or run a shell command. Results go
back to Grok until the work is done, you stop it, or a round cap
fires.

The workspace is the current directory, or `--dir`. Sessions are jsonl
files keyed by that path. Resume is opening the latest file for this
workspace. There is one provider: xAI Grok. Tokens stay on this
machine.

Four tools are the whole surface: `read`, `write`, `edit`, `bash`.
There is no search tool and no browser. If Grok needs a directory
listing, it reads a directory. If it needs tests, it runs them in the
shell.

When the live payload is fat, Fun does not summarize the transcript.
It hides named messages from the next API call and leaves the file and
the TUI intact. User lines always go. Dropped tool noise leaves holes
on purpose.

This book starts at install and a first session, then the TUI, the
tools, sessions, the agent loop, and pruning.

```sh
fun login
fun
```
