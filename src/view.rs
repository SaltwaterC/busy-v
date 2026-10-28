use super::*;

impl Editor {
    pub(super) fn line_display_width(&self, line: &[u8], upto: usize) -> usize {
        let mut width = 0;
        for &byte in line.iter().take(upto) {
            if byte == b'\t' {
                width += self.tabstop - (width % self.tabstop);
            } else if byte.is_ascii_control() {
                width += 2;
            } else {
                width += 1;
            }
        }
        width
    }

    pub(super) fn refresh_size(&mut self) -> bool {
        let from_environment = || {
            let rows = std::env::var("LINES").ok()?.parse::<usize>().ok()?;
            let columns = std::env::var("COLUMNS").ok()?.parse::<usize>().ok()?;
            (rows > 0 && columns > 0).then_some((rows, columns))
        };
        let from_tty = terminal::size()
            .ok()
            .map(|(columns, rows)| (rows as usize, columns as usize));
        let (rows, columns) = from_tty.or(from_environment()).unwrap_or((24, 80));
        let rows = rows.clamp(2, 512);
        let columns = columns.clamp(10, 4096);
        let changed = self.screen_rows != rows || self.screen_cols != columns;
        self.screen_rows = rows;
        self.screen_cols = columns;
        changed
    }

    /// Drops what the last render believed was on screen, so the next one
    /// clears the terminal and repaints every row.
    ///
    /// [`Editor::render_to`] writes only the rows that differ from
    /// `rendered_rows`, which assumes nothing but this editor changes the
    /// screen. A resize breaks that assumption: the terminal drops rows on a
    /// shrink and pushes the survivors up again on a grow, and it does not tell
    /// the program which rows moved. Comparing sizes cannot detect it either,
    /// because a shrink and a matching grow leave the size where it started
    /// while the content has moved. Without this the stale rows are never
    /// rewritten, so the buffer and the screen stay out of step for the rest of
    /// the session.
    pub(super) fn force_redraw(&mut self) {
        self.rendered_rows.clear();
        self.rendered_size = (0, 0);
    }

    pub(super) fn body_rows(&self) -> usize {
        self.screen_rows.saturating_sub(1).max(1)
    }

    pub(super) fn horizontal_width(&self) -> usize {
        self.screen_cols
            .saturating_sub(self.number_column_width())
            .max(1)
    }

    pub(super) fn number_column_width(&self) -> usize {
        if self.number {
            self.lines.len().to_string().len().max(3) + 1
        } else {
            0
        }
    }

    /// Screen rows `line` takes, capped at `limit`. Without `:set wrap` every
    /// line takes one row. A wrapped cursor line also makes room for an insert
    /// cursor sitting just past a row that is exactly full, as Vim does.
    pub(super) fn line_rows(&self, line: usize, limit: usize) -> usize {
        let limit = limit.max(1);
        if !self.wrap {
            return 1;
        }
        if self.linebreak {
            return self.row_starts(line, limit).len();
        }
        let width = self.horizontal_width();
        let mut display_column = 0;
        for &byte in &self.lines[line] {
            display_column += fragment_len(byte, display_column, self.tabstop);
            // Stop early so a huge line costs no more than the rows asked for.
            if display_column / width >= limit {
                return limit;
            }
        }
        let mut rows = display_column.div_ceil(width).max(1);
        if line == self.row {
            rows = rows.max(self.line_display_width(&self.lines[line], self.col) / width + 1);
        }
        rows.min(limit)
    }

    /// Display columns at which the screen rows of `line` start, for at most
    /// `limit` rows. Under `:set linebreak` a wrapped row ends after the last
    /// `breakat` character that fits, as in Vim, so rows can be narrower than
    /// the screen. A word too long for a row still breaks anywhere. Blanks
    /// that overflow a row hang off its end unseen, so no row after the first
    /// starts with one; a row can then span more than `width` columns.
    pub(super) fn row_starts(&self, line: usize, limit: usize) -> Vec<usize> {
        let limit = limit.max(1);
        if !self.wrap {
            return vec![self.screen_left];
        }
        let width = self.horizontal_width();
        if !self.linebreak {
            return (0..self.line_rows(line, limit))
                .map(|row| row * width)
                .collect();
        }
        let mut starts = vec![0];
        let mut row_start = 0;
        let mut last_break = None;
        let mut hanging = false;
        let mut display_column = 0;
        let blank = |byte: u8| byte == b' ' || byte == b'\t';
        for &byte in &self.lines[line] {
            let end = display_column + fragment_len(byte, display_column, self.tabstop);
            if hanging && !blank(byte) {
                if starts.len() == limit {
                    return starts;
                }
                starts.push(display_column);
                row_start = display_column;
                last_break = None;
                hanging = false;
            }
            while !hanging && end - row_start > width {
                if blank(byte) {
                    hanging = true;
                    break;
                }
                if starts.len() == limit {
                    return starts;
                }
                let next = last_break.take().unwrap_or(row_start + width);
                starts.push(next);
                row_start = next;
            }
            display_column = end;
            if BREAKAT.contains(&byte) {
                last_break = Some(end);
            }
        }
        if line == self.row && starts.len() < limit {
            // Make room for an insert cursor past the end of a full row.
            let cursor = self.line_display_width(&self.lines[line], self.col);
            if hanging && cursor >= display_column {
                starts.push(display_column);
            } else if !hanging && cursor >= row_start + width {
                starts.push(row_start + width);
            }
        }
        starts
    }

    /// The wrapped row of the cursor line that holds the cursor, and the
    /// cursor's column within that row.
    pub(super) fn cursor_wrapped_position(&self) -> (usize, usize) {
        let cursor = self.line_display_width(&self.lines[self.row], self.col);
        if !self.linebreak {
            let width = self.horizontal_width();
            return (cursor / width, cursor % width);
        }
        let starts = self.row_starts(self.row, usize::MAX);
        let segment = starts
            .partition_point(|start| *start <= cursor)
            .saturating_sub(1);
        (segment, cursor - starts[segment])
    }

    /// The first line to show so that the lines through `line` take at most
    /// `rows` screen rows. `line` itself is always kept, even if it is taller.
    pub(super) fn wrapped_top_for(&self, line: usize, rows: usize) -> usize {
        let body = self.body_rows();
        let mut used = self.line_rows(line, body);
        let mut top = line;
        while top > 0 {
            let above = self.line_rows(top - 1, body);
            if used + above > rows {
                break;
            }
            used += above;
            top -= 1;
        }
        top
    }

    /// The last line shown in full, or `screen_top` when not even that fits.
    pub(super) fn last_visible_line(&self) -> usize {
        let last_line = self.lines.len().saturating_sub(1);
        let body = self.body_rows();
        if !self.wrap {
            return self
                .screen_top
                .saturating_add(body.saturating_sub(1))
                .min(last_line);
        }
        let mut line = self.screen_top;
        let mut used = self
            .line_rows(line, self.wrap_skip.saturating_add(body))
            .saturating_sub(self.wrap_skip);
        while line < last_line {
            used += self.line_rows(line + 1, body);
            if used > body {
                break;
            }
            line += 1;
        }
        line
    }

    /// The line shown on `screen_row`, the display column of its first text
    /// cell, and how many columns of the line the row shows. `None` past the
    /// end of the buffer.
    pub(super) fn screen_row_source(&self, screen_row: usize) -> Option<(usize, usize, usize)> {
        let width = self.horizontal_width();
        if !self.wrap {
            let line = self.screen_top.saturating_add(screen_row);
            return (line < self.lines.len()).then_some((line, self.screen_left, width));
        }
        let mut line = self.screen_top;
        let mut segment = self.wrap_skip.saturating_add(screen_row);
        while line < self.lines.len() {
            let starts = self.row_starts(line, segment + 2);
            if let Some(&start) = starts.get(segment) {
                let row_width = starts
                    .get(segment + 1)
                    .map_or(width, |next| (next - start).min(width));
                return Some((line, start, row_width));
            }
            segment -= starts.len();
            line += 1;
        }
        None
    }

    /// How many whole lines scrolling by `rows` screen rows passes over. A
    /// line taller than `rows` still scrolls, so paging never gets stuck.
    pub(super) fn wrapped_scroll_lines(&self, rows: usize, direction: i32) -> usize {
        let body = self.body_rows();
        let mut lines = 0;
        let mut used = 0;
        loop {
            let line = if direction < 0 {
                self.screen_top.checked_sub(lines + 1)
            } else {
                Some(self.screen_top + lines).filter(|line| *line < self.lines.len())
            };
            let Some(line) = line else {
                break;
            };
            used += self.line_rows(line, body);
            if used > rows {
                break;
            }
            lines += 1;
        }
        lines.max(rows.min(1))
    }

    pub(super) fn clear_mouse_selection(&mut self) {
        self.selection = None;
        self.mouse_viewport_scrolled = false;
        self.last_mouse_click = None;
    }

    pub(super) fn selection_bounds(&self) -> Option<(SelectionPoint, SelectionPoint)> {
        let selection = self.selection?;
        if selection.anchor == selection.focus {
            return None;
        }
        let (start, mut end) = if selection.anchor <= selection.focus {
            (selection.anchor, selection.focus)
        } else {
            (selection.focus, selection.anchor)
        };
        if selection.kind == SelectionKind::Characters {
            let line_length = self.lines.get(end.line)?.len();
            if end.offset < line_length {
                end.offset += 1;
            }
        }
        Some((start, end))
    }

    pub(super) fn selection_offsets_on_line(&self, line: usize) -> Option<(usize, usize)> {
        let selection = self.selection?;
        match selection.kind {
            SelectionKind::Lines => {
                let first = selection.anchor.line.min(selection.focus.line);
                let last = selection.anchor.line.max(selection.focus.line);
                (first..=last)
                    .contains(&line)
                    .then(|| (0, self.lines.get(line).map_or(0, Vec::len)))
            }
            SelectionKind::Characters | SelectionKind::Word => {
                let (start, end) = self.selection_bounds()?;
                if line < start.line || line > end.line {
                    return None;
                }
                let length = self.lines.get(line)?.len();
                let first = if line == start.line {
                    start.offset.min(length)
                } else {
                    0
                };
                let last = if line == end.line {
                    end.offset.min(length)
                } else {
                    length
                };
                (first < last).then_some((first, last))
            }
        }
    }

    pub(super) fn selection_text(&self) -> Option<Vec<u8>> {
        let selection = self.selection?;
        if selection.kind == SelectionKind::Lines {
            let first = selection.anchor.line.min(selection.focus.line);
            let last = selection
                .anchor
                .line
                .max(selection.focus.line)
                .min(self.lines.len().saturating_sub(1));
            let mut text = Vec::new();
            for line in first..=last {
                if line > first {
                    text.push(b'\n');
                }
                text.extend_from_slice(&self.lines[line]);
            }
            return (!text.is_empty()).then_some(text);
        }

        let (start, end) = self.selection_bounds()?;
        let mut text = Vec::new();
        for line in start.line..=end.line.min(self.lines.len().saturating_sub(1)) {
            let source = &self.lines[line];
            let first = if line == start.line {
                start.offset.min(source.len())
            } else {
                0
            };
            let last = if line == end.line {
                end.offset.min(source.len())
            } else {
                source.len()
            };
            text.extend_from_slice(&source[first..last]);
            if line < end.line {
                text.push(b'\n');
            }
        }
        (!text.is_empty()).then_some(text)
    }

    pub(super) fn mouse_point(&self, mouse: MouseEvent) -> Option<SelectionPoint> {
        let screen_row = mouse.row as usize;
        if screen_row >= self.body_rows() || self.lines.is_empty() {
            // The last row is the status row. Do not let a status click turn
            // into a selection of the last file line.
            return None;
        }
        let Some((line, first_column, row_width)) = self.screen_row_source(screen_row) else {
            let line = self.lines.len() - 1;
            return Some(SelectionPoint {
                line,
                offset: self.lines[line].len(),
            });
        };
        let column = mouse.column as usize;
        let gutter = self.number_column_width();
        if column < gutter && !self.wrap {
            return Some(SelectionPoint { line, offset: 0 });
        }
        // A wrapped row's gutter selects from the start of that row's text,
        // and the blank after a linebreak selects the row's last character.
        let display_column = first_column.saturating_add(
            column
                .saturating_sub(gutter)
                .min(row_width.saturating_sub(1)),
        );
        Some(SelectionPoint {
            line,
            offset: byte_offset_at_display_column(&self.lines[line], display_column, self.tabstop),
        })
    }

    pub(super) fn mouse_click_count(&mut self, point: SelectionPoint) -> u8 {
        const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(450);
        let now = Instant::now();
        let count = match self.last_mouse_click {
            Some((when, previous, count))
                if previous == point
                    && now.duration_since(when) <= DOUBLE_CLICK_WINDOW
                    && count < 3 =>
            {
                count + 1
            }
            _ => 1,
        };
        self.last_mouse_click = Some((now, point, count));
        count
    }

    pub(super) fn auto_scroll_for_mouse(&mut self, mouse: MouseEvent) {
        let body = self.body_rows();
        let row = mouse.row as usize;
        if row == 0 {
            self.screen_top = self.screen_top.saturating_sub(1);
        } else if row.saturating_add(1) >= body {
            let last_line = self.lines.len().saturating_sub(1);
            self.screen_top = self.screen_top.saturating_add(1).min(last_line);
        }

        let column = mouse.column as usize;
        let gutter = self.number_column_width();
        if self.wrap {
            // Wrapped lines have nothing hidden to either side.
        } else if column <= gutter {
            self.screen_left = self.screen_left.saturating_sub(1);
        } else if column.saturating_add(1) >= self.screen_cols {
            self.screen_left = self.screen_left.saturating_add(1);
        }
        self.mouse_viewport_scrolled = true;
    }

    pub(super) fn word_bounds(&self, point: SelectionPoint) -> (SelectionPoint, SelectionPoint) {
        let line = &self.lines[point.line];
        if line.is_empty() {
            return (point, point);
        }
        let at = point.offset.min(line.len().saturating_sub(1));
        let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
        let word_kind = word(line[at]);
        let mut start = at;
        while start > 0 && word(line[start - 1]) == word_kind {
            start -= 1;
        }
        let mut end = at + 1;
        while end < line.len() && word(line[end]) == word_kind {
            end += 1;
        }
        (
            SelectionPoint {
                line: point.line,
                offset: start,
            },
            SelectionPoint {
                line: point.line,
                offset: end,
            },
        )
    }

    pub(super) fn finish_mouse_selection(&mut self) -> Option<Vec<u8>> {
        let selection = self.selection.as_mut()?;
        if !selection.dragging {
            return None;
        }
        selection.dragging = false;
        self.selection_text()
    }

    pub(super) fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Option<Vec<u8>> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Holding Shift is the explicit escape hatch for terminal
                // native selection in terminals that provide one.
                if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                    return None;
                }
                let previous = self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.dragging);
                let copied = previous.then(|| self.finish_mouse_selection()).flatten();
                let point = match self.mouse_point(mouse) {
                    Some(point) => point,
                    None => return copied,
                };
                let count = self.mouse_click_count(point);
                let line_length = self.lines[point.line].len();
                let linewise = count >= 3
                    || mouse.modifiers.contains(KeyModifiers::ALT)
                    || mouse.modifiers.contains(KeyModifiers::SUPER);
                let wordwise =
                    !linewise && (count == 2 || mouse.modifiers.contains(KeyModifiers::CONTROL));
                let (anchor, focus, kind) = if linewise {
                    (
                        SelectionPoint {
                            line: point.line,
                            offset: 0,
                        },
                        SelectionPoint {
                            line: point.line,
                            offset: line_length,
                        },
                        SelectionKind::Lines,
                    )
                } else if wordwise {
                    let (start, end) = self.word_bounds(point);
                    (start, end, SelectionKind::Word)
                } else {
                    (point, point, SelectionKind::Characters)
                };
                self.selection = Some(MouseSelection {
                    anchor,
                    focus,
                    kind,
                    dragging: true,
                });
                copied
            }
            MouseEventKind::Drag(MouseButton::Left)
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.dragging) =>
            {
                self.auto_scroll_for_mouse(mouse);
                let point = self.mouse_point(mouse)?;
                if let Some(selection) = self.selection.as_mut() {
                    if selection.kind != SelectionKind::Lines {
                        selection.kind = SelectionKind::Characters;
                    }
                    selection.focus = if selection.kind == SelectionKind::Lines {
                        SelectionPoint {
                            line: point.line,
                            offset: self.lines[point.line].len(),
                        }
                    } else {
                        point
                    };
                }
                None
            }
            MouseEventKind::Moved
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.dragging) =>
            {
                // With any-event tracking, a motion without a button is the
                // first reliable indication that a multiplexer swallowed the
                // left-button release.
                self.finish_mouse_selection()
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let copied = self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.dragging)
                    .then(|| self.finish_mouse_selection())
                    .flatten();
                if copied.is_none()
                    && self
                        .selection
                        .as_ref()
                        .is_some_and(|selection| selection.anchor == selection.focus)
                {
                    self.selection = None;
                }
                copied
            }
            MouseEventKind::ScrollUp => {
                self.screen_top = self.screen_top.saturating_sub(3);
                self.mouse_viewport_scrolled = true;
                None
            }
            MouseEventKind::ScrollDown => {
                let last_line = self.lines.len().saturating_sub(1);
                self.screen_top = self.screen_top.saturating_add(3).min(last_line);
                self.mouse_viewport_scrolled = true;
                None
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight if self.wrap => None,
            MouseEventKind::ScrollLeft => {
                self.screen_left = self.screen_left.saturating_sub(3);
                self.mouse_viewport_scrolled = true;
                None
            }
            MouseEventKind::ScrollRight => {
                self.screen_left = self.screen_left.saturating_add(3);
                self.mouse_viewport_scrolled = true;
                None
            }
            _ => None,
        }
    }

    pub(super) fn sync_screen(&mut self) {
        let last_line = self.lines.len().saturating_sub(1);
        let body = self.body_rows();
        if self.selection.is_some() || self.mouse_viewport_scrolled {
            self.screen_top = self.screen_top.min(last_line);
            if self.screen_top != self.row {
                self.wrap_skip = 0;
            }
            return;
        }
        let half = body / 2;
        self.screen_top = self.screen_top.min(last_line);

        if self.row < self.screen_top {
            let distance = self.screen_top - self.row;
            self.screen_top = self.row;
            if distance > half {
                self.screen_top = self.row.saturating_sub(half);
            }
        } else {
            let end = self
                .screen_top
                .saturating_add(body.saturating_sub(1))
                .min(last_line);
            if self.row > end {
                let distance = self.row - end;
                if distance > half {
                    self.screen_top = self.row.saturating_sub(half);
                } else {
                    self.screen_top = self.screen_top.saturating_add(distance).min(last_line);
                }
            }
        }

        if self.wrap {
            self.sync_wrapped_screen();
            return;
        }
        self.wrap_skip = 0;
        let cursor = self.line_display_width(&self.lines[self.row], self.col);
        let width = self.horizontal_width();
        if cursor < self.screen_left {
            self.screen_left = cursor;
        }
        if cursor >= self.screen_left.saturating_add(width) {
            self.screen_left = cursor - width + 1;
        }
        if self.col == 0 && self.lines[self.row].first() == Some(&b'\t') {
            self.screen_left = 0;
        }
    }

    /// Finishes [`Editor::sync_screen`] for `:set wrap`, where the line-based
    /// scrolling above can still leave the cursor line partly below the
    /// screen, and nothing ever scrolls sideways.
    pub(super) fn sync_wrapped_screen(&mut self) {
        self.screen_left = 0;
        let body = self.body_rows();
        if self.row != self.screen_top {
            self.wrap_skip = 0;
        }
        if self.row > self.last_visible_line() {
            self.screen_top = self.screen_top.max(self.wrapped_top_for(self.row, body));
        }
        if self.row != self.screen_top {
            self.wrap_skip = 0;
            return;
        }
        // Only a cursor line taller than the screen gets here with rows to
        // hide: scroll within it just far enough to show the cursor.
        let rows = self.line_rows(self.row, self.wrap_skip.saturating_add(body));
        self.wrap_skip = self.wrap_skip.min(rows.saturating_sub(body));
        let (segment, _) = self.cursor_wrapped_position();
        if segment < self.wrap_skip {
            self.wrap_skip = segment;
        } else if segment >= self.wrap_skip + body {
            self.wrap_skip = segment + 1 - body;
        }
    }

    pub(super) fn scroll_screen(&mut self, count: usize, direction: i32) {
        let last_line = self.lines.len().saturating_sub(1);
        // Scroll counts are screen rows. A wrapped line can take several.
        let count = if self.wrap {
            self.wrapped_scroll_lines(count, direction)
        } else {
            count
        };
        self.wrap_skip = 0;
        if direction < 0 {
            self.screen_top = self.screen_top.saturating_sub(count);
        } else {
            self.screen_top = self.screen_top.saturating_add(count).min(last_line);
        }
        let end = self.last_visible_line();
        self.row = self.row.clamp(self.screen_top, end);
        if direction > 0 && self.row == last_line {
            self.col = self.lines[self.row].len().saturating_sub(1);
        }
        while self.col < self.lines[self.row].len()
            && self.lines[self.row][self.col].is_ascii_whitespace()
        {
            self.col += 1;
        }
    }

    pub(super) fn edit_status(&self) -> String {
        let current = self.row + 1;
        let total = self.lines.len();
        let percent = current * 100 / total.max(1);
        let mode = match self.mode {
            Mode::Command => '-',
            Mode::Insert => 'I',
            Mode::Replace => 'R',
        };
        let name = self
            .filename
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "No file".to_owned());
        format!(
            "{} {}{}{} {}/{} {}%",
            mode,
            name,
            if self.readonly { " [Readonly]" } else { "" },
            if self.modified { " [Modified]" } else { "" },
            current,
            total,
            percent
        )
    }

    pub(super) fn render(&mut self, prompt: Option<&str>) -> io::Result<()> {
        let mut out = io::stdout().lock();
        self.render_to(&mut out, prompt)
    }

    pub(super) fn render_to<W: Write>(
        &mut self,
        out: &mut W,
        prompt: Option<&str>,
    ) -> io::Result<()> {
        self.sync_screen();
        let visible_range = (self.syntax_highlights_dirty || self.syntax_highlight_request_pending)
            .then(|| self.visible_byte_range());
        self.refresh_syntax_highlighting(false, visible_range);
        let body = self.body_rows();
        let width = self.horizontal_width();
        // Ask the embedding highlighter only once per frame. The optional
        // styles preserve the plain `:set nu` gutter when no highlighter is
        // installed, while allowing a syntax-aware editor to distinguish the
        // current line as Vim does.
        let (line_number_style, current_line_number_style) = if self.number {
            self.syntax_highlighter
                .as_ref()
                .map(|highlighter| {
                    (
                        highlighter.line_number_style(false),
                        highlighter.line_number_style(true),
                    )
                })
                .unwrap_or((None, None))
        } else {
            (None, None)
        };
        // Keep adjusted full-result spans visible outside the preview range,
        // while the preview cursor takes precedence within that range. The
        // two sorted span lists stay separate so scrolling never rebuilds or
        // sorts the full-buffer result.
        let syntax_enabled =
            !self.syntax_highlights.is_empty() || !self.syntax_preview_highlights.is_empty();
        let mut highlight_cursor = HighlightCursor::new(&self.syntax_highlights);
        let mut preview_cursor = HighlightCursor::new(&self.syntax_preview_highlights);
        let preview_range = self.syntax_preview_range;
        let mut frame = Vec::with_capacity(self.screen_rows);
        // With `:set wrap` a line spans one screen row per entry of
        // `starts`, of which this frame is drawing `segment`. Without it every
        // line is the single segment starting at `screen_left`.
        let mut index = self.screen_top;
        let mut segment = self.wrap_skip;
        let mut starts = if index < self.lines.len() {
            // One row past the screen, so the bottom row knows where it ends.
            self.row_starts(index, segment.saturating_add(body) + 1)
        } else {
            Vec::new()
        };
        for screen_line in 0..body {
            let mut row = Vec::new();
            if index >= self.lines.len() {
                row.extend_from_slice(b"~\x1b[K");
                frame.push(row);
                continue;
            }
            // Like Vim, mark a wrapped line that does not fit at the bottom
            // with `@` rather than show part of it.
            if segment == 0 && screen_line > 0 && screen_line + starts.len() > body {
                row.extend_from_slice(b"@\x1b[K");
                frame.push(row);
                continue;
            }
            let left = starts[segment];
            // Hanging blanks past the screen edge are not drawn.
            let row_width = starts
                .get(segment + 1)
                .map_or(width, |next| (next - left).min(width));
            if segment > 0 {
                row.resize(self.number_column_width(), b' ');
            } else if self.number {
                let style = if index == self.row {
                    current_line_number_style
                } else {
                    line_number_style
                };
                write_line_number(
                    &mut row,
                    index + 1,
                    self.number_column_width().saturating_sub(1),
                    style,
                )?;
            }
            let selected = self.selection_offsets_on_line(index);
            let columns = if syntax_enabled {
                write_highlighted_line_with_selection(
                    &mut row,
                    &self.lines[index],
                    self.syntax_line_offset(index),
                    &mut highlight_cursor,
                    &mut preview_cursor,
                    preview_range,
                    left,
                    row_width,
                    self.tabstop,
                    selected,
                )?
            } else {
                write_plain_line_with_selection(
                    &mut row,
                    &self.lines[index],
                    left,
                    row_width,
                    self.tabstop,
                    selected,
                )?
            };
            // A row that reaches the last column leaves the terminal's cursor
            // on that column, where erasing to the end of the line would wipe
            // its final character. There is nothing left to erase anyway.
            if self.number_column_width() + columns < self.screen_cols {
                row.extend_from_slice(b"\x1b[K");
            }
            frame.push(row);
            segment += 1;
            if segment >= starts.len() {
                index += 1;
                segment = 0;
                if index < self.lines.len() {
                    starts = self.row_starts(index, body + 1);
                }
            }
        }
        let mut status_row = Vec::new();
        status_row.extend_from_slice(b"\x1b[K");
        if let Some(prompt) = prompt {
            let visible = prompt.chars().take(self.screen_cols.saturating_sub(1));
            for character in visible {
                write!(status_row, "{}", character)?;
            }
        } else if !self.status.is_empty() {
            if self.status_highlighted {
                status_row.extend_from_slice(b"\x1b[7m");
            }
            status_row.write_all(&self.status_bytes)?;
            if self.status_highlighted {
                status_row.extend_from_slice(b"\x1b[m");
            }
        } else {
            for character in self
                .edit_status()
                .chars()
                .take(self.screen_cols.saturating_sub(1))
            {
                write!(status_row, "{}", character)?;
            }
        }
        frame.push(status_row);

        let size = (self.screen_rows, self.screen_cols);
        let full_redraw = self.rendered_size != size || self.rendered_rows.len() != frame.len();
        if full_redraw {
            out.write_all(b"\x1b[2J\x1b[H")?;
        }
        for (index, row) in frame.iter().enumerate() {
            if full_redraw || self.rendered_rows.get(index) != Some(row) {
                write!(out, "\x1b[{};1H", index + 1)?;
                write_rendered_row(out, row)?;
            }
        }
        self.rendered_rows = frame;
        self.rendered_size = size;

        let (cursor_row, cursor_col) = if let Some(prompt) = prompt {
            (
                self.screen_rows,
                prompt
                    .chars()
                    .count()
                    .min(self.screen_cols.saturating_sub(1))
                    + 1,
            )
        } else {
            let (screen_row, mut screen_col) = if self.wrap {
                // Each line above takes at least one row, so `body` lines are
                // enough to reach the bottom of the screen.
                let rows_above: usize = (self.screen_top..self.row)
                    .take(body)
                    .map(|line| self.line_rows(line, body))
                    .sum();
                let (segment, column) = self.cursor_wrapped_position();
                (
                    (rows_above + segment).saturating_sub(self.wrap_skip) + 1,
                    column,
                )
            } else {
                (
                    self.row.saturating_sub(self.screen_top) + 1,
                    self.line_display_width(&self.lines[self.row], self.col)
                        .saturating_sub(self.screen_left),
                )
            };
            screen_col += self.number_column_width();
            (
                screen_row.min(body).max(1),
                screen_col.min(self.screen_cols.saturating_sub(1)) + 1,
            )
        };
        write!(out, "\x1b[?25h\x1b[{};{}H", cursor_row, cursor_col)?;
        out.flush()
    }

    pub(super) fn prompt(
        &mut self,
        reader: &mut KeyReader,
        prefix: &str,
    ) -> io::Result<Option<Vec<u8>>> {
        let mut value = Vec::new();
        let mut prompt = prefix.to_owned();
        let buffered_only = reader.buffered_only;
        if !buffered_only {
            self.render(Some(&prompt))?;
        }
        loop {
            let Some(key) = reader.try_key()? else {
                if buffered_only {
                    return Ok(None);
                }
                let resized = reader.take_resized();
                if resized {
                    self.force_redraw();
                }
                if self.refresh_size() || resized {
                    self.render(Some(&prompt))?;
                }
                continue;
            };
            let _ = reader.take_special();
            if reader.take_resized() {
                self.force_redraw();
            }
            match key {
                b'\r' | b'\n' => return Ok(Some(value)),
                0x1b => return Ok(None),
                8 | 127 => {
                    let _ = value.pop();
                }
                byte if byte.is_ascii() && !byte.is_ascii_control() => value.push(byte),
                _ => {}
            }
            prompt = format!("{}{}", prefix, String::from_utf8_lossy(&value));
            if !buffered_only {
                self.render(Some(&prompt))?;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClipboardDestination {
    Clipboard,
    Primary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ClipboardHelperCommand {
    pub(super) program: &'static str,
    pub(super) args: &'static [&'static str],
}

pub(super) fn clipboard_helper_commands(
    destination: ClipboardDestination,
) -> Vec<ClipboardHelperCommand> {
    let mut commands = Vec::new();
    #[cfg(unix)]
    {
        match destination {
            ClipboardDestination::Clipboard => {
                commands.push(ClipboardHelperCommand {
                    program: "wl-copy",
                    args: &[],
                });
                commands.push(ClipboardHelperCommand {
                    program: "xclip",
                    args: &["-selection", "clipboard"],
                });
                commands.push(ClipboardHelperCommand {
                    program: "xsel",
                    args: &["--clipboard", "--input"],
                });
                #[cfg(target_os = "macos")]
                commands.push(ClipboardHelperCommand {
                    program: "pbcopy",
                    args: &[],
                });
            }
            ClipboardDestination::Primary => {
                commands.push(ClipboardHelperCommand {
                    program: "wl-copy",
                    args: &["--primary"],
                });
                commands.push(ClipboardHelperCommand {
                    program: "xclip",
                    args: &["-selection", "primary"],
                });
                commands.push(ClipboardHelperCommand {
                    program: "xsel",
                    args: &["--primary", "--input"],
                });
            }
        }
    }
    #[cfg(windows)]
    if destination == ClipboardDestination::Clipboard {
        commands.push(ClipboardHelperCommand {
            program: "clip.exe",
            args: &[],
        });
    }
    commands
}

pub(super) fn write_osc52_clipboard<W: Write>(out: &mut W, text: &[u8]) -> io::Result<()> {
    out.execute(CopyToClipboard::to_clipboard_from(text))?;
    #[cfg(unix)]
    out.execute(CopyToClipboard::to_primary_from(text))?;
    out.flush()
}

pub(super) fn run_clipboard_helper(command: ClipboardHelperCommand, text: &[u8]) -> bool {
    let Ok(mut child) = Command::new(command.program)
        .args(command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let wrote = child
        .stdin
        .take()
        .and_then(|mut stdin| stdin.write_all(text).ok())
        .is_some();
    if !wrote {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

pub(super) fn copy_selection_to_clipboards(text: &[u8]) {
    {
        let mut out = io::stdout().lock();
        let _ = write_osc52_clipboard(&mut out, text);
    }
    for destination in [
        ClipboardDestination::Clipboard,
        ClipboardDestination::Primary,
    ] {
        let commands = clipboard_helper_commands(destination);
        for command in commands {
            if run_clipboard_helper(command, text) {
                break;
            }
        }
        #[cfg(not(unix))]
        if destination == ClipboardDestination::Primary {
            break;
        }
    }
}

pub(super) struct HighlightCursor<'a> {
    spans: &'a [HighlightSpan],
    pub(super) index: usize,
}

impl<'a> HighlightCursor<'a> {
    pub(super) fn new(spans: &'a [HighlightSpan]) -> Self {
        Self { spans, index: 0 }
    }

    pub(super) fn style_at(&mut self, offset: usize) -> Option<HighlightStyle> {
        // HighlightSpan's sorted, non-overlapping contract means every span
        // preceding the viewport or current byte only needs to be visited once.
        while self
            .spans
            .get(self.index)
            .is_some_and(|span| span.end <= offset)
        {
            self.index += 1;
        }
        self.spans
            .get(self.index)
            .filter(|span| span.start <= offset && offset < span.end)
            .map(|span| span.style)
            .filter(|style| !style.is_plain())
    }
}

/// Vim's default `breakat`: the characters `:set linebreak` may wrap after.
pub(super) const BREAKAT: &[u8] = b" \t!@*-+;:,./?";

pub(super) fn fragment_len(byte: u8, display_column: usize, tabstop: usize) -> usize {
    if byte == b'\t' {
        let tabstop = tabstop.max(1);
        tabstop - (display_column % tabstop)
    } else if byte.is_ascii_control() {
        2
    } else {
        1
    }
}

pub(super) fn byte_offset_at_display_column(line: &[u8], target: usize, tabstop: usize) -> usize {
    let mut display_column: usize = 0;
    for (offset, byte) in line.iter().copied().enumerate() {
        let end = display_column.saturating_add(fragment_len(byte, display_column, tabstop));
        if target < end {
            return offset;
        }
        display_column = end;
    }
    line.len()
}

pub(super) fn write_fragment<W: Write>(
    out: &mut W,
    byte: u8,
    start: usize,
    stop: usize,
) -> io::Result<()> {
    if byte == b'\t' {
        const SPACES: &[u8; 32] = b"                                ";
        let mut remaining = stop - start;
        while remaining > 0 {
            let count = remaining.min(SPACES.len());
            out.write_all(&SPACES[..count])?;
            remaining -= count;
        }
    } else if byte.is_ascii_control() {
        out.write_all(&[b'^', byte ^ 0x40][start..stop])?;
    } else {
        out.write_all(&[byte][start..stop])?;
    }
    Ok(())
}

// Rust writes to a legacy Windows console through UTF-16 and rejects malformed
// UTF-8. A byte-oriented buffer can be clipped in the middle of a Unicode
// scalar before its ANSI erase sequence is appended, so make the final frame
// write valid UTF-8 only for that console path.
#[cfg(windows)]
pub(super) fn write_rendered_row<W: Write>(out: &mut W, row: &[u8]) -> io::Result<()> {
    let row = String::from_utf8_lossy(row);
    out.write_all(row.as_bytes())
}

#[cfg(not(windows))]
pub(super) fn write_rendered_row<W: Write>(out: &mut W, row: &[u8]) -> io::Result<()> {
    out.write_all(row)
}

pub(super) fn write_line_number<W: Write>(
    out: &mut W,
    line_number: usize,
    width: usize,
    style: Option<HighlightStyle>,
) -> io::Result<()> {
    let style = style.filter(|style| !style.is_plain());
    if let Some(style) = style {
        style.write_sgr(out)?;
    }
    write!(out, "{line_number:>width$} ")?;
    if style.is_some() {
        out.write_all(b"\x1b[0m")?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn write_plain_line<W: Write>(
    out: &mut W,
    line: &[u8],
    screen_left: usize,
    width: usize,
    tabstop: usize,
) -> io::Result<()> {
    write_plain_line_with_selection(out, line, screen_left, width, tabstop, None).map(|_| ())
}

pub(super) fn write_plain_line_with_selection<W: Write>(
    out: &mut W,
    line: &[u8],
    screen_left: usize,
    width: usize,
    tabstop: usize,
    selection: Option<(usize, usize)>,
) -> io::Result<usize> {
    let visible_end = screen_left.saturating_add(width);
    let mut display_column = 0;
    let mut selected = false;
    let mut columns = 0;

    for (source_offset, byte) in line.iter().copied().enumerate() {
        let length = fragment_len(byte, display_column, tabstop);
        let fragment_end = display_column + length;
        let visible_start = screen_left.max(display_column);
        let visible_stop = visible_end.min(fragment_end);
        if visible_start < visible_stop {
            let next_selected =
                selection.is_some_and(|(start, end)| start <= source_offset && source_offset < end);
            if next_selected != selected {
                if selected {
                    out.write_all(b"\x1b[0m")?;
                }
                if next_selected {
                    out.write_all(b"\x1b[7m")?;
                }
                selected = next_selected;
            }
            write_fragment(
                out,
                byte,
                visible_start - display_column,
                visible_stop - display_column,
            )?;
            columns += visible_stop - visible_start;
        }
        display_column = fragment_end;
        if display_column >= visible_end {
            break;
        }
    }
    if selected {
        out.write_all(b"\x1b[0m")?;
    }
    Ok(columns)
}

#[cfg(test)]
pub(super) fn write_highlighted_line<W: Write>(
    out: &mut W,
    line: &[u8],
    line_start: usize,
    highlights: &mut HighlightCursor<'_>,
    screen_left: usize,
    width: usize,
    tabstop: usize,
) -> io::Result<()> {
    let mut preview = HighlightCursor::new(&[]);
    write_highlighted_line_with_preview(
        out,
        line,
        line_start,
        highlights,
        &mut preview,
        None,
        screen_left,
        width,
        tabstop,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn write_highlighted_line_with_preview<W: Write>(
    out: &mut W,
    line: &[u8],
    line_start: usize,
    highlights: &mut HighlightCursor<'_>,
    preview: &mut HighlightCursor<'_>,
    preview_range: Option<(usize, usize)>,
    screen_left: usize,
    width: usize,
    tabstop: usize,
) -> io::Result<()> {
    write_highlighted_line_with_selection(
        out,
        line,
        line_start,
        highlights,
        preview,
        preview_range,
        screen_left,
        width,
        tabstop,
        None,
    )
    .map(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write_highlighted_line_with_selection<W: Write>(
    out: &mut W,
    line: &[u8],
    line_start: usize,
    highlights: &mut HighlightCursor<'_>,
    preview: &mut HighlightCursor<'_>,
    preview_range: Option<(usize, usize)>,
    screen_left: usize,
    width: usize,
    tabstop: usize,
    selection: Option<(usize, usize)>,
) -> io::Result<usize> {
    let visible_end = screen_left.saturating_add(width);
    let mut display_column = 0;
    let mut active_style = None;
    let mut active_selection = false;
    let mut columns = 0;

    for (source_offset, byte) in line.iter().copied().enumerate() {
        let length = fragment_len(byte, display_column, tabstop);
        let fragment_end = display_column + length;
        let visible_start = screen_left.max(display_column);
        let visible_stop = visible_end.min(fragment_end);
        if visible_start < visible_stop {
            let offset = line_start + source_offset;
            let style = if preview_range.is_some_and(|(start, end)| start <= offset && offset < end)
            {
                // An empty preview span list intentionally masks stale full
                // spans for the visible bytes too.
                preview.style_at(offset)
            } else {
                highlights.style_at(offset)
            };
            let selected =
                selection.is_some_and(|(start, end)| start <= source_offset && source_offset < end);
            if style != active_style || selected != active_selection {
                if active_style.is_some() || active_selection {
                    write!(out, "\x1b[0m")?;
                }
                if let Some(style) = style {
                    style.write_sgr(out)?;
                }
                if selected {
                    write!(out, "\x1b[7m")?;
                }
                active_style = style;
                active_selection = selected;
            }
            let start = visible_start - display_column;
            let stop = visible_stop - display_column;
            write_fragment(out, byte, start, stop)?;
            columns += stop - start;
        }
        display_column = fragment_end;
        if display_column >= visible_end {
            break;
        }
    }
    if active_style.is_some() || active_selection {
        write!(out, "\x1b[0m")?;
    }
    Ok(columns)
}
