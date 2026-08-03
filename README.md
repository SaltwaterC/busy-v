# busy-v

busy-v is a Rust port of the independently extracted BusyBox vi clone. The
original C implementation has been kept for reference and for comparison
testing.

The Rust implementation owns its text buffer and uses the cross-platform
`crossterm` crate for terminal integration. It provides command and insert modes, file I/O,
colon commands, small regex-style search/substitution, yank/put and named registers,
undo/redo, marks, multi-file navigation, startup `EXINIT`/`.exrc`,
autoindent, replacement, and common movement and editing commands. The
terminal integration uses the crate's native raw-mode, event, alternate-screen,
and sizing support on Unix and Windows; it does not require a shell utility or
project-local platform FFI for terminal handling.

While the editor is functional and care has been taken to make sure it matches the
reference version, there are two potential sources for bugs:

  1. Extraction bugs when the reference version was separated from BusyBox.
  2. Porting bugs where the feature was not reimplemented entirely the same.

This is not a c2rust port as the RustyBox experiment shows that 5kloc of C
can become 97kloc of unmaintainable and unreadable Rust while still importing
usafe calls. This is more of a reimplementation using the C version as reference.

That being said, the world does not need another vi clone. This project exists for
one purpose only: have a minimal vi-like editor that I can embed into
[Zetta](https://github.com/SaltwaterC/zetta) as fallback editor on all supported
platforms.

## Embedding and optional syntax highlighting

The Rust library is the embedding surface. `Editor::from_bytes` constructs an
editor without opening a file or taking over a terminal, and `execute_keys`
feeds it input from the host application. `Editor::run` is only the optional
standalone terminal frontend.

Syntax highlighting is an extension point, not a built-in language feature. An
embedding application can install its existing parser or highlighter through
`set_syntax_highlighter`; the base project does not contain grammars, parser
dependencies, or a theme. The highlighter receives the complete buffer and
returns sorted, non-overlapping byte ranges with terminal presentation styles:

```rust
use busy_v::{Editor, HighlightColor, HighlightSpan, HighlightStyle};

let mut editor = Editor::from_bytes(b"fn main() {}\n", None, false);
editor.set_syntax_highlighter(Box::new(|buffer: &[u8]| {
    let end = buffer.iter().position(|byte| *byte == b' ').unwrap_or(0);
    vec![HighlightSpan::new(
        0,
        end,
        HighlightStyle::foreground(HighlightColor::Ansi(75)),
    )]
}));

// The terminal frontend applies the spans during editor.run().
// A custom frontend can use editor.bytes() and editor.syntax_highlights().
```

Ranges use the same byte-oriented model as the editor buffer, so an adapter can
map an external syntax engine's token ranges without bringing that engine into
this crate. Call `invalidate_syntax_highlighting` when the host changes its
language or theme without changing the buffer.

Unlike the reference version, the resulting binary is large by comparison. Not by
a tiny margin. Almost 7X larger. I chose BusyBox's implementation as reference as
it makes porting far easier when the footprint after removing dead code is ~3kloc.

## Additional features

 * Native Windows and macOS support
 * :set number for line numbering including colouring when syntax higlight is
   plugged in
 * Page Up/Down work as expected

## Build and test

```sh
make
make analyze
make test
```

On Windows, `make` or `cargo build --release` builds `vi.exe`; the C reference
and POSIX PTY comparison scripts are Unix-only.

The tests include public core-editor integration tests in
`tests/editor_core.rs`, a Rust PTY smoke test, and reference functional tests
in `tests/reference.sh` that run identical keyboard sequences through `vi-c`
and `vi`. Arrow-key CSI sequences, including sequences split across reads, are
covered. The smoke test also verifies that terminal raw mode processes input
before a newline, that the configured terminal dimensions are used, and that
repeated pane-style resize/restore events leave the C reference cursor live.
`make analyze` runs Clang's core, dead-store, allocation, and C-string checks
on the C reference and Clippy on Rust. The source
is GPLv2-or-later, matching the BusyBox source from which the editor was
extracted.
