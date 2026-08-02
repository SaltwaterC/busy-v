#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/busy-v-reference.XXXXXX")
trap 'rm -rf "$tmp"' EXIT HUP INT TERM

run_command_mode_case() {
    binary=$1
    file=$2
    log=$3
    {
        sleep 1
        printf '\033'
        sleep 0.03
        printf '[C'
        printf '\033[D'
        printf '\033[B'
        printf '\033[A'
        printf 'x:wq\n'
    } | script -qefc "$binary $file" "$log" >/dev/null 2>&1
}

run_basic_insert_case() {
    binary=$1
    file=$2
    log=$3
    {
        sleep 1
        printf 'iXY\033:wq\n'
    } | script -qefc "$binary $file" "$log" >/dev/null 2>&1
}

run_insert_home_end_case() {
    binary=$1
    file=$2
    log=$3
    {
        sleep 1
        printf 'iX'
        sleep 0.1
        printf '\033[H'
        sleep 0.1
        printf 'H'
        sleep 0.1
        printf '\033[F'
        sleep 0.1
        printf '!'
        sleep 0.1
        printf '\033'
        sleep 0.1
        printf ':wq\n'
    } | script -qefc "$binary $file" "$log" >/dev/null 2>&1
}

run_no_write_error_case() {
    binary=$1
    file=$2
    log=$3
    {
        sleep 1
        printf 'iX\033:q\n'
        sleep 0.3
        printf ':q!\n'
    } | script -qefc "$binary $file" "$log" >/dev/null 2>&1
}

run_status_case() {
    binary=$1
    file=$2
    command=$3
    log=$4
    {
        sleep 1
        printf ':%s\n' "$command"
        sleep 0.3
        printf ':q!\n'
    } | script -qefc "$binary $file" "$log" >/dev/null 2>&1
}

run_multi_status_case() {
    binary=$1
    first=$2
    second=$3
    command=$4
    log=$5
    {
        sleep 1
        printf ':%s\n' "$command"
        sleep 0.3
        printf ':q!\n'
    } | script -qefc "$binary $first $second" "$log" >/dev/null 2>&1
}

run_initial_frame_case() {
    binary=$1
    log=$2
    {
        sleep 1
        printf '\033:q!\n'
    } | script -qefc "cd '$tmp/frame'; stty rows 8 cols 40; exec $binary file" "$log" >/dev/null 2>&1
}

run_scroll_key_case() {
    binary=$1
    key=$2
    log=$3
    {
        sleep 1
        case "$key" in
            b) printf '\002' ;;
            f) printf '\006' ;;
            u) printf '\025' ;;
            d) printf '\004' ;;
        esac
        sleep 0.2
        printf '\033:q!\n'
    } | script -qefc "cd '$tmp/scroll'; stty rows 10 cols 40; exec $binary file" "$log" >/dev/null 2>&1
}

run_horizontal_case() {
    binary=$1
    log=$2
    {
        sleep 1
        printf 'llllllllll'
        sleep 0.2
        printf '\033:q!\n'
    } | script -qefc "cd '$tmp/horizontal'; stty rows 6 cols 10; exec $binary file" "$log" >/dev/null 2>&1
}

run_eof_case() {
    binary=$1
    log=$2
    {
        sleep 1
        i=0
        while test "$i" -lt 60; do
            printf '\006'
            i=$((i + 1))
        done
        sleep 0.2
        printf '\033:q!\n'
    } | script -qefc "cd '$tmp/eof'; stty rows 40 cols 80; exec $binary file" "$log" >/dev/null 2>&1
}

printf 'abc\ndef\n' >"$tmp/c-command"
cp "$tmp/c-command" "$tmp/rust-command"

set +e
"$root/vi-c" -H >"$tmp/c-help" 2>"$tmp/c-help.err"
c_help_status=$?
"$root/vi" -H >"$tmp/rust-help" 2>"$tmp/rust-help.err"
rust_help_status=$?
set -e
test "$c_help_status" -eq 1
test "$rust_help_status" -eq 1
cmp "$tmp/c-help" "$tmp/rust-help"
cmp "$tmp/c-help.err" "$tmp/rust-help.err"

run_command_mode_case "$root/vi-c" "$tmp/c-command" "$tmp/c-command.log"
run_command_mode_case "$root/vi" "$tmp/rust-command" "$tmp/rust-command.log"
cmp "$tmp/c-command" "$tmp/rust-command"
test "$(sed -n '1p' "$tmp/rust-command")" = "bc"

printf 'abc\n' >"$tmp/c-insert"
cp "$tmp/c-insert" "$tmp/rust-insert"
run_basic_insert_case "$root/vi-c" "$tmp/c-insert" "$tmp/c-insert.log"
run_basic_insert_case "$root/vi" "$tmp/rust-insert" "$tmp/rust-insert.log"
cmp "$tmp/c-insert" "$tmp/rust-insert"
test "$(cat "$tmp/rust-insert")" = "XYabc"

printf 'abc\n' >"$tmp/c-insert-home-end"
cp "$tmp/c-insert-home-end" "$tmp/rust-insert-home-end"
run_insert_home_end_case "$root/vi-c" "$tmp/c-insert-home-end" "$tmp/c-insert-home-end.log"
run_insert_home_end_case "$root/vi" "$tmp/rust-insert-home-end" "$tmp/rust-insert-home-end.log"
cmp "$tmp/c-insert-home-end" "$tmp/rust-insert-home-end"
test "$(cat "$tmp/rust-insert-home-end")" = "HXabc!"

printf 'abc\n' >"$tmp/c-no-write"
cp "$tmp/c-no-write" "$tmp/rust-no-write"
run_no_write_error_case "$root/vi-c" "$tmp/c-no-write" "$tmp/c-no-write.log"
run_no_write_error_case "$root/vi" "$tmp/rust-no-write" "$tmp/rust-no-write.log"
no_write_error=$(printf '\033[7mNo write since last change (:q! overrides)\033[m')
for log in "$tmp/c-no-write.log" "$tmp/rust-no-write.log"; do
	grep -a -F -q "$no_write_error" "$log"
done

printf 'one\ntwo\n' >"$tmp/c-set"
cp "$tmp/c-set" "$tmp/rust-set"
run_status_case "$root/vi-c" "$tmp/c-set" "set" "$tmp/c-set.log"
run_status_case "$root/vi" "$tmp/rust-set" "set" "$tmp/rust-set.log"
set_status=$(printf '\033[7mnoautoindent noexpandtab noflash noignorecase noshowmatch tabstop=8\033[m')
for log in "$tmp/c-set.log" "$tmp/rust-set.log"; do
	grep -a -F -q "$set_status" "$log"
done

printf 'one\ntwo\n' >"$tmp/c-substitute"
cp "$tmp/c-substitute" "$tmp/rust-substitute"
run_status_case "$root/vi-c" "$tmp/c-substitute" "s/missing/replacement/" "$tmp/c-substitute.log"
run_status_case "$root/vi" "$tmp/rust-substitute" "s/missing/replacement/" "$tmp/rust-substitute.log"
no_match_status=$(printf '\033[7mNo match\033[m')
for log in "$tmp/c-substitute.log" "$tmp/rust-substitute.log"; do
	grep -a -F -q "$no_match_status" "$log"
done

printf 'one\ntwo\n' >"$tmp/c-yank"
cp "$tmp/c-yank" "$tmp/rust-yank"
run_status_case "$root/vi-c" "$tmp/c-yank" "yank" "$tmp/c-yank.log"
run_status_case "$root/vi" "$tmp/rust-yank" "yank" "$tmp/rust-yank.log"
yank_status=$(printf 'Yank 1 lines (4 chars) into [D]')
for log in "$tmp/c-yank.log" "$tmp/rust-yank.log"; do
	grep -a -F -q "$yank_status" "$log"
done

printf 'one\n' >"$tmp/c-prev"
cp "$tmp/c-prev" "$tmp/rust-prev"
run_status_case "$root/vi-c" "$tmp/c-prev" "p" "$tmp/c-prev.log"
run_status_case "$root/vi" "$tmp/rust-prev" "p" "$tmp/rust-prev.log"
prev_status=$(printf '\033[7mNo previous files to edit\033[m')
for log in "$tmp/c-prev.log" "$tmp/rust-prev.log"; do
	grep -a -F -q "$prev_status" "$log"
done

printf 'one\n' >"$tmp/c-unknown-ex"
cp "$tmp/c-unknown-ex" "$tmp/rust-unknown-ex"
run_status_case "$root/vi-c" "$tmp/c-unknown-ex" "g" "$tmp/c-unknown-ex.log"
run_status_case "$root/vi" "$tmp/rust-unknown-ex" "g" "$tmp/rust-unknown-ex.log"
unknown_ex_status=$(printf "\033[7m'g' is not implemented\033[m")
for log in "$tmp/c-unknown-ex.log" "$tmp/rust-unknown-ex.log"; do
	grep -a -F -q "$unknown_ex_status" "$log"
done

printf 'one\n' >"$tmp/c-mark"
cp "$tmp/c-mark" "$tmp/rust-mark"
run_status_case "$root/vi-c" "$tmp/c-mark" "'z=" "$tmp/c-mark.log"
run_status_case "$root/vi" "$tmp/rust-mark" "'z=" "$tmp/rust-mark.log"
mark_status=$(printf '\033[7mMark not set\033[m')
for log in "$tmp/c-mark.log" "$tmp/rust-mark.log"; do
	grep -a -F -q "$mark_status" "$log"
done

printf 'one\n' >"$tmp/c-bad-address-search"
cp "$tmp/c-bad-address-search" "$tmp/rust-bad-address-search"
run_status_case "$root/vi-c" "$tmp/c-bad-address-search" "/[/" "$tmp/c-bad-address-search.log"
run_status_case "$root/vi" "$tmp/rust-bad-address-search" "/[/" "$tmp/rust-bad-address-search.log"
bad_address_search_status=$(printf '\033[7mbad search pattern '\''['\'': Invalid regular expression\033[m')
for log in "$tmp/c-bad-address-search.log" "$tmp/rust-bad-address-search.log"; do
	grep -a -F -q "$bad_address_search_status" "$log"
done

printf 'one\n' >"$tmp/c-undo"
cp "$tmp/c-undo" "$tmp/rust-undo"
for binary in "$root/vi-c" "$root/vi"; do
	case "$binary" in
		*vi-c) log="$tmp/c-undo.log"; file="$tmp/c-undo" ;;
		*) log="$tmp/rust-undo.log"; file="$tmp/rust-undo" ;;
	esac
	{
		sleep 1
		printf 'xu'
		sleep 0.3
		printf ':q!\n'
	} | script -qefc "$binary $file" "$log" >/dev/null 2>&1
done
undo_status=$(printf 'Undo [1] restored 1 chars at position 0')
for log in "$tmp/c-undo.log" "$tmp/rust-undo.log"; do
	grep -a -F -q "$undo_status" "$log"
done

printf 'one\n' >"$tmp/c-multi-one"
printf 'two\n' >"$tmp/c-multi-two"
cp "$tmp/c-multi-one" "$tmp/rust-multi-one"
cp "$tmp/c-multi-two" "$tmp/rust-multi-two"
run_multi_status_case "$root/vi-c" "$tmp/c-multi-one" "$tmp/c-multi-two" "q" "$tmp/c-multi-q.log"
run_multi_status_case "$root/vi" "$tmp/rust-multi-one" "$tmp/rust-multi-two" "q" "$tmp/rust-multi-q.log"
more_files_status=$(printf '\033[7m1 more file(s) to edit\033[m')
for log in "$tmp/c-multi-q.log" "$tmp/rust-multi-q.log"; do
	grep -a -F -q "$more_files_status" "$log"
done

mkdir "$tmp/frame"
printf 'one\ntwo\n' >"$tmp/frame/file"
run_initial_frame_case "$root/vi-c" "$tmp/c-frame.log"
run_initial_frame_case "$root/vi" "$tmp/rust-frame.log"
for log in "$tmp/c-frame.log" "$tmp/rust-frame.log"; do
	grep -a -F -q '~' "$log"
	grep -a -F -q -- '- file 1/2 50%' "$log"
done

mkdir "$tmp/scroll"
i=1
while test "$i" -le 100; do
	printf 'line%03d\n' "$i" >>"$tmp/scroll/file"
	i=$((i + 1))
done
for key in b f u d; do
	run_scroll_key_case "$root/vi-c" "$key" "$tmp/c-scroll-$key.log"
	run_scroll_key_case "$root/vi" "$key" "$tmp/rust-scroll-$key.log"
done
grep -a -F -q -- '- file 1/100 1%' "$tmp/c-scroll-b.log"
grep -a -F -q -- '- file 9/100 9%' "$tmp/c-scroll-f.log"
grep -a -F -q -- '- file 1/100 1%' "$tmp/c-scroll-u.log"
grep -a -F -q -- '- file 5/100 5%' "$tmp/c-scroll-d.log"
grep -a -F -q -- '- file 1/100 1%' "$tmp/rust-scroll-b.log"
grep -a -F -q -- '- file 9/100 9%' "$tmp/rust-scroll-f.log"
grep -a -F -q -- '- file 1/100 1%' "$tmp/rust-scroll-u.log"
grep -a -F -q -- '- file 5/100 5%' "$tmp/rust-scroll-d.log"

mkdir "$tmp/horizontal"
printf '0123456789abcdefghij\n' >"$tmp/horizontal/file"
run_horizontal_case "$root/vi-c" "$tmp/c-horizontal.log"
run_horizontal_case "$root/vi" "$tmp/rust-horizontal.log"
horizontal_frame=$(printf '\033[1;10H')
for log in "$tmp/c-horizontal.log" "$tmp/rust-horizontal.log"; do
	grep -a -F -q '123456789a' "$log"
	grep -a -F -q "$horizontal_frame" "$log"
done

mkdir "$tmp/eof"
cp "$root/LICENSE" "$tmp/eof/file"
run_eof_case "$root/vi-c" "$tmp/c-eof.log"
run_eof_case "$root/vi" "$tmp/rust-eof.log"
eof_cursor=$(printf '\033[1;39H')
for log in "$tmp/c-eof.log" "$tmp/rust-eof.log"; do
	grep -a -F -q 'Public License instead of this License.' "$log"
	grep -a -F -q '~' "$log"
	grep -a -F -q -- '- file 348/348 100%' "$log"
	grep -a -F -q "$eof_cursor" "$log"
done

echo "reference functional tests: ok"
