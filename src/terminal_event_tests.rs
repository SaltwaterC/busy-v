use super::*;

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn keep_syntax_highlighting_deferred(editor: &mut Editor) {
    editor.syntax_highlight_ready_at = Some(Instant::now() + Duration::from_secs(60));
}

#[test]
fn mouse_events_are_queued_without_changing_keyboard_events() {
    let mut reader = KeyReader::from_bytes(&[]);
    let event = Event::Mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 1));
    assert_eq!(reader.push_event(event), None);
    assert_eq!(
        reader.take_mouse(),
        Some(mouse(MouseEventKind::Down(MouseButton::Left), 2, 1))
    );
    assert_eq!(
        reader.push_event(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ))),
        Some(b'x')
    );
}

#[test]
fn buffered_key_reader_treats_a_trailing_escape_as_a_key() {
    let mut reader = KeyReader::from_bytes(b"\x1b");
    assert_eq!(reader.key().expect("read buffered escape"), 0x1b);
    assert!(!reader.has_pending());
}

#[test]
fn numbered_mouse_selection_copies_only_source_text() {
    let mut editor = Editor::from_bytes(b"  one\n\ttwo\n\nthree", None, false);
    editor.screen_rows = 5;
    editor.screen_cols = 20;
    editor.execute_ex("set nu");

    assert_eq!(
        editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0)),
        None
    );
    assert_eq!(
        editor.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), 19, 2)),
        None
    );
    let copied = editor
        .handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), 19, 2))
        .expect("release copies a selection");
    assert_eq!(copied, b"  one\n\ttwo\n");
    assert!(!copied
        .windows(b"  1 ".len())
        .any(|window| window == b"  1 "));
    assert!(!copied
        .windows(b"  2 ".len())
        .any(|window| window == b"  2 "));
}

#[test]
fn selection_overlay_starts_after_the_gutter_and_keeps_syntax_styles() {
    let mut editor = Editor::from_bytes(b"one\ntwo", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 20;
    editor.execute_ex("set nu");
    editor.set_syntax_highlighter(Box::new(|_: &[u8]| {
        vec![HighlightSpan::new(
            0,
            3,
            HighlightStyle::foreground(HighlightColor::Ansi(42)),
        )]
    }));
    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), 6, 0));

    let mut rendered = Vec::new();
    editor
        .render_to(&mut rendered, None)
        .expect("render selected syntax line");
    assert!(rendered
        .windows(b"  1 \x1b[0m".len())
        .any(|window| window == b"  1 \x1b[0m"));
    assert!(rendered
        .windows(b"\x1b[38;5;42m\x1b[7mone".len())
        .any(|window| window == b"\x1b[38;5;42m\x1b[7mone"));
    assert!(!rendered
        .windows(b"\x1b[7m  1 ".len())
        .any(|window| window == b"\x1b[7m  1 "));
}

#[test]
fn double_and_triple_clicks_select_words_and_lines() {
    let mut editor = Editor::from_bytes(b"alpha beta\ngamma", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 20;

    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 7, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), 7, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 7, 0));
    let word = editor
        .handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), 7, 0))
        .expect("double click selects a word");
    assert_eq!(word, b"beta");

    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 1, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), 1, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 1, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), 1, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 1, 0));
    let line = editor
        .handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), 1, 0))
        .expect("triple click selects a line");
    assert_eq!(line, b"alpha beta");
}

#[test]
fn status_row_mouse_clicks_are_ignored() {
    let mut editor = Editor::from_bytes(b"one", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 20;
    assert_eq!(
        editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 3)),
        None
    );
    assert!(editor.selection.is_none());
}

#[test]
fn mouse_hit_testing_handles_tabs_controls_and_horizontal_scroll() {
    assert_eq!(byte_offset_at_display_column(b"\tab", 0, 8), 0);
    assert_eq!(byte_offset_at_display_column(b"\tab", 7, 8), 0);
    assert_eq!(byte_offset_at_display_column(b"\tab", 8, 8), 1);
    assert_eq!(byte_offset_at_display_column(b"a\x01b", 1, 8), 1);
    assert_eq!(byte_offset_at_display_column(b"a\x01b", 2, 8), 1);

    let mut editor = Editor::from_bytes(b"\t  a\x01bc", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 16;
    editor.screen_left = 10;
    assert_eq!(
        editor.mouse_point(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0)),
        Some(SelectionPoint { line: 0, offset: 3 })
    );
    assert_eq!(
        editor.mouse_point(mouse(MouseEventKind::Down(MouseButton::Left), 2, 0)),
        Some(SelectionPoint { line: 0, offset: 4 })
    );
}

#[test]
fn reverse_and_right_edge_selections_have_no_terminal_padding() {
    let mut editor = Editor::from_bytes(b"abcdef\nsecond", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 8;

    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 6, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), 1, 0));
    let reverse = editor
        .handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), 1, 0))
        .expect("reverse selection copies");
    assert_eq!(reverse, b"bcdef");

    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), 7, 0));
    let edge = editor
        .handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), 7, 0))
        .expect("right edge selection copies");
    assert_eq!(edge, b"abcdef");
}

#[test]
fn selection_scrolls_without_moving_the_vi_cursor_or_dropping_lines() {
    let mut data = Vec::new();
    for line in 0..12 {
        if line > 0 {
            data.push(b'\n');
        }
        data.extend_from_slice(format!("line{line}").as_bytes());
    }
    let mut editor = Editor::from_bytes(&data, None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 20;
    assert_eq!(editor.cursor(), (0, 0));

    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
    for _ in 0..5 {
        editor.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), 19, 2));
    }
    assert_eq!(editor.cursor(), (0, 0));
    assert_eq!(editor.screen_top, 5);
    let copied = editor
        .handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), 19, 2))
        .expect("edge drag copies");
    assert_eq!(
        copied,
        b"line0\nline1\nline2\nline3\nline4\nline5\nline6\nline7"
    );

    editor.selection = None;
    editor.mouse_viewport_scrolled = false;
    editor.handle_mouse_event(mouse(MouseEventKind::ScrollDown, 3, 0));
    assert_eq!(editor.cursor(), (0, 0));
    assert_eq!(editor.screen_top, 8);
}

#[test]
fn a_missed_mouse_release_can_be_recovered_by_the_next_key() {
    let mut editor = Editor::from_bytes(b"one\ntwo", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 20;
    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), 2, 0));
    let copied = editor
        .finish_mouse_selection()
        .expect("the pending drag is finalized");
    assert_eq!(copied, b"one");
    assert!(editor.finish_mouse_selection().is_none());

    editor.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
    editor.handle_mouse_event(mouse(MouseEventKind::Drag(MouseButton::Left), 2, 0));
    let copied = editor
        .handle_mouse_event(mouse(MouseEventKind::Moved, 2, 0))
        .expect("buttonless motion recovers a missed release");
    assert_eq!(copied, b"one");
}

#[test]
fn osc52_clipboard_encoding_targets_clipboard_and_primary() {
    let mut encoded = Vec::new();
    write_osc52_clipboard(&mut encoded, b"one\ntwo").expect("encode OSC 52");
    #[cfg(unix)]
    let expected = b"\x1b]52;c;b25lCnR3bw==\x1b\\\x1b]52;p;b25lCnR3bw==\x1b\\";
    #[cfg(not(unix))]
    let expected = b"\x1b]52;c;b25lCnR3bw==\x1b\\";
    assert_eq!(encoded, expected);
}

#[test]
fn platform_clipboard_helpers_use_argument_vectors() {
    let clipboard = clipboard_helper_commands(ClipboardDestination::Clipboard);
    #[cfg(unix)]
    let primary = clipboard_helper_commands(ClipboardDestination::Primary);
    #[cfg(unix)]
    {
        assert_eq!(clipboard[0].program, "wl-copy");
        assert_eq!(clipboard[0].args, &[] as &[&str]);
        assert_eq!(primary[0].program, "wl-copy");
        assert_eq!(primary[0].args, &["--primary"]);
        assert!(clipboard.iter().all(|command| command.program != "sh"));
        assert!(primary.iter().all(|command| command.program != "sh"));
    }
    #[cfg(windows)]
    assert_eq!(
        clipboard,
        vec![ClipboardHelperCommand {
            program: "clip.exe",
            args: &[]
        }]
    );
}

#[test]
fn crossterm_events_preserve_editor_key_contract() {
    let mut reader = KeyReader::from_bytes(&[]);
    assert_eq!(
        reader.push_event_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
        Some(0x82)
    );
    assert!(reader.take_special());
    assert_eq!(
        reader.push_event_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Some(3)
    );
    assert_eq!(
        reader.push_event_key(KeyEvent::new_with_kind(
            KeyCode::Char('!'),
            KeyModifiers::NONE,
            KeyEventKind::Release
        )),
        None
    );
    assert_eq!(
        reader.push_event_key(KeyEvent::new(KeyCode::Char('¬'), KeyModifiers::NONE)),
        Some(0xc2)
    );
    assert_eq!(reader.pending.pop_front(), Some(0xac));

    assert_eq!(
        reader.push_event_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::ALT)),
        Some(0x1b)
    );
    assert_eq!(reader.pending.pop_front(), Some(b':'));
    assert_eq!(decode_escape_sequence(b"C", b'C'), 0x82);
    assert_eq!(decode_escape_sequence(b"6~", b'~'), 0x89);
}

#[test]
fn syntax_spans_style_only_their_visible_byte_ranges() {
    let spans = [HighlightSpan::new(
        0,
        3,
        HighlightStyle::foreground(HighlightColor::Ansi(42)),
    )];
    let mut rendered = Vec::new();
    let mut highlights = HighlightCursor::new(&spans);
    write_highlighted_line(&mut rendered, b"let x", 0, &mut highlights, 0, 80, 8)
        .expect("render highlighted line");
    assert_eq!(rendered, b"\x1b[38;5;42mlet\x1b[0m x");

    rendered.clear();
    highlights = HighlightCursor::new(&spans);
    write_highlighted_line(&mut rendered, b"let x", 0, &mut highlights, 2, 2, 8)
        .expect("render clipped highlighted line");
    assert_eq!(rendered, b"\x1b[38;5;42mt\x1b[0m ");
}

#[cfg(windows)]
#[test]
fn windows_console_rows_replace_a_clipped_unicode_sequence() {
    let mut rendered = Vec::new();
    write_rendered_row(&mut rendered, b"\x1b[1;1H\xe2\x9d").expect("render a clipped unicode row");

    assert_eq!(rendered, b"\x1b[1;1H\xef\xbf\xbd");
}

#[test]
fn syntax_highlighter_styles_number_gutter_like_vim() {
    struct Highlighter;

    impl SyntaxHighlighter for Highlighter {
        fn highlight(&mut self, _: &[u8]) -> Vec<HighlightSpan> {
            Vec::new()
        }
    }

    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 20;
    editor.execute_ex("set nu");
    editor.set_syntax_highlighter(Box::new(Highlighter));

    let mut rendered = Vec::new();
    editor
        .render_to(&mut rendered, None)
        .expect("render numbered syntax editor");

    assert!(rendered
        .windows(b"\x1b[1;38;5;6m  1 \x1b[0mone".len())
        .any(|row| row == b"\x1b[1;38;5;6m  1 \x1b[0mone"));
    assert!(rendered
        .windows(b"\x1b[38;5;6m  2 \x1b[0mtwo".len())
        .any(|row| row == b"\x1b[38;5;6m  2 \x1b[0mtwo"));
}

#[test]
fn number_gutter_stays_plain_without_syntax_highlighting() {
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 20;
    editor.execute_ex("set nu");

    let mut rendered = Vec::new();
    editor
        .render_to(&mut rendered, None)
        .expect("render plain numbered editor");

    assert!(rendered
        .windows(b"  1 one".len())
        .any(|row| row == b"  1 one"));
    assert!(!rendered
        .windows(b"\x1b[38;5;6m".len())
        .any(|style| style == b"\x1b[38;5;6m"));
}

#[test]
fn syntax_span_cursor_advances_across_lines() {
    let style = HighlightStyle::foreground(HighlightColor::Ansi(42));
    let spans = [
        HighlightSpan::new(0, 3, style),
        HighlightSpan::new(4, 7, style),
        HighlightSpan::new(8, 11, style),
    ];
    let mut rendered = Vec::new();
    let mut highlights = HighlightCursor::new(&spans);
    write_highlighted_line(&mut rendered, b"two", 8, &mut highlights, 0, 80, 8)
        .expect("render line after preceding spans");

    assert_eq!(highlights.index, 2);
    assert_eq!(rendered, b"\x1b[38;5;42mtwo\x1b[0m");
}

#[test]
fn syntax_highlighting_coalesces_edits_and_polls_async_results() {
    use std::cell::Cell;
    use std::rc::Rc;

    struct AsyncHighlighter {
        calls: Rc<Cell<usize>>,
        ready: Rc<Cell<bool>>,
        pending_length: Option<usize>,
    }

    impl SyntaxHighlighter for AsyncHighlighter {
        fn highlight(&mut self, buffer: &[u8]) -> Vec<HighlightSpan> {
            self.calls.set(self.calls.get() + 1);
            self.pending_length = Some(buffer.len());
            Vec::new()
        }

        fn poll(&mut self) -> Option<Vec<HighlightSpan>> {
            self.ready
                .replace(false)
                .then(|| self.pending_length.take())
                .flatten()
                .map(|length| {
                    vec![HighlightSpan::new(
                        0,
                        length.min(3),
                        HighlightStyle::foreground(HighlightColor::Ansi(42)),
                    )]
                })
        }
    }

    let calls = Rc::new(Cell::new(0));
    let ready = Rc::new(Cell::new(false));
    let mut editor = Editor::from_bytes(b"one\n", None, false);
    editor.set_syntax_highlighter(Box::new(AsyncHighlighter {
        calls: Rc::clone(&calls),
        ready: Rc::clone(&ready),
        pending_length: None,
    }));

    let mut first_frame = Vec::new();
    editor
        .render_to(&mut first_frame, None)
        .expect("schedule initial syntax work");
    assert_eq!(calls.get(), 1);
    assert!(editor.syntax_highlights.is_empty());

    editor
        .execute_keys(b"iXYZ\x1b")
        .expect("edit while syntax work is pending");
    keep_syntax_highlighting_deferred(&mut editor);
    let mut typing_frame = Vec::new();
    editor
        .render_to(&mut typing_frame, None)
        .expect("render without reparsing every key");
    assert_eq!(calls.get(), 1);
    assert!(editor.syntax_highlights.is_empty());

    editor.syntax_highlight_ready_at = Some(Instant::now() - SYNTAX_HIGHLIGHT_DEBOUNCE);
    assert!(editor.refresh_syntax_highlighting(false, None));
    assert_eq!(calls.get(), 2);

    ready.set(true);
    assert!(editor.refresh_syntax_highlighting(false, None));
    assert_eq!(editor.syntax_highlights[0].end, 3);
    assert_eq!(editor.syntax_line_offsets, vec![0]);
}

#[test]
fn syntax_preview_styles_the_first_frame_and_refreshes_after_scrolling() {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct PreviewHighlighter {
        visible_ranges: Rc<RefCell<Vec<Range<usize>>>>,
        ready: Rc<Cell<bool>>,
        pending: bool,
        full_length: usize,
    }

    impl SyntaxHighlighter for PreviewHighlighter {
        fn highlight(&mut self, _: &[u8]) -> Vec<HighlightSpan> {
            self.pending = true;
            Vec::new()
        }

        fn highlight_visible(
            &mut self,
            buffer: &[u8],
            visible_range: Range<usize>,
        ) -> Option<Vec<HighlightSpan>> {
            self.visible_ranges.borrow_mut().push(visible_range.clone());
            let line_end = buffer[visible_range.clone()]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(visible_range.end, |offset| visible_range.start + offset);
            let color = if visible_range.start == 0 { 42 } else { 43 };
            Some(vec![HighlightSpan::new(
                visible_range.start,
                line_end,
                HighlightStyle::foreground(HighlightColor::Ansi(color)),
            )])
        }

        fn poll(&mut self) -> Option<Vec<HighlightSpan>> {
            if self.ready.replace(false) {
                self.pending = false;
                Some(vec![HighlightSpan::new(
                    0,
                    self.full_length,
                    HighlightStyle::foreground(HighlightColor::Ansi(44)),
                )])
            } else {
                None
            }
        }

        fn has_pending_work(&self) -> bool {
            self.pending
        }
    }

    let source = b"one\ntwo\nthree\n";
    let visible_ranges = Rc::new(RefCell::new(Vec::new()));
    let ready = Rc::new(Cell::new(false));
    let mut editor = Editor::from_bytes(source, None, false);
    editor.screen_rows = 3;
    editor.screen_cols = 20;
    editor.set_syntax_highlighter(Box::new(PreviewHighlighter {
        visible_ranges: Rc::clone(&visible_ranges),
        ready: Rc::clone(&ready),
        pending: false,
        full_length: source.len(),
    }));

    let mut first_frame = Vec::new();
    editor
        .render_to(&mut first_frame, None)
        .expect("render first syntax preview");
    assert_eq!(visible_ranges.borrow().as_slice(), vec![0..7]);
    assert!(first_frame
        .windows(b"\x1b[38;5;42mone".len())
        .any(|row| row == b"\x1b[38;5;42mone"));

    editor.scroll_screen(1, 1);
    let mut scrolled_frame = Vec::new();
    editor
        .render_to(&mut scrolled_frame, None)
        .expect("render scrolled syntax preview");
    assert_eq!(visible_ranges.borrow().as_slice(), [0..7, 4..13]);
    assert!(editor.rendered_rows[0].starts_with(b"\x1b[38;5;43mtwo"));

    ready.set(true);
    let mut completed_frame = Vec::new();
    editor
        .render_to(&mut completed_frame, None)
        .expect("render completed syntax result");
    assert!(editor.rendered_rows[0].starts_with(b"\x1b[38;5;44mtwo"));
    assert!(!editor.rendered_rows[0].starts_with(b"two"));

    let preview_count = visible_ranges.borrow().len();
    editor
        .execute_keys(b"iX\x1b")
        .expect("edit after syntax result");
    keep_syntax_highlighting_deferred(&mut editor);
    let mut typing_frame = Vec::new();
    editor
        .render_to(&mut typing_frame, None)
        .expect("render during syntax debounce");
    assert_eq!(visible_ranges.borrow().len(), preview_count);
}

#[test]
fn syntax_highlighting_keeps_unrelated_rows_styled_while_typing() {
    use std::cell::Cell;
    use std::rc::Rc;

    struct Highlighter {
        calls: Rc<Cell<usize>>,
    }

    impl SyntaxHighlighter for Highlighter {
        fn highlight(&mut self, _: &[u8]) -> Vec<HighlightSpan> {
            self.calls.set(self.calls.get() + 1);
            vec![HighlightSpan::new(
                8,
                13,
                HighlightStyle::foreground(HighlightColor::Ansi(42)),
            )]
        }
    }

    let calls = Rc::new(Cell::new(0));
    let mut editor = Editor::from_bytes(b"one\ntwo\nthree\n", None, false);
    editor.set_syntax_highlighter(Box::new(Highlighter {
        calls: Rc::clone(&calls),
    }));
    editor.screen_rows = 4;
    editor.screen_cols = 20;

    let mut initial = Vec::new();
    editor
        .render_to(&mut initial, None)
        .expect("render initial syntax highlighting");
    assert_eq!(calls.get(), 1);

    editor.execute_keys(b"iX\x1b").expect("edit the first line");
    keep_syntax_highlighting_deferred(&mut editor);
    let mut typing = Vec::new();
    editor
        .render_to(&mut typing, None)
        .expect("render while syntax highlighting is deferred");

    assert_eq!(calls.get(), 1);
    assert_eq!(editor.syntax_highlights[0].start, 9);
    assert_eq!(
        editor.rendered_rows[2],
        b"\x1b[38;5;42mthree\x1b[0m\x1b[K".to_vec()
    );
    assert!(!typing.windows(b"three".len()).any(|row| row == b"three"));
}

#[test]
fn syntax_line_offset_shifts_coalesce_per_edited_row() {
    use std::cell::Cell;
    use std::rc::Rc;

    struct Highlighter {
        calls: Rc<Cell<usize>>,
    }

    impl SyntaxHighlighter for Highlighter {
        fn highlight(&mut self, _: &[u8]) -> Vec<HighlightSpan> {
            self.calls.set(self.calls.get() + 1);
            vec![HighlightSpan::new(
                8,
                13,
                HighlightStyle::foreground(HighlightColor::Ansi(42)),
            )]
        }
    }

    let calls = Rc::new(Cell::new(0));
    let mut editor = Editor::from_bytes(b"one\ntwo\nthree\n", None, false);
    editor.set_syntax_highlighter(Box::new(Highlighter {
        calls: Rc::clone(&calls),
    }));
    editor.screen_rows = 6;
    editor.screen_cols = 20;
    editor
        .render_to(&mut Vec::new(), None)
        .expect("render initial syntax highlighting");

    editor
        .execute_keys(b"iABCDEFGH\x1b")
        .expect("type a run of bytes into one row");
    assert_eq!(editor.syntax_line_offset_shifts, vec![(1, 8)]);

    editor.row = 1;
    editor.col = 2;
    editor
        .execute_keys(b"iXY\x1b")
        .expect("type into a second row");
    assert_eq!(editor.syntax_line_offset_shifts, vec![(1, 8), (2, 2)]);

    // The coalesced list has to resolve the same offsets a per-byte list
    // would have produced.
    assert_eq!(editor.syntax_line_offset(0), 0);
    assert_eq!(editor.syntax_line_offset(1), 12);
    assert_eq!(editor.syntax_line_offset(2), 18);
}

#[test]
fn plain_line_rendering_stops_at_the_visible_columns() {
    let mut rendered = Vec::new();
    write_plain_line(&mut rendered, b"ab\tcd\x01ignored", 1, 5, 4)
        .expect("render clipped plain line");

    assert_eq!(rendered, b"b  cd");
}

#[test]
fn unchanged_frames_only_emit_cursor_positioning() {
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 20;
    let mut first = Vec::new();
    editor.render_to(&mut first, None).expect("first frame");
    assert!(first.windows(4).any(|bytes| bytes == b"\x1b[2J"));

    let mut second = Vec::new();
    editor
        .render_to(&mut second, None)
        .expect("unchanged frame");
    assert!(!second.windows(3).any(|bytes| bytes == b"\x1b[K"));
    assert!(second.starts_with(b"\x1b[?25h"));

    editor.execute_keys(b"l").expect("move cursor");
    let mut moved = Vec::new();
    editor
        .render_to(&mut moved, None)
        .expect("cursor-only frame");
    assert!(!moved.windows(3).any(|bytes| bytes == b"\x1b[K"));

    editor.execute_keys(b"x").expect("change first row");
    let mut changed = Vec::new();
    editor.render_to(&mut changed, None).expect("changed frame");
    assert!(changed.windows(6).any(|bytes| bytes == b"\x1b[1;1H"));
    assert!(!changed.windows(6).any(|bytes| bytes == b"\x1b[2;1H"));
}

/// A resize the editor never noticed leaves the terminal holding rows this
/// editor did not write, and the differential renderer would keep skipping
/// them because the buffer behind them never changed.
#[test]
fn a_forced_redraw_repaints_every_row_over_an_unchanged_buffer() {
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 20;
    editor
        .render_to(&mut Vec::new(), None)
        .expect("first frame");

    let mut unchanged = Vec::new();
    editor
        .render_to(&mut unchanged, None)
        .expect("unchanged frame");
    assert!(!unchanged.windows(4).any(|bytes| bytes == b"\x1b[2J"));
    // No row was rewritten, so nothing cleared to end of line either. The
    // trailing cursor placement is all an unchanged frame emits.
    assert!(!unchanged.windows(3).any(|bytes| bytes == b"\x1b[K"));

    editor.force_redraw();
    let mut repainted = Vec::new();
    editor
        .render_to(&mut repainted, None)
        .expect("forced frame");
    assert!(repainted.starts_with(b"\x1b[2J\x1b[H"));
    assert!(repainted.windows(3).any(|bytes| bytes == b"\x1b[K"));
    // Row 1 is skipped: its positioning is indistinguishable from the
    // trailing cursor placement, which every frame emits.
    for row in 2..=4 {
        let cursor_to_row = format!("\x1b[{row};1H").into_bytes();
        assert!(
            repainted
                .windows(cursor_to_row.len())
                .any(|bytes| bytes == cursor_to_row),
            "row {row} was not repainted"
        );
    }
}

/// Vi's `^L`. Without it there is no way back from a screen the editor did
/// not corrupt and cannot detect.
#[test]
fn control_l_forces_a_full_redraw() {
    let mut editor = Editor::from_bytes(b"one\ntwo\n", None, false);
    editor.screen_rows = 4;
    editor.screen_cols = 20;
    editor
        .render_to(&mut Vec::new(), None)
        .expect("first frame");

    editor.execute_keys(b"\x0c").expect("redraw command");
    let mut repainted = Vec::new();
    editor
        .render_to(&mut repainted, None)
        .expect("redrawn frame");

    assert!(repainted.starts_with(b"\x1b[2J\x1b[H"));
    assert!(repainted.windows(6).any(|bytes| bytes == b"\x1b[1;1H"));
    assert!(repainted.windows(6).any(|bytes| bytes == b"\x1b[2;1H"));
}

fn wrapped_editor(data: &[u8], rows: usize, cols: usize) -> Editor {
    let mut editor = Editor::from_bytes(data, None, false);
    editor.screen_rows = rows;
    editor.screen_cols = cols;
    editor.execute_ex("set wrap");
    editor
}

fn frame_rows(editor: &mut Editor) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut rendered = Vec::new();
    editor.render_to(&mut rendered, None).expect("render frame");
    let rows = editor
        .rendered_rows
        .iter()
        .take(editor.body_rows())
        .map(|row| row.strip_suffix(b"\x1b[K").unwrap_or(row).to_vec())
        .collect();
    (rows, rendered)
}

fn ends_with_cursor(rendered: &[u8], row: usize, col: usize) -> bool {
    rendered.ends_with(format!("\x1b[?25h\x1b[{row};{col}H").as_bytes())
}

#[test]
fn wrapped_lines_continue_on_the_next_rows_with_a_blank_gutter() {
    let mut editor = wrapped_editor(b"abcdefghijklmn\nxy", 6, 9);
    editor.execute_ex("set nu");

    let (rows, rendered) = frame_rows(&mut editor);
    assert_eq!(
        rows,
        vec![
            b"  1 abcde".to_vec(),
            b"    fghij".to_vec(),
            b"    klmn".to_vec(),
            b"  2 xy".to_vec(),
            b"~".to_vec(),
        ]
    );
    assert!(ends_with_cursor(&rendered, 1, 5));

    editor
        .execute_keys(b"$")
        .expect("move to the end of the line");
    let (_, rendered) = frame_rows(&mut editor);
    assert!(ends_with_cursor(&rendered, 3, 8));

    editor.execute_ex("set nowrap");
    let (rows, _) = frame_rows(&mut editor);
    assert_eq!(rows[0], b"  1 jklmn");
}

#[test]
fn wrapping_splits_tabs_and_control_bytes_across_rows() {
    let mut editor = wrapped_editor(b"abc\tde\x01f", 4, 5);

    let (rows, _) = frame_rows(&mut editor);
    assert_eq!(
        rows,
        vec![b"abc  ".to_vec(), b"   de".to_vec(), b"^Af".to_vec()]
    );
}

#[test]
fn a_wrapped_line_that_does_not_fit_is_shown_as_at_signs() {
    let mut editor = wrapped_editor(b"one\nabcdefghijkl\nlast", 4, 5);

    let (rows, _) = frame_rows(&mut editor);
    assert_eq!(rows, vec![b"one".to_vec(), b"@".to_vec(), b"@".to_vec()]);
    assert_eq!(editor.last_visible_line(), 0);

    editor
        .execute_keys(b"j")
        .expect("move onto the wrapped line");
    let (rows, rendered) = frame_rows(&mut editor);
    assert_eq!(editor.screen_top, 1);
    assert_eq!(
        rows,
        vec![b"abcde".to_vec(), b"fghij".to_vec(), b"kl".to_vec()]
    );
    assert!(ends_with_cursor(&rendered, 1, 1));

    editor
        .execute_keys(b"j")
        .expect("move below the wrapped line");
    let (rows, rendered) = frame_rows(&mut editor);
    assert_eq!(editor.screen_top, 2);
    assert_eq!(rows[0], b"last");
    assert!(ends_with_cursor(&rendered, 1, 1));
}

#[test]
fn a_cursor_line_taller_than_the_screen_scrolls_within_itself() {
    let line = (b'a'..=b'z').collect::<Vec<_>>();
    let mut editor = wrapped_editor(&line, 4, 5);

    editor
        .execute_keys(b"$")
        .expect("move to the end of the line");
    let (rows, rendered) = frame_rows(&mut editor);
    assert_eq!(editor.wrap_skip, 3);
    assert_eq!(
        rows,
        vec![b"pqrst".to_vec(), b"uvwxy".to_vec(), b"z".to_vec()]
    );
    assert!(ends_with_cursor(&rendered, 3, 1));
    assert_eq!(
        editor.mouse_point(mouse(MouseEventKind::Down(MouseButton::Left), 1, 1)),
        Some(SelectionPoint {
            line: 0,
            offset: 21
        })
    );

    editor
        .execute_keys(b"0")
        .expect("move to the start of the line");
    let (rows, rendered) = frame_rows(&mut editor);
    assert_eq!(editor.wrap_skip, 0);
    assert_eq!(rows[0], b"abcde");
    assert!(ends_with_cursor(&rendered, 1, 1));
}

#[test]
fn insert_at_the_end_of_an_exactly_full_row_moves_to_a_new_row() {
    let mut editor = wrapped_editor(b"abcde\nnext", 5, 5);

    editor.execute_keys(b"A").expect("append to the line");
    let (rows, rendered) = frame_rows(&mut editor);
    assert_eq!(
        rows,
        vec![
            b"abcde".to_vec(),
            Vec::new(),
            b"next".to_vec(),
            b"~".to_vec()
        ]
    );
    assert!(ends_with_cursor(&rendered, 2, 1));
}

#[test]
fn wrapped_mouse_points_map_continuation_rows_to_their_line() {
    let mut editor = wrapped_editor(b"abcdefghij\nxyz", 5, 9);
    editor.execute_ex("set nu");

    let press = |column, row| mouse(MouseEventKind::Down(MouseButton::Left), column, row);
    assert_eq!(
        editor.mouse_point(press(5, 1)),
        Some(SelectionPoint { line: 0, offset: 6 })
    );
    assert_eq!(
        editor.mouse_point(press(0, 1)),
        Some(SelectionPoint { line: 0, offset: 5 })
    );
    assert_eq!(
        editor.mouse_point(press(5, 2)),
        Some(SelectionPoint { line: 1, offset: 1 })
    );
    assert_eq!(
        editor.mouse_point(press(5, 3)),
        Some(SelectionPoint { line: 1, offset: 3 })
    );

    editor.handle_mouse_event(mouse(MouseEventKind::ScrollRight, 3, 0));
    assert_eq!(editor.screen_left, 0);
}

#[test]
fn wrapped_paging_and_screen_motions_count_screen_rows() {
    let mut data = Vec::new();
    for line in 0..10 {
        data.extend_from_slice(format!("line{line}-long\n").as_bytes());
    }
    // Every line takes two rows of a five-column screen.
    let mut editor = wrapped_editor(&data, 7, 5);
    assert_eq!(editor.last_visible_line(), 2);

    editor
        .execute_keys(b"L")
        .expect("move to the last visible line");
    assert_eq!(editor.cursor(), (2, 0));
    editor.execute_keys(b"M").expect("move to the middle line");
    assert_eq!(editor.cursor(), (1, 0));

    editor.execute_keys(b"\x06").expect("page forward");
    assert_eq!(editor.screen_top, 2);
    assert_eq!(editor.cursor(), (2, 0));

    editor.execute_keys(b"G").expect("move to the last line");
    frame_rows(&mut editor);
    assert_eq!(editor.cursor().0, 9);
    assert_eq!(editor.last_visible_line(), 9);
    assert_eq!(editor.screen_top, 7);
}

#[test]
fn linebreak_wraps_after_the_last_breakat_character_that_fits() {
    let mut editor = wrapped_editor(b"one two three-four\nnext", 6, 8);
    editor.execute_ex("set lbr");

    let (rows, rendered) = frame_rows(&mut editor);
    assert_eq!(
        rows,
        vec![
            b"one two ".to_vec(),
            b"three-".to_vec(),
            b"four".to_vec(),
            b"next".to_vec(),
            b"~".to_vec(),
        ]
    );
    assert!(ends_with_cursor(&rendered, 1, 1));

    editor
        .execute_keys(b"fh")
        .expect("move onto the second row");
    let (_, rendered) = frame_rows(&mut editor);
    assert!(ends_with_cursor(&rendered, 2, 2));

    // The blank after a broken row selects the row's last character.
    assert_eq!(
        editor.mouse_point(mouse(MouseEventKind::Down(MouseButton::Left), 7, 1)),
        Some(SelectionPoint {
            line: 0,
            offset: 13
        })
    );

    // A word longer than a row still breaks at the screen edge.
    let mut editor = wrapped_editor(b"abcdefghij k", 4, 8);
    editor.execute_ex("set linebreak");
    let (rows, _) = frame_rows(&mut editor);
    assert_eq!(
        rows,
        vec![b"abcdefgh".to_vec(), b"ij k".to_vec(), b"~".to_vec()]
    );

    editor.execute_ex("set nolinebreak");
    let (rows, _) = frame_rows(&mut editor);
    assert_eq!(rows[1], b"ij k");
}

#[test]
fn linebreak_rows_never_start_with_the_blank_they_broke_at() {
    // "cd." ends exactly at the edge, so the blank after it overflows.
    let mut editor = wrapped_editor(b"ab cd.  ef\tgh", 5, 6);
    editor.execute_ex("set lbr nu");
    editor.screen_cols = 10;

    let (rows, _) = frame_rows(&mut editor);
    assert_eq!(
        rows,
        vec![
            b"  1 ab cd.".to_vec(),
            b"    ef    ".to_vec(),
            b"    gh".to_vec(),
            b"~".to_vec(),
        ]
    );

    // A cursor on a hanging blank stays at the end of its row.
    editor
        .execute_keys(b"6|")
        .expect("move onto the hanging blank");
    let (_, rendered) = frame_rows(&mut editor);
    assert!(ends_with_cursor(&rendered, 1, 10));

    // Typing after a hanging blank continues on the next row.
    let mut editor = wrapped_editor(b"ab cd", 4, 5);
    editor.execute_ex("set lbr");
    editor.execute_keys(b"A x").expect("append past the edge");
    let (rows, rendered) = frame_rows(&mut editor);
    assert_eq!(rows, vec![b"ab cd".to_vec(), b"x".to_vec(), b"~".to_vec()]);
    assert!(ends_with_cursor(&rendered, 2, 2));
    editor.execute_keys(b"\x08").expect("erase the typed word");
    let (rows, rendered) = frame_rows(&mut editor);
    assert_eq!(rows, vec![b"ab cd".to_vec(), Vec::new(), b"~".to_vec()]);
    assert!(ends_with_cursor(&rendered, 2, 1));
}

#[test]
fn rows_that_fill_the_screen_are_not_erased_from_the_last_column() {
    // Erasing to the end of the line after writing the last column wipes
    // that column's character in real terminals.
    let mut editor = wrapped_editor(b"ab the end\nabcdefghijk", 6, 10);
    editor.execute_ex("set lbr nu");
    editor.screen_cols = 10;
    editor
        .render_to(&mut Vec::new(), None)
        .expect("render frame");
    assert_eq!(
        editor.rendered_rows[..4],
        [
            b"  1 ab the".to_vec(),
            b"    end\x1b[K".to_vec(),
            b"  2 abcdef".to_vec(),
            b"    ghijk\x1b[K".to_vec(),
        ]
    );

    editor.execute_ex("set nowrap");
    editor
        .render_to(&mut Vec::new(), None)
        .expect("render frame");
    assert_eq!(
        editor.rendered_rows[..2],
        [b"  1 ab the".to_vec(), b"  2 abcdef".to_vec()]
    );
}

#[test]
fn set_bang_toggles_boolean_options() {
    let mut editor = Editor::from_bytes(b"one\n", None, false);

    editor.execute_ex("set wrap!");
    assert!(editor.wrap);
    editor.execute_ex("set wrap! nu!");
    assert!(!editor.wrap);
    assert!(editor.number);
    editor.execute_ex("set nonu lbr");
    assert!(!editor.number);
    assert!(editor.linebreak);
    assert_eq!(editor.status(), "");

    editor.execute_ex("set wrapx!");
    assert_eq!(editor.status(), "bad option: wrapx!");
}

/// The run loop asks once per frame and has to see the resize exactly once,
/// or every later frame would repaint in full.
#[test]
fn a_recorded_resize_is_reported_once() {
    let mut reader = KeyReader::from_bytes(b"");
    assert!(!reader.take_resized());

    reader.resized = true;
    assert!(reader.take_resized());
    assert!(!reader.take_resized());
}

#[test]
fn failed_star_matches_evaluate_each_state_once() {
    let text = vec![b'a'; 64];
    let pattern = b"^a*a*a*a*a*a*a*a*b$";
    let mut context = MatchContext::new(pattern);
    let matched =
        match_pattern_captures(&text, 0, pattern, 0, [None; 10], Vec::new(), &mut context);

    assert!(matched.is_none());
    assert!(context.evaluated_states <= (text.len() + 1) * (pattern.len() + 1));
}

#[test]
fn undo_journal_stores_only_changed_bytes() {
    let mut data = vec![b'a'; 1_000_000];
    data.push(b'\n');
    let mut editor = Editor::from_bytes(&data, None, false);
    editor
        .execute_keys(b"Axyz\x1b")
        .expect("append to large buffer");

    assert_eq!(editor.undo.len(), 1);
    assert_eq!(editor.undo[0].edits.len(), 1);
    let Edit::Bytes {
        removed, inserted, ..
    } = &editor.undo[0].edits[0]
    else {
        panic!("append should create a byte edit");
    };
    assert!(removed.is_empty());
    assert_eq!(inserted, b"xyz");

    editor.execute_keys(b"u").expect("undo append");
    assert_eq!(editor.bytes(), data);
    editor.execute_keys(b"\x12").expect("redo append");
    assert!(editor.bytes().ends_with(b"xyz\n"));
}
