# Standalone vi

This directory is an independent extraction of BusyBox `editors/vi.c`. It
builds a regular POSIX-hosted `vi` executable without BusyBox or `libbb`.

The extracted build enables the normal vi features (colon commands, search,
yank/put, undo, signals, resizing, and read-only mode), uses POSIX regex, and
omits BusyBox's disabled CRASHME test code. `vi.c` is the specialized output
of Clang's preprocessor, so unused feature branches and their configuration
scaffolding are absent from the source. `standalone.h` supplies only the
small libc/POSIX compatibility routines still required by the editor.

Build and test:

```sh
make
make analyze
make test
```

The smoke test runs the binary under a pseudo-terminal, inserts text, and
saves it with `:wq`. The source is GPLv2-or-later, matching the BusyBox source
from which it was extracted.
