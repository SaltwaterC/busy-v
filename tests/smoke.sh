#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/standalone-vi.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM


# Give script(1) time to put the child into raw mode before sending input.
{ sleep 1; printf 'iHello standalone vi\033:wq\n'; } |
	script -qefc "$root/vi $tmp/file" "$tmp/session.log" >/dev/null 2>&1

test "$(cat "$tmp/file")" = "Hello standalone vi"

"$root/vi" -H 2>&1 | grep -q 'Pattern searches with / and ?'
echo "standalone vi smoke test: ok"
