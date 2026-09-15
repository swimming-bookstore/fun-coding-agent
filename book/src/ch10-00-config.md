# Config

Optional `~/.config/fun/config.json`. Override the path with
`FUN_CODING_AGENT_CONFIG`. Missing file, empty file, or invalid JSON
keeps the compiled defaults. Fun does not write this file for you.

```json
{
  "auth": "~/.local/share/fun/auth.json",
  "home": "master",
  "colors": {
    "text": "#e2e6f1",
    "muted": "#7a829a",
    "accent": "#7dcfef",
    "user": "#f7a878",
    "agent": "#c4a7f7",
    "tool": "#e8c468",
    "ok": "#9ece6a",
    "error": "#f38ba8",
    "code": "#89dcd2",
    "select": "#34436e",
    "queue": "#2a4056",
    "think": "#948ca8",
    "think_border": "#9a82c4"
  },
  "actions": [
    {
      "label": "[ checkout and pull {home} ]",
      "prompt": "checkout and pull {home}",
      "color": "ok",
      "when": "git"
    },
    {
      "label": "[ commit to {branch} and push ]",
      "label_home": "[ commit and push ]",
      "prompt": "commit to {branch} and push",
      "prompt_home": "commit and push a new branch",
      "color": "accent",
      "ask": "init git on {home} if needed, add origin {origin}, then commit and push"
    }
  ]
}
```

## Keys

| Key | Meaning |
| --- | --- |
| `auth` | Token file. A leading `~` is expanded. `PROVIDER_GROK_AUTH` still wins. |
| `home` | Preferred trunk name when git has none yet (`master` / `main` / `dev` are also probed). |
| `colors` | Palette. Any omitted color keeps its default hex. |
| `actions` | Replace the default chips. Omit the key to keep defaults. An empty array draws no chips. |

Action fields are described in [Action chips](ch06-03-actions.md).
Color values on actions are **names** (`ok`, `accent`, …), not hex;
they index this palette.

Restart `fun` after editing the file. The TUI does not watch it.
