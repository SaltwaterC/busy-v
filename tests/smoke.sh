#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/busy-v.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM


# Give script(1) time to put the child into raw mode before sending input.
{ sleep 1; printf 'iHello busy-v\033:wq\n'; } |
	script -qefc "$root/vi $tmp/file" "$tmp/session.log" >/dev/null 2>&1

test "$(cat "$tmp/file")" = "Hello busy-v"

"$root/vi" -H 2>&1 | grep -q 'Pattern searches with / and ?'
cursor_show=$(printf '\033[?25h')
grep -a -F -q "$cursor_show" "$tmp/session.log"

raw_log="$tmp/raw.session.log"
raw_file="$tmp/raw-file"
printf 'abc\n' >"$raw_file"
{
	sleep 1
	printf 'iX'
	sleep 1
	if ! grep -a -F -q 'Xabc' "$raw_log" 2>/dev/null; then
		: >"$tmp/raw-mode-failed"
	fi
	printf '\033:q!\n'
} | script -qefc "$root/vi $raw_file" "$raw_log" >/dev/null 2>&1
test ! -e "$tmp/raw-mode-failed"

size_log="$tmp/size.session.log"
size_file="$tmp/size-file"
i=0
while test "$i" -lt 130; do
	printf a >>"$size_file"
	i=$((i + 1))
done
printf '\n' >>"$size_file"
{ sleep 1; printf 'A\033:q!\n'; } |
	script -qefc "stty rows 12 cols 120; exec $root/vi $size_file" "$size_log" >/dev/null 2>&1
size_escape=$(printf '\033[12;1H')
grep -a -F -q "$size_escape" "$size_log"
wide_cursor=$(printf '\033[1;120H')
grep -a -F -q "$wide_cursor" "$size_log"
prompt_cursor=$(printf '\033[?25h\033[12;4H')
grep -a -F -q "$prompt_cursor" "$size_log"

resize_log="$tmp/resize.session.log"
resize_file="$tmp/resize-file"
printf 'one\ntwo\nthree\nfour\n' >"$resize_file"
{ sleep 2; printf '\033:q!\n'; } |
	script -qefc "stty rows 12 cols 40; (sleep 1; stty rows 8 cols 30 </dev/tty) & exec $root/vi $resize_file" "$resize_log" >/dev/null 2>&1
resized_status=$(printf '\033[8;1H\033[K-')
grep -a -F -q "$resized_status" "$resize_log"

pane_log="$tmp/pane-resize.session.log"
pane_file="$tmp/pane-resize-file"
printf 'one\ntwo\nthree\nfour\n' >"$pane_file"
{
	sleep 1
	sleep 2
	printf 'l'
	sleep 0.2
	printf '\033:q!\n'
} | script -qefc "stty rows 12 cols 40; (i=0; while test \"\$i\" -lt 12; do if test \$((i % 2)) -eq 0; then stty rows 8 cols 30 </dev/tty; else stty rows 12 cols 40 </dev/tty; fi; i=\$((i + 1)); done) & exec $root/vi-c $pane_file" "$pane_log" >/dev/null 2>&1
pane_cursor=$(printf '\033[1;2H')
grep -a -F -q "$pane_cursor" "$pane_log"
pane_redraw=$(grep -a -F -o "$(printf '\033[H\033[J')" "$pane_log" | wc -l)
test "$pane_redraw" -ge 2

echo "busy-v smoke test: ok"
