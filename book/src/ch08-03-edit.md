# edit

Replace `old` with `new` in an existing file. **`old` must appear
exactly once.** That rule is the whole point: if the snippet is missing
or appears twice, the tool errors with the count, and the model has to
`read` again instead of guessing.

| Argument | Type | Meaning |
| --- | --- | --- |
| `path` | string | Required. |
| `old` | string | Exact text to find. Must not be empty. |
| `new` | string | Replacement. May be empty (delete the snippet). |

Result:

```text
edited <path>
```

`old` is exact: whitespace, quotes, and newlines matter. A mismatch is
not a fuzzy patch.

Typical failure:

```text
old not found
```

or a count greater than one. Both are tool errors (`is_error: true` in
the session). The model should `read` the file and try a unique
snippet.

`edit` will not create a file. Use `write` for new paths.
