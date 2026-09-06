# Fun coding agent

CLI: `fun` (terminal UI). GTK 4: `fun-gtk4` in `~/fun-coding-agent-gtk4`.

```sh
cargo run --bin fun -- login
cargo run --bin fun
fun "fix the tests"

# other repo, shares fun-core
cargo run --manifest-path ../fun-coding-agent-gtk4/Cargo.toml --bin fun-gtk4
```

Shared crate: `crates/fun-core` (agent, tools, Grok, sessions, config).

Config: `~/.config/fun/config.json`  
Auth: `~/.local/share/fun/auth.json`  
Sessions: `~/.local/share/fun/sessions/`

Grok tokens stay on this machine. Point other apps at the same file with `PROVIDER_GROK_AUTH`, `provider_grok::set_auth_path`, or `"auth"` in config.
