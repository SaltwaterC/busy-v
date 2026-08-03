use busy_v::{Editor, HighlightColor, HighlightSpan, HighlightStyle};
use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

#[test]
fn embedded_buffer_round_trips_newlines() {
    let editor = Editor::from_bytes(b"one\ntwo\n", None, false);
    assert_eq!(editor.bytes(), b"one\ntwo\n");
    let no_final_newline = Editor::from_bytes(b"one\ntwo", None, false);
    assert_eq!(no_final_newline.bytes(), b"one\ntwo");
}

#[test]
fn embedding_can_supply_and_reuse_syntax_highlighting() {
    let calls = Rc::new(Cell::new(0));
    let observed = Rc::new(std::cell::RefCell::new(Vec::new()));
    let call_count = Rc::clone(&calls);
    let seen_buffers = Rc::clone(&observed);
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);

    editor.set_syntax_highlighter(Box::new(move |buffer: &[u8]| {
        call_count.set(call_count.get() + 1);
        seen_buffers.borrow_mut().push(buffer.to_vec());
        let first_line_end = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(buffer.len());
        vec![HighlightSpan::new(
            0,
            first_line_end,
            HighlightStyle::foreground(HighlightColor::Ansi(42)),
        )]
    }));

    assert_eq!(editor.syntax_highlights().unwrap()[0].start, 0);
    assert_eq!(calls.get(), 1);
    assert_eq!(editor.syntax_highlights().unwrap()[0].end, 3);
    assert_eq!(calls.get(), 1);

    editor
        .execute_keys(b"iX\x1b")
        .expect("modify highlighted buffer");
    assert_eq!(editor.syntax_highlights().unwrap()[0].end, 4);
    assert_eq!(calls.get(), 2);
    assert_eq!(
        observed.borrow().as_slice(),
        &[b"one\ntwo\n".to_vec(), b"Xone\ntwo\n".to_vec()]
    );

    editor.invalidate_syntax_highlighting();
    let _ = editor.syntax_highlights();
    assert_eq!(calls.get(), 3);
    editor.clear_syntax_highlighter();
    assert!(editor.syntax_highlights().is_none());
}

#[test]
fn ex_substitute_and_delete_are_owned_operations() {
    let mut editor = Editor::from_bytes(b"foo\nbar\nfoo\n", None, false);
    editor.execute_ex(":s/foo/baz/");
    assert_eq!(editor.bytes(), b"baz\nbar\nfoo\n");
    editor.execute_ex("%s/bar/BAR/");
    assert_eq!(editor.bytes(), b"baz\nBAR\nfoo\n");
    editor.execute_ex("1d");
    assert_eq!(editor.bytes(), b"BAR\nfoo\n");
    editor.execute_ex(".,$d");
    assert_eq!(editor.bytes(), b"\n");

    let mut addressed = Editor::from_bytes(b"one\ntwo\nthree\n", None, false);
    addressed.execute_ex("/two/d");
    assert_eq!(addressed.bytes(), b"one\nthree\n");
}

#[test]
fn delta_undo_redo_covers_byte_and_structural_edits() {
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);

    editor.execute_keys(b"oadded\x1b").expect("open line");
    assert_eq!(editor.bytes(), b"one\nadded\ntwo\n");
    editor.execute_keys(b"u").expect("undo open line");
    assert_eq!(editor.bytes(), b"one\ntwo\n");
    editor.execute_keys(b"\x12").expect("redo open line");
    assert_eq!(editor.bytes(), b"one\nadded\ntwo\n");

    editor.execute_ex("%s/o/O/g");
    assert_eq!(editor.bytes(), b"One\nadded\ntwO\n");
    editor.execute_keys(b"u").expect("undo substitution");
    assert_eq!(editor.bytes(), b"one\nadded\ntwo\n");

    editor.execute_ex("2d");
    assert_eq!(editor.bytes(), b"one\ntwo\n");
    editor.execute_keys(b"u").expect("undo line deletion");
    assert_eq!(editor.bytes(), b"one\nadded\ntwo\n");
}

#[test]
fn reference_pattern_cases_work_through_editor_commands() {
    let mut editor = Editor::from_bytes(b"alpha beta\ncat cot cut\nfoo food\n", None, false);
    editor.execute_ex("1s/^a.*a$/<matched>/");
    editor.execute_ex("2s/c[ao]t/<&>/g");
    editor.execute_ex("3s/\\(fo\\)o/\\1X/g");
    assert_eq!(editor.bytes(), b"<matched>\n<cat> <cot> cut\nfoX foXd\n");

    let mut ignorecase = Editor::from_bytes(b"Foo\n", None, false);
    ignorecase.execute_ex("set ignorecase");
    ignorecase.execute_ex("s/foo/bar/");
    assert_eq!(ignorecase.bytes(), b"bar\n");
}

#[test]
fn insert_newline_preserves_indentation_when_enabled() {
    let mut editor = Editor::from_bytes(b"  one\n", None, false);
    editor.execute_ex("set autoindent");
    editor.execute_keys(b"A\n").expect("insert newline");
    assert_eq!(editor.bytes(), b"  one\n  \n");
}

#[test]
fn expandtab_and_percent_filename_expansion_are_safe() {
    let path = std::env::temp_dir().join(format!("busy-v-expand-{}", std::process::id()));
    let backup = PathBuf::from(format!("{}.bak", path.display()));
    let mut editor = Editor::from_bytes(b"x", Some(path.clone()), false);
    editor.execute_ex("set expandtab");
    editor.execute_keys(b"A\t").expect("expand tab");
    assert_eq!(editor.bytes(), b"x       ");
    editor.execute_ex("w %.bak");
    assert_eq!(fs::read(&backup).expect("backup output"), b"x       ");
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(backup);
}

#[test]
fn ex_write_uses_the_requested_path() {
    let path = std::env::temp_dir().join(format!("busy-v-test-{}", std::process::id()));
    let mut editor = Editor::from_bytes(b"safe\n", Some(path.clone()), false);
    editor.execute_ex("w");
    assert_eq!(fs::read(&path).expect("test output"), b"safe\n");
    assert_eq!(editor.filename(), Some(path.as_path()));
    assert!(!editor.is_modified());
    assert_eq!(editor.status(), format!("'{}' 1L, 5C", path.display()));
    let _ = fs::remove_file(path);
}

#[test]
fn modified_file_quit_error_matches_reference_wording() {
    let mut editor = Editor::from_bytes(b"abc\n", None, false);
    editor.execute_keys(b"iX\x1b").expect("modify buffer");

    editor.execute_ex(":q");

    assert_eq!(
        editor.status(),
        "No write since last change (:q! overrides)"
    );
    assert!(!editor.should_quit());
}

#[test]
fn common_application_messages_match_the_c_reference() {
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);
    editor.execute_ex("set all");
    assert_eq!(
        editor.status(),
        "noautoindent noexpandtab noflash noignorecase noshowmatch tabstop=8"
    );

    editor.execute_ex("set tabstop=0");
    assert_eq!(editor.status(), "bad option: tabstop=0");

    editor.execute_ex("s/missing/replacement/");
    assert_eq!(editor.status(), "No match");
    editor.execute_ex("s/one/ONE/");
    assert_eq!(editor.status(), "");

    let mut search = Editor::from_bytes(b"one\ntwo\n", None, false);
    search.execute_keys(b"n").expect("search without a pattern");
    assert_eq!(search.status(), "No previous search");

    search.execute_keys(b"/one\n").expect("wrapped search");
    assert_eq!(search.status(), "search hit BOTTOM, continuing at TOP");

    editor.execute_ex("y");
    assert_eq!(editor.status(), "Yank 1 lines (4 chars) into [D]");

    editor.execute_keys(b"x").expect("delete for undo");
    editor.execute_keys(b"u").expect("undo");
    assert_eq!(editor.status(), "Undo [2] restored 1 chars at position 0");
}

#[test]
fn ex_addresses_and_line_yank_messages_match_the_reference() {
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);
    editor.execute_keys(b"ma").expect("set mark");
    editor.execute_ex("'a=");
    assert_eq!(editor.status(), "1");

    editor.execute_ex("'z=");
    assert_eq!(editor.status(), "Mark not set");

    editor.execute_ex("1+1=");
    assert_eq!(editor.status(), "2");
    editor.execute_ex("0=");
    assert_eq!(editor.status(), "0");

    editor.execute_ex("1,2list");
    assert_eq!(editor.status(), "one$");

    editor.execute_keys(b"Y").expect("yank line");
    assert_eq!(editor.status(), "Yank 1 lines (4 chars) from [D]");

    editor.execute_ex("/two/");
    editor.execute_ex("s//TWO/");
    assert_eq!(editor.bytes(), b"one\nTWO\n");

    editor.execute_ex(":/[/");
    assert_eq!(
        editor.status(),
        "bad search pattern '[': Invalid regular expression"
    );
}

#[test]
fn arrow_keys_move_without_inserting_escape_bytes() {
    let mut editor = Editor::from_bytes(b"abc\ndef\n", None, false);
    editor
        .execute_keys(b"\x1b[C\x1b[B")
        .expect("arrow movement");
    assert_eq!(editor.cursor(), (1, 1));
    editor.execute_keys(b"x").expect("delete after movement");
    assert_eq!(editor.bytes(), b"abc\ndf\n");
}

#[test]
fn page_keys_scroll_by_a_full_viewport_without_inserting_escape_bytes() {
    let data = (1..=100)
        .map(|line| format!("line{line:03}\n"))
        .collect::<String>();
    let mut editor = Editor::from_bytes(data.as_bytes(), None, false);

    editor.execute_keys(b"\x1b[6~").expect("page down movement");
    assert_eq!(editor.cursor(), (22, 0));

    editor
        .execute_keys(b"i\x1b[5~X\x1b")
        .expect("page up and insert");
    assert_eq!(editor.cursor(), (22, 0));
    assert!(editor
        .bytes()
        .windows(b"Xline023".len())
        .any(|window| window == b"Xline023"));
}

#[test]
fn set_number_accepts_long_and_short_forms() {
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);

    editor.execute_ex("set number");
    editor.execute_ex("set nonu");
    editor.execute_ex("set nu");

    assert_eq!(editor.status(), "");
}

#[test]
fn home_and_end_move_the_insert_cursor_without_inserting_escape_bytes() {
    let mut editor = Editor::from_bytes(b"abc\n", None, false);
    editor
        .execute_keys(b"iX\x1b[HH\x1b[F!\x1b")
        .expect("home and end movement");
    assert_eq!(editor.bytes(), b"HXabc!\n");

    let mut tilde_sequences = Editor::from_bytes(b"abc\n", None, false);
    tilde_sequences
        .execute_keys(b"iX\x1b[1~H\x1b[4~!\x1b")
        .expect("tilde home and end movement");
    assert_eq!(tilde_sequences.bytes(), b"HXabc!\n");
}

#[test]
fn insert_mode_accepts_utf8_bytes_and_navigation_alone_is_not_a_change() {
    let mut editor = Editor::from_bytes(b"\n", None, false);
    editor
        .execute_keys("i¬\x1b".as_bytes())
        .expect("insert non-ASCII character");
    assert_eq!(editor.bytes(), "¬\n".as_bytes());

    let mut wide_bytes = Editor::from_bytes(b"\n", None, false);
    wide_bytes
        .execute_keys("iĀ\x1b".as_bytes())
        .expect("insert UTF-8 bytes overlapping cursor sentinels");
    assert_eq!(wide_bytes.bytes(), "Ā\n".as_bytes());

    let mut unchanged = Editor::from_bytes(b"abc\n", None, false);
    unchanged
        .execute_keys(b"i\x1b[H\x1b[F\x1b")
        .expect("navigate in insert mode");
    assert!(!unchanged.is_modified());
    unchanged.execute_ex(":q");
    assert!(unchanged.should_quit());
}
