# bash

Run a shell command in the workspace. Combined stdout and stderr,
capped at 20 KB. **Not sandboxed.** There is no TTY — a sudo or
password prompt fails instead of hanging the TUI.

| Argument | Type | Meaning |
| --- | --- | --- |
| `cmd` | string | Required. Passed to `/bin/sh -c`. |
| `timeout` | integer | Seconds before kill. Default 30, max 600. |

Fun sets `GIT_TERMINAL_PROMPT=0` and similar so git/ssh do not try to
talk to the TUI. `PATH` is the process PATH plus a small Unix fallback
(`/usr/bin:/bin:…`) so common tools resolve.

Non-zero exit is a tool error:

```text
exit <status>: <output>
```

Empty output becomes `(no output)`.

On timeout:

```text
bash timed out after Ns
```

On abort (Esc / interrupt), the process group is killed. The session
gets a skipped/aborted tool row, not a hang.

Long test runs should pass `"timeout": 120` (or similar). The default
30s is easy to hit on a cold `cargo test`.
