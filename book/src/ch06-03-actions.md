# Action chips

Action chips are canned prompts drawn as a row above the composer. They
are **not** tools. Clicking one sends a user turn — the same as if you
had typed the filled prompt and pressed Enter.

The defaults exist so a git workspace can checkout, pull, commit, and
push without retyping the branch name. You can replace the whole list
in [config](ch10-00-config.md). If `actions` is omitted, Fun uses the
built-in two chips below.

## What you see

- `[ checkout and pull {home} ]` — hidden until the workspace is a git
  repo (`"when": "git"`).
- `[ commit to {branch} and push ]` — on a feature branch.
- `[ commit and push ]` — when you are already on trunk
  (`label_home` / `prompt_home`).

`{home}` and `{branch}` are filled **before** the chip is painted, so
you see real names: `[ checkout and pull master ]`.

## Tokens

| Token | Meaning |
| --- | --- |
| `{home}` | Trunk. Fun looks at local `master` / `main` / `dev`, then `origin/HEAD`, then `"home"` in config if git has no branch yet. |
| `{branch}` | Current checkout. Before `git init`, this is `{home}`. |
| `{origin}` | The URL you type into the ask box. Only used in `"ask"` templates. |

`"when"` hides a chip:

| Value | Show when |
| --- | --- |
| omitted / always | Always |
| `git` | Workspace is a git repo |
| `origin` | A repo **and** a remote named `origin` |

`"color"` is a palette key: `accent`, `ok`, `user`, `agent`, `tool`,
`muted`, `text`, `error`.

## Ask

If `"ask"` is set and the workspace has no `origin`, Fun opens a small
dialog instead of sending immediately. You type a URL. That value fills
`{origin}` in the **ask** template, then Fun sends that string — not
the ordinary `prompt`.

The default commit chip uses this so a brand-new directory can
`git init`, add origin, commit, and push in one user message.

If `origin` already exists, Fun skips the dialog and sends `prompt` or
`prompt_home` as usual.

## Clicking while a turn is running

An action click is a user prompt. If a turn is already running, Fun
treats it like composer Enter: the filled text is **queued**, not
interrupting. Use Ctrl+Enter if you meant to stop now.
