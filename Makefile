CC = clang
CFLAGS ?= -std=gnu11 -O2 -g -Wall -Wextra
CFLAGS += -Wno-sign-compare -Wno-unused-parameter -Wno-implicit-fallthrough -Wno-unused-result -ffunction-sections -fdata-sections
LDFLAGS += -Wl,--gc-sections

.PHONY: all clean analyze test

all: vi

vi: vi.c standalone.h
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ vi.c

analyze:
	$(CC) --analyze -std=gnu11 \
		-Xanalyzer -analyzer-checker=core,deadcode.DeadStores \
		-Xanalyzer -analyzer-output=text vi.c

test: vi
	./tests/smoke.sh

clean:
	rm -f vi
