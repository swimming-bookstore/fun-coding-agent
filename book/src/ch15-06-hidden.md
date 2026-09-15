# Hidden

Hidden is every id that scored **out**: the union of every previous
ledger **drop** list, after pairing and lock stripping. Those ids are
not listed as candidates again. They remain on disk and in the TUI.
Scrolling up still shows the full bash dump.

Hidden is not a fifth listing section. The prune model does not
re-decide them. There is no “unhide” command. The only way a hidden
row appears on the wire again is:

- it is a **user** message (always sent; and Fun will not keep it in
  the hidden set)
- it is a [restore stub](ch15-09-restore.md) for a latest write/edit

Apply never deletes a transcript row. It only adds ids to the hidden
set and appends a prune record.

On resume, hidden is rebuilt from the ledger (or from the `hidden`
field on prune lines), then sanitized. Stale user ids in old files are
dropped from the set so later constraints always go to the model.
