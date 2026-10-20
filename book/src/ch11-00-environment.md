# Environment

These variables are read at process start.

| Variable | Default | Meaning |
| --- | --- | --- |
| `FUN_CODING_AGENT_MODEL` | `grok-4.7` | Model id. `vertex/…`, `google/…`, and a bare `gemini-*` id use Vertex. The TUI popup shows the provider. |
| `FUN_CODING_AGENT_EFFORT` | `medium` | Reasoning effort for real completes. `low` / `minimal` → low; `high` / `xhigh` / `x-high` → high; anything else → medium. **Prune calls always use `low`**, regardless of this. On Vertex, Gemini 2.5 maps this to a thinking budget. Gemini 3 and Gemini 4 map it to a thinking level. Argon rejects `MINIMAL` and `thinkingBudget`. |
| `FUN_CODING_AGENT_CONFIG` | `~/.config/fun/config.json` | Config path. |
| `PROVIDER_GROK_AUTH` | `~/.local/share/fun/auth.json` | xAI token file. Wins over `"auth"` in config. |
| `GOOGLE_CLOUD_PROJECT` | config, then ADC `quota_project_id` | Vertex project. `GCLOUD_PROJECT` is also read. |
| `GOOGLE_CLOUD_LOCATION` | config, else `global` | Vertex region, or `global`. |
| `GOOGLE_APPLICATION_CREDENTIALS` | `~/.config/gcloud/application_default_credentials.json` | ADC or service-account JSON. |
| `XDG_CONFIG_HOME` | `~/.config` | Parent of `fun/config.json`. |
| `XDG_DATA_HOME` | `~/.local/share` | Parent of `fun/auth.json` and `fun/sessions/`. |
| `HOME` | required | Fallback when XDG vars are unset. |

Example:

```sh
FUN_CODING_AGENT_EFFORT=low fun
FUN_CODING_AGENT_MODEL=grok-4.7 fun --new "quick pass"
FUN_CODING_AGENT_MODEL=vertex/gemini-2.5-pro fun
FUN_CODING_AGENT_MODEL=gemini-4-argon fun
```

There is no env var to disable pruning. Prune is gated on payload size
and candidate count; a small session never calls it.
