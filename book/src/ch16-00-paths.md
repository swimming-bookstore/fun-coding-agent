# Reference

| Path | Contents |
| --- | --- |
| `~/.config/fun/config.json` | Palette, action chips, optional auth path, home branch |
| `~/.local/share/fun/auth.json` | xAI OAuth tokens |
| `~/.local/share/fun/sessions/<encoded-cwd>/*.jsonl` | Transcript, usage, prune ledger |

Override config with `FUN_CODING_AGENT_CONFIG`. Override auth with
`PROVIDER_GROK_AUTH` or `"auth"` in config. `XDG_CONFIG_HOME` and
`XDG_DATA_HOME` move the parents.

Workspace for tools is `--dir` or the current directory. Session files
are keyed by that **canonical** path, not by git remote. Two clones in
two directories are two session folders.

The encoded folder name is a sanitized cwd plus an 8-hex digest, so
paths that only differ by punctuation still do not collide.

Fun never writes config by itself. It does create the data directories
on first login and first session.
