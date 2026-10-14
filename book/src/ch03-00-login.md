# Login

Two providers can sign in. The model id picks the provider. Click the model
name at the bottom right of the TUI and choose a model. `fun login` is xAI only.

## xAI Grok

Sign-in is the OAuth **device flow**: Fun prints a URL and a short code;
you open the URL in a browser, enter the code, and wait. When the flow
finishes, tokens are written on this machine. Completes use those
tokens. Nothing is stored in the session file.

```sh
fun login
```

```sh
fun logout
```

`logout` deletes the token file, or prints `not logged in` if there is
nothing to delete.

A complete with no tokens fails with:

```text
not logged in — fun login
```

Access tokens refresh on their own, with a few minutes of skew. You
should not need to log in again until you `logout` or the refresh token
is revoked.

The default path is `$XDG_DATA_HOME/fun/auth.json`. If `XDG_DATA_HOME`
is unset, that is `~/.local/share/fun/auth.json`.

| Override | Effect |
| --- | --- |
| `PROVIDER_GROK_AUTH` | Token path. Wins over everything else. |
| `"auth"` in `~/.config/fun/config.json` | Token path. A leading `~` is expanded. |

Point another Fun process at the same file if you want them to share a
login. Do not copy the file into a repository. Do not commit it.

## Google Cloud Vertex AI

Vertex uses Application Default Credentials. There is no `fun login` for
Vertex. Sign in with `gcloud`, then set the project and location:

```sh
gcloud auth application-default login
export GOOGLE_CLOUD_PROJECT=your-project
export GOOGLE_CLOUD_LOCATION=global
FUN_CODING_AGENT_MODEL=gemini-4-argon fun
```

`GOOGLE_CLOUD_LOCATION` defaults to `global` when unset. The same two values can live in `~/.config/fun/config.json` instead of the environment. Environment variables win when both are set.

```json
{
  "google_cloud_project": "your-project",
  "google_cloud_location": "global"
}
```

If the ADC file has `quota_project_id`, that is used when neither the environment nor config sets a project.
Or set `GOOGLE_APPLICATION_CREDENTIALS` to a service-account JSON file.

The Agent Platform API must be enabled, and billing must be on, for that
project. Replace `PROJECT_ID` with your project id:

```sh
gcloud services enable aiplatform.googleapis.com --project=PROJECT_ID
```

Or open:

https://console.developers.google.com/apis/api/aiplatform.googleapis.com/overview?project=PROJECT_ID

If you just enabled it, wait a minute and retry. A 403 `SERVICE_DISABLED`
means this step is still missing.

ADC tokens are minted per request from the refresh token. Service-account
tokens are RS256 JWTs. Scope is
`https://www.googleapis.com/auth/cloud-platform`.

Requests go to
`https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:streamGenerateContent`.
`global` uses `https://aiplatform.googleapis.com`. Thinking follows
`FUN_CODING_AGENT_EFFORT`: Gemini 2.5 gets a token budget. Gemini 3 and
Gemini 4 get a thinking level. Gemini 4 Argon only accepts `LOW`,
`MEDIUM`, and `HIGH` (`minimal` is sent as `LOW`) and rejects a thinking
budget. One streaming Argon reply is capped at 65,536 output tokens.

Argon inputs are text, images, audio, video, and PDF. Drop a file on the
composer, or pass `--image` / `--file`. Inline limits: images 7 MB, PDF
50 MB, audio and video 20 MB.

Next: [First session](ch04-00-first-session.md).
