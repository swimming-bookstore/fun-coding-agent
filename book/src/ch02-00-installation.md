# Installation

Fun is a Rust workspace. The default binary is `fun`. Build a release
binary and put it on `PATH`:

```sh
cargo build --release --bin fun
install -m 755 target/release/fun ~/.local/bin/fun
```

`~/.local/bin` must be on `PATH`. Then:

```sh
fun login
fun
```

## Without installing

From the source tree, the same commands work as cargo subcommands. Put
`--` before Fun’s own flags:

```sh
cargo run --bin fun -- login
cargo run --bin fun
cargo run --bin fun -- --new "fix the tests"
```

A debug build is slower to start but fine for trying the TUI.

## What you need

- A Rust toolchain that can compile the workspace.
- A terminal that speaks ANSI and mouse events (the TUI uses both).
- Network access for `fun login` and for completes.

There is no extra runtime besides the binary. Config, auth, and
sessions are ordinary files under XDG paths; Fun creates the
directories it needs.

## Check that it works

```sh
fun --help
```

should print the CLI. If `fun` is not found, `~/.local/bin` is not on
`PATH` in this shell.

Next: [Login](ch03-00-login.md).
