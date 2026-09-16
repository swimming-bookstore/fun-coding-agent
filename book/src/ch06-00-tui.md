# Terminal UI

`fun` with no extra words is a full-screen terminal. This chapter is
that screen: what each region is, what a turn looks like, and the keys.
The next three pages cover the composer, the queue, and action chips in
detail.

## Layout

From the top of the terminal:

```text
transcript          user lines, assistant markdown, tool runs, dim notes
thinking box        only while Grok is reasoning
interrupt strip     stop-now prompts (Ctrl+Enter)
steer box           after this tool batch (“send now”)
queue box           after this turn (Enter while working)
action chips        canned git prompts
composer            >
status              workspace · branch · tokens · model
```

The **transcript is the session**. Every user, assistant, and tool
entry is painted here. Pruning never removes a row from this view; it
only changes what Grok sees on the next complete. Colors come from
[config](ch10-00-config.md): user, agent, tool, muted, error, and so on.

**Thinking** streams into a dim italic box. After the answer starts,
that box holds for about two seconds and then hides. You can still
scroll the transcript to read it if it was painted as part of the
stream history for this turn.

**Tool runs** collapse to `name(args)` plus a short result. A group is
clickable: expand to see the full output Fun stored. Errors use the
error color.

**Status** is workspace path, git branch (if any), last usage
(input / output / cached / reasoning tokens), model id, and effort.

A dim prune line looks like:

```text
(dropped 4 messages from context)
```

or, when write/edit stubs went back on the wire:

```text
(dropped 4 messages from context, restored 2)
```

Click it to open the inspect overlay. Esc or a click outside closes
the overlay. Nothing auto-opens.

## A turn

You type `fix the tests` and press Enter.

1. The composer clears. The user line is painted and written to jsonl.
2. If the live payload is fat, Fun may prune. You get one dim line, or
   nothing if the gate did not fire.
3. A spinner starts. Thinking, then text, stream in.
4. Tool calls run in the workspace, one after another in the order the
   model asked.
5. Fun loops until Grok replies with no tools, you abort, or it hits
   200 rounds.

If the model’s stream is truncated, Fun drops that reply’s tool calls
and notes `(truncated; tool calls dropped)` so it does not run a
partial batch.

## Keys — idle

Nothing is running. The composer is a normal prompt.

| Key | Action |
| --- | --- |
| Enter | Send the composer as a user turn |
| Esc | Close a prune overlay if one is open |
| Ctrl+C | Quit. Shift+Ctrl+C copies a selection instead |
| PageUp / PageDown | Scroll the transcript (8 lines) |
| End | Jump to the bottom |
| Up / Down | Prompt history (previous user lines in this session) |

## Keys — a turn is running

| Key | Action |
| --- | --- |
| Enter | Queue the composer for after this turn |
| Ctrl+Enter | Interrupt: stop tools and the stream, inject the text, restart |
| Esc | Abort. Tools stop. Composer stays. Transcript gets `(aborted)` |

Abort and interrupt both set an abort flag the tools poll. Bash is
killed. `read` / `write` / `edit` stop at their next check. Skipped
tool calls still get a dummy result row so the transcript stays paired
(an assistant `calls` list never sits without matching tool entries).

## Keys — composer editing

These work whether a turn is running or not.

| Key | Action |
| --- | --- |
| Left / Right | Move by one atom (a character or a whole paste chip) |
| Home / Ctrl+A | Start of the line |
| Backspace / Ctrl+H | Delete backward |
| Delete | Delete forward |
| Ctrl+W | Kill the word before the cursor |
| Ctrl+U | Kill from the start of the line to the cursor |

Mouse: click to place the cursor, drag to select transcript text,
click a paste chip to drop it, click queue controls, click a prune
note, click action chips.

## Mouse selection

Drag in the transcript to highlight. Shift+Ctrl+C copies. A click
without dragging is not a selection; it is a hit test for tools, queue
chips, prune notes, or action chips.
