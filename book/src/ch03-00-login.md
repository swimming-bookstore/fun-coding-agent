# Login

xAI Grok is the only provider. Sign-in is the OAuth **device flow**:
Fun prints a URL and a short code; you open the URL in a browser, enter
the code, and wait. When the flow finishes, tokens are written on this
machine. Completes use those tokens. Nothing is stored in the session
file.

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

## Where the file lives

The default path is `$XDG_DATA_HOME/fun/auth.json`. If
`XDG_DATA_HOME` is unset, that is `~/.local/share/fun/auth.json`.

| Override | Effect |
| --- | --- |
| `PROVIDER_GROK_AUTH` | Token path. Wins over everything else. |
| `"auth"` in `~/.config/fun/config.json` | Token path. A leading `~` is expanded. |

Point another Fun process at the same file if you want them to share a
login. Do not copy the file into a repository. Do not commit it.

The OAuth scope includes API access for Grok. You do not configure
scopes yourself.

Next: [First session](ch04-00-first-session.md).
