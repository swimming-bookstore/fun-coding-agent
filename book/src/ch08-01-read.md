# read

Read a file, or list a directory.

| Argument | Type | Meaning |
| --- | --- | --- |
| `path` | string | Required. Relative to the workspace, or absolute. |
| `offset` | integer | 0-based start line. Default 0. |
| `limit` | integer | Max lines. Default 500. |

## Files

Lines are numbered for display:

```text
     1	fn main() {
     2	    println!("hi");
     3	}
```

**Do not copy those numbers into `edit` or `write`.** They are not in
the file.

Output is capped at 50 KB. Over that, Fun appends
`... (truncated at 50KB; offset=…)` so the model can page with a
higher `offset`.

## Directories

A directory lists names, sorted. Directories get a trailing `/`. There
is no recursive tree; the model `read`s children it cares about.

## Binary

Binary files do not fail UTF-8. PNG reports width and height. JPEG,
GIF, WebP, and other NUL-containing files report type and size instead
of a garbled dump.
