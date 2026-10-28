use super::*;

impl Editor {
    pub fn new(filename: Option<PathBuf>, readonly: bool) -> io::Result<Self> {
        let no_file = filename.is_none();
        let mut editor = Self::from_bytes(&[], filename, readonly);
        if no_file {
            editor.trailing_newline = true;
        }
        editor.load_current()?;
        Ok(editor)
    }

    /// Construct an editor around an existing byte buffer without touching
    /// the filesystem. This is the small embedding API; the terminal runner
    /// is optional and the editing state remains owned by the caller.
    pub fn from_bytes(data: &[u8], filename: Option<PathBuf>, readonly: bool) -> Self {
        let mut editor = Self {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
            filename,
            alternate_filename: None,
            modified: false,
            readonly,
            trailing_newline: true,
            mode: Mode::Command,
            number: false,
            wrap: false,
            linebreak: false,
            textwidth: 0,
            autoindent: false,
            expandtab: false,
            flash: false,
            ignorecase: false,
            showmatch: false,
            tabstop: 8,
            modeline: true,
            modelines: 5,
            status: String::new(),
            status_bytes: Vec::new(),
            status_highlighted: false,
            hit_return: false,
            yank: Vec::new(),
            yank_linewise: true,
            registers: HashMap::new(),
            selected_register: None,
            undo: Vec::new(),
            redo: Vec::new(),
            pending_change: None,
            replaying: false,
            last_change: Vec::new(),
            search: None,
            char_search: None,
            quit: false,
            force_quit: false,
            next_file: false,
            previous_file: false,
            rewind_files: false,
            file_index: 0,
            file_count: 1,
            screen_rows: 24,
            screen_cols: 80,
            screen_top: 0,
            screen_left: 0,
            wrap_skip: 0,
            rendered_rows: Vec::new(),
            rendered_size: (0, 0),
            selection: None,
            mouse_viewport_scrolled: false,
            last_mouse_click: None,
            marks: [None; 26],
            syntax_highlighter: None,
            syntax_highlights: Vec::new(),
            syntax_preview_highlights: Vec::new(),
            syntax_line_offsets: Vec::new(),
            syntax_line_offset_shifts: Vec::new(),
            syntax_highlights_dirty: false,
            syntax_highlight_ready_at: None,
            syntax_highlight_request_pending: false,
            syntax_preview_viewport: None,
            syntax_preview_range: None,
            syntax_buffer_snapshot: None,
        };
        editor.set_bytes(data);
        editor.apply_modelines();
        editor
    }

    /// Return the current buffer in the same newline form used by `:write`.
    pub fn bytes(&self) -> Vec<u8> {
        // Sized up front: an asynchronous highlighter takes this snapshot on
        // every debounced edit, and growing it reallocates the whole buffer
        // roughly a dozen times for a large file.
        let mut data = Vec::with_capacity(serialized_lines_len(&self.lines));
        for (index, line) in self.lines.iter().enumerate() {
            data.extend_from_slice(line);
            if index + 1 < self.lines.len() || self.trailing_newline {
                data.push(b'\n');
            }
        }
        data
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// The most recent user-facing status or error message.
    pub fn status(&self) -> &str {
        &self.status
    }

    pub(super) fn clear_status(&mut self) {
        self.status.clear();
        self.status_bytes.clear();
        self.status_highlighted = false;
    }

    pub(super) fn set_status(&mut self, status: impl Into<String>) {
        let status = status.into();
        self.status = status.clone();
        self.status_bytes = status.into_bytes();
        self.status_highlighted = false;
    }

    pub(super) fn set_error(&mut self, status: impl Into<String>) {
        let status = status.into();
        self.status = status.clone();
        self.status_bytes = status.into_bytes();
        self.status_highlighted = true;
    }

    pub(super) fn set_status_bytes(&mut self, status: Vec<u8>) {
        self.status = String::from_utf8_lossy(&status).into_owned();
        self.status_bytes = status;
        self.status_highlighted = false;
    }

    pub(super) fn set_file_context(&mut self, index: usize, count: usize) {
        self.file_index = index;
        self.file_count = count.max(1);
    }

    pub(super) fn remaining_files(&self) -> usize {
        self.file_count.saturating_sub(self.file_index + 1)
    }

    pub(super) fn finish_write_command(&mut self, command: &str, force: bool) {
        if command == "wn" {
            self.quit = true;
            self.next_file = true;
        } else if self.remaining_files() > 0 && !force {
            self.set_error(format!("{} more file(s) to edit", self.remaining_files()));
        } else {
            self.quit = true;
        }
    }

    pub(super) fn finish_zz_command(&mut self) {
        if self.remaining_files() > 0 {
            self.set_error(format!("{} more file(s) to edit", self.remaining_files()));
        } else {
            self.quit = true;
        }
    }

    /// Return the zero-based logical cursor position.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// Return the current filename, if one is associated with the buffer.
    pub fn filename(&self) -> Option<&Path> {
        self.filename.as_deref()
    }

    /// Whether an ex command has requested that the embedding caller stop.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Install a host-provided syntax highlighter for the terminal renderer.
    ///
    /// The highlighter is deliberately owned by the editor so it can retain
    /// parser state between redraws. Use [`Editor::clear_syntax_highlighter`]
    /// to return to the unstyled base renderer. A closure can be supplied
    /// directly because closures implementing `FnMut(&[u8]) -> Vec<HighlightSpan>`
    /// implement [`SyntaxHighlighter`]. Async highlighters can return their
    /// completed work from [`SyntaxHighlighter::poll`] without stalling input.
    pub fn set_syntax_highlighter(&mut self, highlighter: Box<dyn SyntaxHighlighter>) {
        self.syntax_highlighter = Some(highlighter);
        self.syntax_highlights.clear();
        self.syntax_preview_highlights.clear();
        self.syntax_line_offsets.clear();
        self.syntax_line_offset_shifts.clear();
        self.syntax_highlight_request_pending = false;
        self.syntax_preview_viewport = None;
        self.syntax_preview_range = None;
        self.syntax_buffer_snapshot = None;
        self.mark_syntax_highlighting_dirty(false);
    }

    /// Remove the optional syntax highlighter and restore plain rendering.
    pub fn clear_syntax_highlighter(&mut self) {
        self.syntax_highlighter = None;
        self.syntax_highlights.clear();
        self.syntax_preview_highlights.clear();
        self.syntax_line_offsets.clear();
        self.syntax_line_offset_shifts.clear();
        self.syntax_highlights_dirty = false;
        self.syntax_highlight_ready_at = None;
        self.syntax_highlight_request_pending = false;
        self.syntax_preview_viewport = None;
        self.syntax_preview_range = None;
        self.syntax_buffer_snapshot = None;
    }

    /// Tell the installed highlighter to recompute even when the buffer did
    /// not change, for example after the embedding application changes its
    /// theme or language selection.
    pub fn invalidate_syntax_highlighting(&mut self) {
        self.mark_syntax_highlighting_dirty(false);
        self.syntax_highlights.clear();
        self.syntax_preview_highlights.clear();
        self.syntax_line_offsets.clear();
        self.syntax_line_offset_shifts.clear();
        self.syntax_highlight_request_pending = false;
        self.syntax_preview_viewport = None;
        self.syntax_preview_range = None;
        self.syntax_buffer_snapshot = None;
    }

    /// Return the installed highlighter's current ranges in complete-buffer
    /// byte offsets. This lets a non-terminal embedding reuse the same
    /// host-provided highlighting data in its own renderer.
    pub fn syntax_highlights(&mut self) -> Option<&[HighlightSpan]> {
        self.refresh_syntax_highlighting(true, None);
        self.syntax_highlighter
            .as_ref()
            .map(|_| self.syntax_highlights.as_slice())
    }

    /// Feed decoded terminal bytes through the editor without taking over a
    /// terminal. This is useful for embedders and behavioral tests; the
    /// terminal integration is exercised by `tests/reference.sh`.
    pub fn execute_keys(&mut self, keys: &[u8]) -> io::Result<()> {
        let mut reader = KeyReader::from_bytes(keys);
        while reader.has_pending() && !self.quit {
            let key = reader.key()?;
            let special = reader.take_special();
            if self.hit_return {
                if matches!(key, b'\r' | b'\n') {
                    self.hit_return = false;
                    self.clear_status();
                }
                continue;
            }
            self.clear_status();
            match self.mode {
                Mode::Command => self.handle_command(&mut reader, key)?,
                Mode::Insert | Mode::Replace => self.handle_insert(&mut reader, key, special)?,
            }
        }
        Ok(())
    }

    pub(super) fn set_bytes(&mut self, data: &[u8]) {
        self.trailing_newline = data.last() == Some(&b'\n');
        self.lines = data
            .split(|b| *b == b'\n')
            .map(|line| line.to_vec())
            .collect();
        if self.trailing_newline {
            let _ = self.lines.pop();
        }
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
        self.row = 0;
        self.col = 0;
        self.screen_top = 0;
        self.screen_left = 0;
        self.wrap_skip = 0;
        self.selection = None;
        self.mouse_viewport_scrolled = false;
        self.last_mouse_click = None;
        self.mark_syntax_highlighting_dirty(false);
        self.syntax_highlights.clear();
        self.syntax_preview_highlights.clear();
        self.syntax_line_offsets.clear();
        self.syntax_line_offset_shifts.clear();
        self.syntax_highlight_request_pending = false;
        self.syntax_preview_viewport = None;
        self.syntax_preview_range = None;
        self.syntax_buffer_snapshot = None;
    }

    pub(super) fn mark_syntax_highlighting_dirty(&mut self, defer: bool) {
        if self.syntax_highlighter.is_none() {
            return;
        }

        if let Some(highlighter) = self.syntax_highlighter.as_mut() {
            highlighter.invalidate_visible();
        }
        self.syntax_highlights_dirty = true;
        self.syntax_highlight_ready_at = defer.then(|| Instant::now() + SYNTAX_HIGHLIGHT_DEBOUNCE);
        self.syntax_preview_highlights.clear();
        self.syntax_preview_viewport = None;
        self.syntax_preview_range = None;
        self.syntax_buffer_snapshot = None;
        if self.syntax_highlights.is_empty() {
            self.syntax_line_offsets.clear();
            self.syntax_line_offset_shifts.clear();
        }
        // Keep the previous ranges visible while the optional highlighter
        // catches up. Editing operations adjust their byte offsets below, so
        // unrelated rows do not flash back to plain text on every keypress.
    }

    pub(super) fn set_syntax_highlights(&mut self, highlights: Vec<HighlightSpan>) {
        if highlights.is_empty() {
            self.syntax_line_offsets.clear();
            self.syntax_line_offset_shifts.clear();
        } else {
            self.syntax_line_offsets = line_offsets(&self.lines);
            self.syntax_line_offset_shifts.clear();
        }
        self.syntax_highlights = highlights;
        self.syntax_preview_highlights.clear();
        self.syntax_preview_viewport = None;
        self.syntax_preview_range = None;
    }

    pub(super) fn set_syntax_preview(
        &mut self,
        highlights: Vec<HighlightSpan>,
        visible_range: Range<usize>,
        viewport: (usize, usize),
    ) {
        let has_preview = !highlights.is_empty();
        self.syntax_preview_highlights = highlights;
        self.syntax_preview_range = Some((visible_range.start, visible_range.end));
        if has_preview && self.syntax_line_offsets.is_empty() {
            // The full-result spans normally initialize these offsets. A
            // preview-only highlighter still needs them to render the line
            // start without rebuilding them on every viewport change.
            self.syntax_line_offsets = line_offsets(&self.lines);
            self.syntax_line_offset_shifts.clear();
        } else if !has_preview && self.syntax_highlights.is_empty() {
            self.syntax_line_offsets.clear();
            self.syntax_line_offset_shifts.clear();
        }
        self.syntax_preview_viewport = Some(viewport);
    }

    pub(super) fn syntax_line_offset(&self, line: usize) -> usize {
        let offset = self.syntax_line_offsets[line];
        let adjustment = self
            .syntax_line_offset_shifts
            .iter()
            .filter(|(first_line, _)| *first_line <= line)
            .map(|(_, delta)| *delta)
            .sum();
        shift_offset(offset, adjustment)
    }

    pub(super) fn adjust_syntax_highlights_for_edit(
        &mut self,
        start: usize,
        removed_len: usize,
        inserted_len: usize,
    ) {
        if self.syntax_highlights.is_empty() {
            return;
        }

        let removed_end = start.saturating_add(removed_len);
        let delta = inserted_len as isize - removed_len as isize;
        let mut adjusted = Vec::with_capacity(self.syntax_highlights.len());
        for span in self.syntax_highlights.drain(..) {
            if span.end <= start {
                adjusted.push(span);
            } else if span.start >= removed_end {
                adjusted.push(HighlightSpan::new(
                    shift_offset(span.start, delta),
                    shift_offset(span.end, delta),
                    span.style,
                ));
            } else {
                // Keep only the portions that are known to remain valid. The
                // changed bytes stay unstyled until the next parse completes.
                if span.start < start {
                    adjusted.push(HighlightSpan::new(span.start, start, span.style));
                }
                if span.end > removed_end {
                    adjusted.push(HighlightSpan::new(
                        start.saturating_add(inserted_len),
                        shift_offset(span.end, delta),
                        span.style,
                    ));
                }
            }
        }
        self.syntax_highlights = adjusted;
        if self.syntax_highlights.is_empty() {
            self.syntax_line_offsets.clear();
            self.syntax_line_offset_shifts.clear();
        }
    }

    pub(super) fn visible_byte_range(&self) -> Range<usize> {
        let first_line = self.screen_top.min(self.lines.len());
        let last_line = first_line
            .saturating_add(self.body_rows())
            .min(self.lines.len());
        let line_offset = |line: usize| {
            self.syntax_line_offsets
                .get(line)
                .map(|_| self.syntax_line_offset(line))
                .unwrap_or_else(|| self.lines[..line].iter().map(|line| line.len() + 1).sum())
        };
        let start = line_offset(first_line);
        let end = last_line
            .checked_sub(1)
            .map(|line| line_offset(line).saturating_add(self.lines[line].len()))
            .unwrap_or(start);
        start..end
    }

    pub(super) fn refresh_syntax_preview(&mut self, visible_range: Range<usize>) -> bool {
        let viewport = (self.screen_top, self.body_rows());
        if self.syntax_preview_viewport == Some(viewport) {
            return false;
        }
        let data = self
            .syntax_buffer_snapshot
            .take()
            .unwrap_or_else(|| self.bytes());
        let highlights = self
            .syntax_highlighter
            .as_mut()
            .and_then(|highlighter| highlighter.highlight_visible(&data, visible_range.clone()));
        self.syntax_buffer_snapshot = Some(data);
        self.syntax_preview_viewport = Some(viewport);
        if let Some(highlights) = highlights {
            self.set_syntax_preview(highlights, visible_range, viewport);
            true
        } else {
            self.syntax_preview_highlights.clear();
            self.syntax_preview_range = None;
            false
        }
    }

    pub(super) fn refresh_syntax_preview_with_data(
        &mut self,
        data: &[u8],
        visible_range: Range<usize>,
    ) -> bool {
        let viewport = (self.screen_top, self.body_rows());
        if self.syntax_preview_viewport == Some(viewport) {
            return false;
        }
        let highlights = self
            .syntax_highlighter
            .as_mut()
            .and_then(|highlighter| highlighter.highlight_visible(data, visible_range.clone()));
        self.syntax_preview_viewport = Some(viewport);
        if let Some(highlights) = highlights {
            self.set_syntax_preview(highlights, visible_range, viewport);
            true
        } else {
            self.syntax_preview_highlights.clear();
            self.syntax_preview_range = None;
            false
        }
    }

    /// Poll completed background work and, when necessary, request a new
    /// buffer snapshot. A render may also supply the complete-buffer byte
    /// range for its visible rows so a highlighter can provide a synchronous
    /// preview while a full parse is pending.
    pub(super) fn refresh_syntax_highlighting(
        &mut self,
        force: bool,
        visible_range: Option<Range<usize>>,
    ) -> bool {
        let mut updated = false;

        // Do not apply a result while a newer buffer is waiting to be sent to
        // the highlighter. Async implementations can otherwise briefly paint
        // byte ranges from the previous document revision.
        let completed = self
            .syntax_highlighter
            .as_mut()
            .and_then(|highlighter| highlighter.poll());
        let pending_after_poll = self
            .syntax_highlighter
            .as_ref()
            .is_some_and(|highlighter| highlighter.has_pending_work());
        self.syntax_highlight_request_pending = pending_after_poll;
        if !self.syntax_highlights_dirty {
            if let Some(highlights) = completed {
                self.set_syntax_highlights(highlights);
                updated = true;
            }
            if !self.syntax_highlight_request_pending {
                self.syntax_buffer_snapshot = None;
            }
            if self.syntax_highlight_request_pending {
                if let Some(visible_range) = visible_range {
                    updated |= self.refresh_syntax_preview(visible_range);
                }
            }
            return updated;
        }
        if !force
            && self
                .syntax_highlight_ready_at
                .is_some_and(|ready_at| Instant::now() < ready_at)
        {
            // Keep the adjusted full-result spans on screen during the
            // debounce window. Running a grammar preview here would put the
            // parser back on the input/render path for every typed byte.
            return updated;
        }
        if self.syntax_highlight_request_pending && !force {
            if let Some(visible_range) = visible_range {
                updated |= self.refresh_syntax_preview(visible_range);
            }
            return updated;
        }

        // This copy is intentionally delayed for edits. A highlighter that
        // parses on a worker thread receives one coalesced snapshot instead
        // of one complete buffer per typed byte.
        let data = self
            .syntax_buffer_snapshot
            .take()
            .unwrap_or_else(|| self.bytes());
        let (highlights, pending) = self
            .syntax_highlighter
            .as_mut()
            .map(|highlighter| {
                let highlights = highlighter.highlight(&data);
                (highlights, highlighter.has_pending_work())
            })
            .unwrap_or_default();
        self.syntax_highlight_request_pending = pending;
        if !pending {
            self.set_syntax_highlights(highlights);
            self.syntax_buffer_snapshot = None;
        } else if let Some(visible_range) = visible_range {
            updated |= self.refresh_syntax_preview_with_data(&data, visible_range);
            self.syntax_buffer_snapshot = Some(data);
        } else {
            self.syntax_buffer_snapshot = Some(data);
        }
        self.syntax_highlights_dirty = false;
        self.syntax_highlight_ready_at = None;
        updated || !pending
    }

    pub(super) fn state(&self) -> EditorState {
        EditorState {
            row: self.row,
            col: self.col,
            trailing_newline: self.trailing_newline,
            modified: self.modified,
        }
    }

    pub(super) fn undo_status(&self, change: &Change) -> String {
        let (removed, inserted, row, col) = change.edits.iter().fold(
            (0, 0, usize::MAX, usize::MAX),
            |(removed, inserted, first_row, first_col), edit| match edit {
                Edit::Bytes {
                    row,
                    start,
                    removed: old,
                    inserted: new,
                } => (
                    removed + old.len(),
                    inserted + new.len(),
                    first_row.min(*row),
                    if *row < first_row {
                        *start
                    } else if *row == first_row {
                        first_col.min(*start)
                    } else {
                        first_col
                    },
                ),
                Edit::Lines {
                    start,
                    removed: old,
                    inserted: new,
                } => (
                    removed + serialized_lines_len(old),
                    inserted + serialized_lines_len(new),
                    first_row.min(*start),
                    if *start < first_row { 0 } else { first_col },
                ),
            },
        );
        let (verb, chars) = if removed > inserted {
            ("restored", removed)
        } else if inserted > removed {
            ("deleted", inserted)
        } else {
            ("restored", removed.max(1))
        };
        let row = row.min(self.lines.len().saturating_sub(1));
        let position = self.lines[..row]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + col.min(self.lines[row].len());
        format!(
            "Undo [{}] {} {} chars at position {}",
            self.undo.len() + 1,
            verb,
            chars,
            position
        )
    }

    pub(super) fn restore_state(&mut self, state: EditorState) {
        self.row = state.row.min(self.lines.len().saturating_sub(1));
        self.col = state.col.min(self.lines[self.row].len());
        self.trailing_newline = state.trailing_newline;
        self.modified = state.modified;
        self.mark_syntax_highlighting_dirty(true);
        self.syntax_highlights.clear();
        self.syntax_preview_highlights.clear();
        self.syntax_line_offsets.clear();
        self.syntax_line_offset_shifts.clear();
        self.syntax_highlight_request_pending = false;
        self.syntax_preview_viewport = None;
        self.syntax_preview_range = None;
        self.syntax_buffer_snapshot = None;
    }

    pub(super) fn begin_change(&mut self) {
        if self.pending_change.is_none() {
            self.pending_change = Some(PendingChange {
                edits: Vec::new(),
                before: self.state(),
            });
        }
    }

    pub(super) fn changed(&mut self) {
        if self
            .pending_change
            .as_ref()
            .is_some_and(|change| change.edits.is_empty())
        {
            self.redo.clear();
        }
        self.modified = true;
        self.mark_syntax_highlighting_dirty(true);
    }

    pub(super) fn end_change(&mut self) {
        let Some(pending) = self.pending_change.take() else {
            return;
        };
        if !pending.edits.is_empty() || pending.before.trailing_newline != self.trailing_newline {
            self.undo.push(Change {
                edits: pending.edits,
                before: pending.before,
                after: self.state(),
            });
        }
    }

    pub(super) fn replace_bytes(
        &mut self,
        row: usize,
        range: std::ops::Range<usize>,
        inserted: impl Into<Vec<u8>>,
    ) -> Vec<u8> {
        let inserted = inserted.into();
        let inserted_len = inserted.len();
        let removed = self.lines[row][range.clone()].to_vec();
        if removed == inserted {
            return removed;
        }
        if !self.syntax_highlights.is_empty() {
            let line_start = self.syntax_line_offset(row);
            self.adjust_syntax_highlights_for_edit(
                line_start.saturating_add(range.start),
                removed.len(),
                inserted_len,
            );
            let delta = inserted_len as isize - removed.len() as isize;
            if delta != 0 {
                // Typing runs of bytes into one line would otherwise append one
                // shift per byte, and `syntax_line_offset` walks this list once
                // per rendered row. Coalescing keeps it proportional to the
                // number of edited rows rather than the number of keystrokes.
                match self.syntax_line_offset_shifts.last_mut() {
                    Some((first_line, previous)) if *first_line == row + 1 => *previous += delta,
                    _ => self.syntax_line_offset_shifts.push((row + 1, delta)),
                }
            }
        }
        self.lines[row].splice(range.clone(), inserted.iter().copied());
        self.changed();
        let edits = &mut self
            .pending_change
            .as_mut()
            .expect("replace_bytes requires begin_change")
            .edits;
        if let Some(Edit::Bytes {
            row: previous_row,
            start,
            removed: previous_removed,
            inserted: previous_inserted,
        }) = edits.last_mut()
        {
            let contiguous = *previous_row == row
                && range.start == *start + previous_inserted.len()
                && ((previous_removed.is_empty() && removed.is_empty())
                    || (previous_removed.len() == previous_inserted.len()
                        && removed.len() == inserted_len));
            if contiguous {
                previous_removed.extend_from_slice(&removed);
                previous_inserted.extend(inserted);
                return removed;
            }
        }
        edits.push(Edit::Bytes {
            row,
            start: range.start,
            removed: removed.clone(),
            inserted,
        });
        removed
    }

    pub(super) fn replace_lines(
        &mut self,
        range: std::ops::Range<usize>,
        inserted: Vec<Vec<u8>>,
    ) -> Vec<Vec<u8>> {
        let removed = self.lines[range.clone()].to_vec();
        if removed == inserted {
            return removed;
        }
        if !self.syntax_highlights.is_empty() {
            let start = self
                .syntax_line_offsets
                .get(range.start)
                .map(|_| self.syntax_line_offset(range.start))
                .unwrap_or_else(|| serialized_lines_len(&self.lines));
            self.adjust_syntax_highlights_for_edit(
                start,
                serialized_lines_len(&removed),
                serialized_lines_len(&inserted),
            );
        }
        self.lines.splice(range.clone(), inserted.iter().cloned());
        if !self.syntax_highlights.is_empty() {
            self.syntax_line_offsets = line_offsets(&self.lines);
            self.syntax_line_offset_shifts.clear();
        }
        self.changed();
        self.pending_change
            .as_mut()
            .expect("replace_lines requires begin_change")
            .edits
            .push(Edit::Lines {
                start: range.start,
                removed: removed.clone(),
                inserted,
            });
        removed
    }

    pub(super) fn apply_change(&mut self, change: &Change, forward: bool) {
        let edits: Box<dyn Iterator<Item = &Edit>> = if forward {
            Box::new(change.edits.iter())
        } else {
            Box::new(change.edits.iter().rev())
        };
        for edit in edits {
            match edit {
                Edit::Bytes {
                    row,
                    start,
                    removed,
                    inserted,
                } => {
                    let (old, new) = if forward {
                        (removed, inserted)
                    } else {
                        (inserted, removed)
                    };
                    self.lines[*row].splice(*start..*start + old.len(), new.iter().copied());
                }
                Edit::Lines {
                    start,
                    removed,
                    inserted,
                } => {
                    let (old, new) = if forward {
                        (removed, inserted)
                    } else {
                        (inserted, removed)
                    };
                    self.lines
                        .splice(*start..*start + old.len(), new.iter().cloned());
                }
            }
        }
        self.restore_state(if forward { change.after } else { change.before });
    }

    pub(super) fn load_current(&mut self) -> io::Result<bool> {
        let Some(path) = self.filename.clone() else {
            return Ok(false);
        };
        let mut new_file = false;
        match fs::read(&path) {
            Ok(data) => {
                self.update_readonly(&path);
                self.set_bytes(&data);
                self.clear_status();
            }
            Err(_) => {
                self.set_bytes(&[]);
                self.trailing_newline = true;
                self.clear_status();
                new_file = true;
            }
        }
        self.row = 0;
        self.col = 0;
        self.modified = false;
        self.apply_modelines();
        Ok(new_file)
    }

    pub(super) fn apply_modelines(&mut self) {
        if !self.modeline || self.modelines == 0 {
            return;
        }
        let first_end = self.modelines.min(self.lines.len());
        let last_start = self
            .lines
            .len()
            .saturating_sub(self.modelines)
            .max(first_end);
        // Overlapping head and tail regions must not apply toggles twice.
        for row in (0..first_end).chain(last_start..self.lines.len()) {
            if !self.modeline {
                break;
            }
            let Some(options) = modeline_options(&self.lines[row]) else {
                continue;
            };
            match options {
                Ok(options) => {
                    for option in options.split_whitespace() {
                        self.set_options(option);
                        if self.status_highlighted {
                            break;
                        }
                    }
                }
                Err(message) => self.set_error(message),
            }
            if self.status_highlighted {
                self.set_error(format!("modeline line {}: {}", row + 1, self.status));
                break;
            }
        }
    }

    pub(super) fn update_readonly(&mut self, path: &Path) {
        let Ok(metadata) = fs::metadata(path) else {
            return;
        };
        #[cfg(unix)]
        let readonly = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o222 == 0
        };
        #[cfg(not(unix))]
        let readonly = metadata.permissions().readonly();
        self.readonly |= readonly;
    }

    pub(super) fn current_file_status(&self, new_file: bool, show_readonly: bool) -> String {
        let name = self
            .filename
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let data = self.bytes();
        format_file_status(&name, &data, new_file, show_readonly && self.readonly)
    }

    pub(super) fn expand_filename(&self, argument: &str) -> io::Result<PathBuf> {
        let current = self
            .filename
            .as_deref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let alternate = self
            .alternate_filename
            .as_deref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut expanded = String::with_capacity(argument.len());
        let mut escaped = false;
        for character in argument.chars() {
            if escaped {
                expanded.push(character);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '%' {
                if current.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "No previous filename",
                    ));
                }
                expanded.push_str(&current);
            } else if character == '#' {
                if alternate.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "No previous filename",
                    ));
                }
                expanded.push_str(&alternate);
            } else {
                expanded.push(character);
            }
        }
        if escaped {
            expanded.push('\\');
        }
        Ok(PathBuf::from(expanded))
    }

    pub(super) fn write_file(&mut self, requested: Option<&Path>, force: bool) -> io::Result<()> {
        self.write_file_range(requested, force, None)
    }

    pub(super) fn write_file_range(
        &mut self,
        requested: Option<&Path>,
        force: bool,
        range: Option<(usize, usize)>,
    ) -> io::Result<()> {
        let path = requested
            .or(self.filename.as_deref())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "No current filename"))?;
        let path = path.to_path_buf();
        if self.readonly && !force && requested.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("'{}' is read only", path.display()),
            ));
        }
        if !force
            && requested.is_some()
            && self.filename.as_deref() != Some(path.as_path())
            && path.exists()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "File exists (:w! overrides)",
            ));
        }
        let data = if let Some((start, end)) = range {
            let first = start.min(self.lines.len() - 1);
            let last = end.min(self.lines.len() - 1);
            let mut data = Vec::new();
            for line in &self.lines[first..=last] {
                data.extend_from_slice(line);
                data.push(b'\n');
            }
            data
        } else {
            self.bytes()
        };
        fs::write(&path, &data).map_err(|error| {
            io::Error::new(error.kind(), format!("'{}' {}", path.display(), error))
        })?;
        self.filename = Some(path.clone());
        if range.is_none() {
            self.modified = false;
        }
        self.set_status(format_file_status(
            &path.display().to_string(),
            &data,
            false,
            false,
        ));
        Ok(())
    }
}
