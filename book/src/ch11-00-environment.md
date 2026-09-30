# Environment

These variables are read at process start.

| Variable | Default | Meaning |
| --- | --- | --- |
| `FUN_CODING_AGENT_MODEL` | `grok-4.7` | Model id for real completes. |
| `FUN_CODING_AGENT_EFFORT` | `medium` | Reasoning effort for real completes. `low` / `minimal` → low; `high` / `xhigh` / `x-high` → high; anything else → medium. **Prune calls always use `low`**, regardless of this. |
| `FUN_CODING_AGENT_CONFIG` | `~/.config/fun/config.json` | Config path. |
| `PROVIDER_GROK_AUTH` | `~/.local/share/fun/auth.json` | Token file. Wins over `"auth"` in config. |
| `XDG_CONFIG_HOME` | `~/.config` | Parent of `fun/config.json`. |
| `XDG_DATA_HOME` | `~/.local/share` | Parent of `fun/auth.json` and `fun/sessions/`. |
| `HOME` | required | Fallback when XDG vars are unset. |

Example:

```sh
FUN_CODING_AGENT_EFFORT=low fun
FUN_CODING_AGENT_MODEL=grok-4.7 fun --new "quick pass"
```

There is no env var to disable pruning. Prune is gated on payload size
and candidate count; a small session never calls it.
