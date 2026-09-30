# Inspecting

Click a dim `(dropped N messages from context)` line. The overlay is
titled “pruned from context” and lists those N hidden ids, one per
line, like:

```text
[3] assistant tools bash cargo test
[4] tool bash (exit 1: …)
```

If that prune also restored write/edit stubs, the overlay lists dropped
noise first, then `restored R:` with path-only lines. A restored write
is not repeated in the dropped list.

The resume line lists every currently hidden id. The drop line is
underlined when it has inspect data. Esc or a click outside closes.
Arrow keys and page keys scroll the overlay.

Nothing auto-opens. After a prune, Fun emits the inspect data and then
the dim line. You click it if you care.

On resume, hidden ids and the ledger reload from jsonl. User ids are
stripped. Drop lines are replayed in place from notes stored at
`(entries.len() at that moment, added ids)`. Opening the session also
notes `(resumed …, N entries, M pruned from context)`.

Headless logs skip prune events. Inspect is TUI-only. To debug a
headless run, open the jsonl and look for `"type":"prune"` or reopen
the same file in the TUI.
