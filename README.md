# Fun coding agent

CLI: `fun`

```sh
cargo run --bin fun -- login
cargo run --bin fun
fun "fix the tests"
```

Config: `~/.config/fun/config.json`  
Auth: `~/.local/share/fun/auth.json`  
Sessions: `~/.local/share/fun/sessions/`

Grok tokens stay on this machine. Point other apps at the same file with `PROVIDER_GROK_AUTH`, `provider_grok::set_auth_path`, or `"auth"` in config.
