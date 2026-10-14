# Fun coding agent

CLI: `fun` (terminal UI).

```sh
cargo run --bin fun -- login
cargo run --bin fun -- --file notes.pdf "summarize this"
cargo run --bin fun
fun "fix the tests"
```

Shared crate: `crates/fun-core` (agent, tools, Grok, Vertex AI, sessions, config).

Docs: mdBook in `book/src/`. Preview: `mdbook serve book`.

Click the model name at the bottom right to pick a model. That choice is the provider.

xAI uses `fun login`. Vertex uses Application Default Credentials. The Agent Platform API must be enabled, and billing must be on. Replace `PROJECT_ID` with your project id:

```sh
gcloud auth application-default login
export GOOGLE_CLOUD_PROJECT=your-project
export GOOGLE_CLOUD_LOCATION=global
gcloud services enable aiplatform.googleapis.com --project=PROJECT_ID
```

Or put them in `~/.config/fun/config.json` (environment variables win):

```json
{
  "google_cloud_project": "your-project",
  "google_cloud_location": "global"
}
```

Or open:

https://console.developers.google.com/apis/api/aiplatform.googleapis.com/overview?project=PROJECT_ID

Then click `gemini-4-argon` in the model popup, or start on it:

```sh
FUN_CODING_AGENT_MODEL=gemini-4-argon cargo run --bin fun -- --file notes.pdf "summarize this"
```

Config: `~/.config/fun/config.json`  
Auth: `~/.local/share/fun/auth.json`  
Sessions: `~/.local/share/fun/sessions/`

Grok tokens stay on this machine. Point other apps at the same file with `PROVIDER_GROK_AUTH`, `provider_grok::set_auth_path`, or `"auth"` in config.
