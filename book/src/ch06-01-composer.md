# Composer

The composer is the `>` line at the bottom. It is not a plain string.
Fun stores it as a list of **atoms**: ordinary characters, and
**paste chips**.

That split exists because a 400-line log pasted into the prompt would
wrap the whole TUI and bury the cursor. A chip is a one-cell label in
the line. The full body is still what Grok gets on send.

## Atoms

Typing `fix tests` is nine character atoms (including the space). The
cursor sits *between* atoms, so Left and Right skip a chip in one step.
Backspace on a chip drops the whole paste, not one character of it.

## When a paste becomes a chip

A paste becomes a chip when it is **120 characters or more**, or
**more than 3 lines**. The label is `paste N lines` or `paste N chars`.
Click the chip to drop it.

Smaller pastes insert as ordinary characters, including newlines.
Control characters other than newline are stripped.

On send, Fun concatenates atoms:

- each character as itself
- each paste chip as its stored body, wrapped with newlines so it does not
  glue to neighboring words
- each image chip as an image part, not as text

That string, plus any image parts, is the user entry written to jsonl and
shown in the transcript. The chip is only a composer convenience. The
transcript shows `[image name]` where the picture was attached. The
bytes go to Grok as `input_image` data URLs. They are not pasted into
the text.

## Images

Ctrl+V attaches a picture when the clipboard is a path to a local
`png`, `jpeg`, `gif`, or `webp` file. Otherwise Ctrl+V pastes the
clipboard as text, same as a bracketed paste.

An image is always a chip, even if the file is small. The label is
`image <basename>`. Click the `×` to drop it. Backspace on the chip
drops it too.

The file must be 4 MB or smaller. Fun stores the bytes as base64 on the
user entry. Older sessions without an `images` field still load.

Headless prompts use `--image PATH`, which can be repeated:

```sh
fun --image shot.png "what is wrong in this screenshot"
```

Queue, steer, and interrupt keep the images attached to that pending
turn. A queued chip shows `[image name]` in its label. Editing the chip
puts the image back in the composer.

## History

Up and Down walk every non-empty **user** line already in this session.
They do not include queued lines you have not sent yet.

Leaving the live line (the empty composer at the bottom of history)
saves a **draft**. Walking back to the bottom restores that draft.
Sending a line appends it to history.

## Bound to a queue slot

Click **edit** on a queued prompt and the composer loads that text. The
editor is then bound to that **queue slot**:

- Enter replaces the slot instead of appending a new idle item (or
  instead of starting a new turn, if you are idle).
- Esc while bound cancels the edit and unbinds. It does **not** abort
  the live turn.

The binding is visible as the queue chip staying highlighted while you
edit. Dropping that chip from the queue also unbinds.
