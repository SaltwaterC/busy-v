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
fn set_queries_boolean_options_without_changing_them() {
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);
    for (name, alias) in [
        ("number", "nu"),
        ("wrap", "wrap"),
        ("linebreak", "lbr"),
        ("autoindent", "ai"),
        ("expandtab", "et"),
        ("flash", "fl"),
        ("ignorecase", "ic"),
        ("showmatch", "sm"),
        ("readonly", "ro"),
        ("modeline", "ml"),
    ] {
        editor.execute_ex(&format!("set no{name}"));
        for query in [name, alias] {
            editor.execute_ex(&format!(":set {query}?"));
            assert_eq!(editor.status(), format!("no{name}"));
        }
        // A query must leave the option disabled, so toggling enables it.
        editor.execute_ex(&format!("set {alias}!"));
        for query in [name, alias] {
            editor.execute_ex(&format!("set {query}?"));
            assert_eq!(editor.status(), name);
        }
        editor.execute_ex(&format!("set {alias}! {name}?"));
        assert_eq!(editor.status(), format!("no{name}"));
    }
    assert_eq!(editor.bytes(), b"one\ntwo\n");
    assert!(!editor.is_modified());
}

#[test]
fn set_queries_numeric_options_and_reports_values_in_command_order() {
    let mut editor = Editor::from_bytes(b"one\n", None, false);
    editor.execute_ex("set tabstop? ts? textwidth? tw?");
    assert_eq!(
        editor.status(),
        "tabstop=8 tabstop=8 textwidth=0 textwidth=0"
    );

    editor
        .execute_ex("set nu? nu nu? ts=4 ts? tw=72 tw? tabstop=2 tabstop? textwidth=0 textwidth?");
    assert_eq!(
        editor.status(),
        "nonumber number tabstop=4 textwidth=72 tabstop=2 textwidth=0"
    );
    editor.execute_ex("set ts? tw?");
    assert_eq!(editor.status(), "tabstop=2 textwidth=0");
}

#[test]
fn set_query_errors_are_not_hidden_by_successful_queries() {
    let mut editor = Editor::from_bytes(b"one\n", None, false);
    for option in ["missing?", "nu??", "nonu?", "nu!?", "tw=3?"] {
        editor.execute_ex(&format!("set nu? {option} tw?"));
        assert_eq!(editor.status(), format!("bad option: {option}"));
    }
    editor.execute_ex("set tabstop=0 nu?");
    assert_eq!(editor.status(), "bad option: tabstop=0");
}

#[test]
fn set_query_works_through_keyboard_input() {
    let mut editor = Editor::from_bytes(b"one\n", None, false);
    editor.execute_keys(b":set nu?\n").expect("query number");
    assert_eq!(editor.status(), "nonumber");
    editor
        .execute_keys(b":set nu\n:set nu?\n")
        .expect("enable and query number");
    assert_eq!(editor.status(), "number");
}

#[test]
fn modelines_apply_supported_settings_without_editing_the_buffer() {
    for data in [
        b"# vim: set ts=4 tw=80 et nu:\ntext\n".as_slice(),
        b"/* Vim: set ts=4 tw=80 et nu: */\ntext\n",
        b"\tvi:ts=4:tw=80:et:nu\r\ntext\r\n",
        b"# ex: se ts=4 tw=80 et nu: trailing comment\ntext\n",
        b"vim: ts=4 tw=80 et nu\ntext\n",
    ] {
        let mut editor = Editor::from_bytes(data, None, false);
        assert_eq!(editor.status(), "");
        editor.execute_ex("set ts? tw? et? nu?");
        assert_eq!(editor.status(), "tabstop=4 textwidth=80 expandtab number");
        assert_eq!(editor.bytes(), data);
        assert_eq!(editor.cursor(), (0, 0));
        assert!(!editor.is_modified());
    }
}

#[test]
fn modelines_scan_only_the_head_and_tail_once_in_file_order() {
    let data =
        b"vim: ts=2\n2\n3\n4\n5\nvim: ts=3\n7\n8\n9\n10\nvim: tw=72\n12\n13\n14\nvim: ts=4\n";
    let mut editor = Editor::from_bytes(data, None, false);
    editor.execute_ex("set ts? tw?");
    assert_eq!(editor.status(), "tabstop=4 textwidth=72");

    let mut editor = Editor::from_bytes(b"vim: nu!\n", None, false);
    editor.execute_ex("set nu?");
    assert_eq!(editor.status(), "number");

    let mut editor =
        Editor::from_bytes(b"1\n2\n3\n4\n5\nvim: tw=12\n7\n8\n9\n10\n11\n", None, false);
    editor.execute_ex("set tw?");
    assert_eq!(editor.status(), "textwidth=0");
}

#[test]
fn ordinary_text_and_commands_are_not_executed_as_modelines() {
    for data in [
        b"prefixvim: set tw=12:\n".as_slice(),
        b"ex: set tw=12:\n",
        b"Vim: set tw=12:\n",
        b"# Vim: tw=12\n",
    ] {
        let mut editor = Editor::from_bytes(data, None, false);
        assert_eq!(editor.status(), "");
        editor.execute_ex("set tw?");
        assert_eq!(editor.status(), "textwidth=0");
    }
    for data in [
        b"# vim: set ts=0 tw=12:\n".as_slice(),
        b"# vim: set tw=12\n",
        b"# vim: set q! tw=12:\n",
        b"# vim: set ts=4 | q! tw=12:\n",
    ] {
        let mut editor = Editor::from_bytes(data, None, false);
        assert!(editor.status().starts_with("modeline line 1: "));
        assert!(!editor.should_quit());
        assert_eq!(editor.bytes(), data);
        editor.execute_ex("set tw?");
        assert_eq!(editor.status(), "textwidth=0");
    }
}

#[test]
fn modeline_controls_apply_to_subsequent_modelines_and_file_reloads() {
    let mut editor = Editor::from_bytes(b"vim: noml tw=12\nvim: tw=80\n", None, false);
    editor.execute_ex("set ml? tw?");
    assert_eq!(editor.status(), "nomodeline textwidth=12");

    let path = std::env::temp_dir().join(format!("busy-v-modelines-{}", std::process::id()));
    fs::write(&path, b"first\nvim: tw=12\nlast\n").expect("write modeline file");
    let mut editor = Editor::new(Some(path.clone()), false).expect("open modeline file");
    editor.execute_ex("set tw?");
    assert_eq!(editor.status(), "textwidth=12");
    editor.execute_ex("set noml tw=0");
    editor.execute_ex("edit");
    editor.execute_ex("set tw?");
    assert_eq!(editor.status(), "textwidth=0");
    editor.execute_ex("set ml mls=1");
    editor.execute_ex("edit");
    editor.execute_ex("set tw? modelines?");
    assert_eq!(editor.status(), "textwidth=0 modelines=1");
    editor.execute_ex("set modelines=0");
    editor.execute_ex("edit");
    editor.execute_ex("set tw?");
    assert_eq!(editor.status(), "textwidth=0");
    editor.execute_ex("set mls=5");
    editor.execute_ex("edit");
    editor.execute_ex("set tw?");
    assert_eq!(editor.status(), "textwidth=12");

    fs::write(&path, b"vim: set missing:\n").expect("write invalid modeline");
    editor.execute_ex("edit");
    assert_eq!(editor.status(), "modeline line 1: bad option: missing");
    fs::remove_file(path).expect("remove modeline file");
}

#[test]
fn textwidth_breaks_the_line_at_the_last_blank_before_the_margin() {
    let mut editor = Editor::from_bytes(b"", None, false);
    editor.execute_ex("set tw=10");
    editor
        .execute_keys(b"ione two three four\x1b")
        .expect("type past the margin");
    assert_eq!(editor.bytes(), b"one two\nthree four");
    assert_eq!(editor.cursor(), (1, 9));

    let mut editor = Editor::from_bytes(b"", None, false);
    editor.execute_ex("set textwidth=8 ai");
    editor
        .execute_keys(b"i  aaaaaaaaaa bb cc\x1b")
        .expect("type a word longer than the margin");
    assert_eq!(editor.bytes(), b"  aaaaaaaaaa\n  bb cc");

    let mut editor = Editor::from_bytes(b"", None, false);
    editor.execute_ex("set tw=0");
    editor
        .execute_keys(b"ione two three four\x1b")
        .expect("type without a margin");
    assert_eq!(editor.bytes(), b"one two three four");
    assert_eq!(editor.status(), "");
}

#[test]
fn textwidth_wrapping_undoes_with_the_insert() {
    let mut editor = Editor::from_bytes(b"", None, false);
    editor.execute_ex("set tw=5");
    editor
        .execute_keys(b"iab cd ef\x1b")
        .expect("type past the margin");
    assert_eq!(editor.bytes(), b"ab cd\nef");

    editor.execute_keys(b"u").expect("undo the insert");
    assert_eq!(editor.bytes(), b"");
}

#[test]
fn set_wrap_toggles_without_errors() {
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);

    editor.execute_ex("set wrap");
    editor.execute_ex("set nowrap");
    editor.execute_ex("set wrap nu");
    editor.execute_ex("set wrap! linebreak nolbr lbr tw=80 textwidth=72");

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
