# write

Create or overwrite a file. Parent directories are created.

| Argument | Type | Meaning |
| --- | --- | --- |
| `path` | string | Required. Relative or absolute. |
| `content` | string | Full contents. This is the entire file after the call. |

Result:

```text
wrote N bytes to <path>
```

Prefer `write` for new files or wholesale replacement. Prefer `edit`
when changing a unique snippet — `write` of a whole file on every
tweak burns context and makes later prunes more likely to hide the
body (the latest write per path is Keep, but the *content* of old
writes is still large until hidden).

`write` is not append. To add a line, `read` then `edit`, or `write`
the whole new file.
