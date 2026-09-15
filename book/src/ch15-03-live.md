# Live

The live tail is the current task. Fun always locks at least the newest
**two** messages, and the tail never starts after the last user message
in that prefix.

At prune time the last entry **is** that user line, so live is almost
always:

```text
[n-2] previous assistant   live
[n-1] your new prompt      live
```

If the current user turn is longer than two messages (interrupt stacked
on interrupt before a complete), the whole turn is live, even if that
is more than two rows.

Those rows are shown verbatim (clipped) under `## Live`. They cannot
hide.

Mid-turn tool results sit here too, which is why Fun refuses to prune
after each tool: those results are the thing you just asked Grok to
look at. Hiding them in the same turn would be hiding the work in
progress.

Live is not “the last user message only.” The assistant immediately
before your new prompt is usually the plan you are continuing. Dropping
it would make the model redo that plan from scratch.
