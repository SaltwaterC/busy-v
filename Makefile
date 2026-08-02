CC = clang
CFLAGS ?= -std=gnu11 -O2 -g -Wall -Wextra
CFLAGS += -Wno-sign-compare -Wno-unused-parameter -Wno-implicit-fallthrough -Wno-unused-result -ffunction-sections -fdata-sections
LDFLAGS += -Wl,--gc-sections

ifeq ($(OS),Windows_NT)
EXEEXT = .exe
else
EXEEXT =
endif

.PHONY: all clean analyze test c-reference

all: vi$(EXEEXT)

vi$(EXEEXT): Cargo.toml src/lib.rs src/main.rs
	cargo build --release
	cp target/release/vi$(EXEEXT) $@

analyze:
	$(CC) --analyze -w -std=gnu11 \
		-Xanalyzer -analyzer-checker=core,deadcode.DeadStores,unix.Malloc,unix.cstring \
		-Xanalyzer -analyzer-output=text vi.c
	cargo clippy -- -D warnings

c-reference: vi.c standalone.h
	$(CC) $(CFLAGS) $(LDFLAGS) -o vi-c vi.c

test: vi$(EXEEXT) c-reference
	./tests/smoke.sh
	./tests/reference.sh

clean:
	rm -f vi vi.exe
	cargo clean
