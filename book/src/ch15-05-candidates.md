# Candidates

Everything before the live tail that is not Keep and not already Hidden.

Fun needs **at least two** candidates or the prune call never happens.
There is no point asking the model if there is nothing it is allowed to
name.

Typical candidates:

- repeated or superseded tool output
- dead-end commands (`bash` that already failed, then you edited the file)
- old plans and chatter
- old tool-less conclusions that a later user line replaced
- older unique `read` dumps (not the previous turn’s latest read per path)

The prune prompt tells the model to prefer that noise so later file
edits stay. If nothing should go, it returns `{"drop":[]}`. Fun then
notes `(nothing to drop)` and still will not retry until the next user
line.

Candidates are listed with ids:

```text
## Candidates (may drop)
[1] assistant tools read path=old.png
[2] tool read (crop dump…)
```

The model should name ids, not ranges. Fun does not interpret `1-4` as
a range. JSON `{"drop":[1,2]}` is preferred; extra prose is ignored.
Digit runs in the reply are parsed only if they match a candidate id.
