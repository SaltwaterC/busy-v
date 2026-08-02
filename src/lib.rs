//! A small, dependency-free vi editor core.
//!
//! The editor owns its text and never exposes pointers into the buffer.  This
//! is intentionally byte-oriented (like the original BusyBox editor), while
//! displaying non-UTF-8 input lossily at the terminal.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, ExecutableCommand};

#[cfg(unix)]
mod platform_user {
    pub(crate) fn current_uid() -> Option<u32> {
        // SAFETY: libc provides the platform-correct ABI for getuid, which
        // takes no pointers and returns a plain user ID.
        Some(unsafe { libc::getuid() as u32 })
    }
}
const HELP: &str = "These features are available:\n\
\tPattern searches with / and ?\n\
\tLast command repeat with .\n\
\tLine marking with 'x\n\
\tNamed buffers with \"x\n\
\tSome colon mode commands with :\n\
\tSettable options with \":set\"\n\
\tSignal catching- ^C\n\
\tJob suspend and resume with ^Z\n\
\tAdapt to window re-sizes\n";

#[derive(Clone)]
struct Snapshot {
    lines: Vec<Vec<u8>>,
    row: usize,
    col: usize,
    trailing_newline: bool,
    modified: bool,
}

#[derive(Clone)]
struct Register {
    lines: Vec<Vec<u8>>,
    linewise: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Command,
    Insert,
    Replace,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operator {
    Delete,
    Yank,
    Change,
}

struct Terminal {
    active: bool,
}

impl Terminal {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        if let Err(error) = out.execute(EnterAlternateScreen) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        Ok(Self { active: true })
    }

    fn interactive(&self) -> bool {
        self.active
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.active {
            let mut out = io::stdout();
            let _ = execute!(out, LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
        }
    }
}

struct KeyReader {
    input: io::Stdin,
    pending: VecDeque<u8>,
    timed_input: bool,
    special: bool,
    events: bool,
}

impl KeyReader {
    fn new(timed_input: bool) -> Self {
        Self {
            input: io::stdin(),
            pending: VecDeque::new(),
            timed_input,
            special: false,
            events: timed_input,
        }
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            input: io::stdin(),
            pending: bytes.iter().copied().collect(),
            timed_input: false,
            special: false,
            events: false,
        }
    }

    fn take_special(&mut self) -> bool {
        let special = self.special;
        self.special = false;
        special
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn read_more(&mut self) -> io::Result<bool> {
        let mut bytes = [0u8; 64];
        let size = self.input.read(&mut bytes)?;
        if size == 0 {
            return Ok(false);
        }
        self.pending.extend(bytes[..size].iter().copied());
        Ok(true)
    }

    fn try_byte(&mut self) -> io::Result<Option<u8>> {
        if let Some(byte) = self.pending.pop_front() {
            return Ok(Some(byte));
        }
        if !self.read_more()? {
            if self.timed_input {
                return Ok(None);
            }
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "input closed"));
        }
        Ok(self.pending.pop_front())
    }

    fn timed_byte(&mut self) -> io::Result<Option<u8>> {
        if let Some(byte) = self.pending.pop_front() {
            return Ok(Some(byte));
        }
        if self.read_more()? {
            Ok(self.pending.pop_front())
        } else {
            Ok(None)
        }
    }

    fn try_key(&mut self) -> io::Result<Option<u8>> {
        if self.events {
            return self.try_event_key(false);
        }
        self.special = false;
        let Some(byte) = self.try_byte()? else {
            return Ok(None);
        };
        if byte != 0x1b {
            return Ok(Some(byte));
        }
        let Some(next) = self.timed_byte()? else {
            return Ok(Some(0x1b));
        };
        if next != b'[' && next != b'O' {
            self.pending.push_front(next);
            return Ok(Some(0x1b));
        }
        let mut sequence = Vec::new();
        let final_byte = loop {
            let Some(value) = self.timed_byte()? else {
                return Ok(Some(0x1b));
            };
            sequence.push(value);
            if (0x40..=0x7e).contains(&value) {
                break value;
            }
            if sequence.len() >= 32 {
                return Ok(Some(0x1b));
            }
        };
        let key = match final_byte {
            b'A' => 0x80,
            b'B' => 0x81,
            b'C' => 0x82,
            b'D' => 0x83,
            b'H' => 0x84,
            b'F' => 0x85,
            b'~' if matches!(sequence.first(), Some(b'1') | Some(b'7')) => 0x84,
            b'~' if matches!(sequence.first(), Some(b'4') | Some(b'8')) => 0x85,
            b'~' if sequence.first() == Some(&b'2') => 0x86,
            b'~' if sequence.first() == Some(&b'3') => 0x87,
            b'~' if sequence.first() == Some(&b'5') => 0x88,
            b'~' if sequence.first() == Some(&b'6') => 0x89,
            _ => 0x1b,
        };
        self.special = (0x80..=0x89).contains(&key);
        Ok(Some(key))
    }

    fn key(&mut self) -> io::Result<u8> {
        if self.events {
            loop {
                if let Some(key) = self.try_event_key(true)? {
                    return Ok(key);
                }
            }
        }
        loop {
            if let Some(key) = self.try_key()? {
                return Ok(key);
            }
        }
    }

    fn try_event_key(&mut self, blocking: bool) -> io::Result<Option<u8>> {
        self.special = false;
        loop {
            if !blocking && !event::poll(std::time::Duration::from_millis(50))? {
                return Ok(None);
            }
            match event::read()? {
                Event::Key(key) => {
                    if let Some(value) = self.push_event_key(key) {
                        return Ok(Some(value));
                    }
                    if !blocking {
                        return Ok(None);
                    }
                }
                Event::Resize(_, _) => return Ok(None),
                _ if !blocking => return Ok(None),
                _ => {}
            }
        }
    }

    fn push_event_key(&mut self, key: KeyEvent) -> Option<u8> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let value = match key.code {
            KeyCode::Up => {
                self.special = true;
                0x80
            }
            KeyCode::Down => {
                self.special = true;
                0x81
            }
            KeyCode::Right => {
                self.special = true;
                0x82
            }
            KeyCode::Left => {
                self.special = true;
                0x83
            }
            KeyCode::Home => {
                self.special = true;
                0x84
            }
            KeyCode::End => {
                self.special = true;
                0x85
            }
            KeyCode::Insert => {
                self.special = true;
                0x86
            }
            KeyCode::Delete => {
                self.special = true;
                0x87
            }
            KeyCode::PageUp => {
                self.special = true;
                0x88
            }
            KeyCode::PageDown => {
                self.special = true;
                0x89
            }
            KeyCode::Backspace => 8,
            KeyCode::Enter => b'\r',
            KeyCode::Tab => b'\t',
            KeyCode::Esc => 0x1b,
            KeyCode::Char(character) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && character.is_ascii() {
                    (character.to_ascii_uppercase() as u8) & 0x1f
                } else {
                    let mut encoded = [0u8; 4];
                    let encoded = character.encode_utf8(&mut encoded);
                    self.pending.extend(encoded.bytes());
                    return self.pending.pop_front();
                }
            }
            _ => return None,
        };
        Some(value)
    }
}

pub struct Editor {
    lines: Vec<Vec<u8>>,
    row: usize,
    col: usize,
    filename: Option<PathBuf>,
    alternate_filename: Option<PathBuf>,
    modified: bool,
    readonly: bool,
    trailing_newline: bool,
    mode: Mode,
    number: bool,
    autoindent: bool,
    expandtab: bool,
    flash: bool,
    ignorecase: bool,
    showmatch: bool,
    tabstop: usize,
    status: String,
    status_bytes: Vec<u8>,
    status_highlighted: bool,
    hit_return: bool,
    yank: Vec<Vec<u8>>,
    yank_linewise: bool,
    registers: HashMap<u8, Register>,
    selected_register: Option<u8>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    change_open: bool,
    replaying: bool,
    last_change: Vec<u8>,
    search: Option<(Vec<u8>, i32)>,
    char_search: Option<(u8, i32, bool)>,
    marks: [Option<(usize, usize)>; 26],
    quit: bool,
    force_quit: bool,
    next_file: bool,
    previous_file: bool,
    rewind_files: bool,
    file_index: usize,
    file_count: usize,
    screen_rows: usize,
    screen_cols: usize,
    screen_top: usize,
    screen_left: usize,
}

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
            autoindent: false,
            expandtab: false,
            flash: false,
            ignorecase: false,
            showmatch: false,
            tabstop: 8,
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
            change_open: false,
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
            marks: [None; 26],
        };
        editor.set_bytes(data);
        editor
    }

    /// Return the current buffer in the same newline form used by `:write`.
    pub fn bytes(&self) -> Vec<u8> {
        let mut data = Vec::new();
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

    fn clear_status(&mut self) {
        self.status.clear();
        self.status_bytes.clear();
        self.status_highlighted = false;
    }

    fn set_status(&mut self, status: impl Into<String>) {
        let status = status.into();
        self.status = status.clone();
        self.status_bytes = status.into_bytes();
        self.status_highlighted = false;
    }

    fn set_error(&mut self, status: impl Into<String>) {
        let status = status.into();
        self.status = status.clone();
        self.status_bytes = status.into_bytes();
        self.status_highlighted = true;
    }

    fn set_status_bytes(&mut self, status: Vec<u8>) {
        self.status = String::from_utf8_lossy(&status).into_owned();
        self.status_bytes = status;
        self.status_highlighted = false;
    }

    fn set_file_context(&mut self, index: usize, count: usize) {
        self.file_index = index;
        self.file_count = count.max(1);
    }

    fn remaining_files(&self) -> usize {
        self.file_count.saturating_sub(self.file_index + 1)
    }

    fn finish_write_command(&mut self, command: &str, force: bool) {
        if command == "wn" {
            self.quit = true;
            self.next_file = true;
        } else if self.remaining_files() > 0 && !force {
            self.set_error(format!("{} more file(s) to edit", self.remaining_files()));
        } else {
            self.quit = true;
        }
    }

    fn finish_zz_command(&mut self) {
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

    fn set_bytes(&mut self, data: &[u8]) {
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
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            row: self.row,
            col: self.col,
            trailing_newline: self.trailing_newline,
            modified: self.modified,
        }
    }

    fn undo_status(&self, previous: &Snapshot) -> String {
        let before = snapshot_bytes(previous);
        let after = self.bytes();
        let prefix = before
            .iter()
            .zip(&after)
            .take_while(|(left, right)| left == right)
            .count();
        let common_tail = before[prefix..]
            .iter()
            .rev()
            .zip(after[prefix..].iter().rev())
            .take_while(|(left, right)| left == right)
            .count()
            .min(before.len().saturating_sub(prefix))
            .min(after.len().saturating_sub(prefix));
        let before_changed = before.len().saturating_sub(prefix + common_tail);
        let after_changed = after.len().saturating_sub(prefix + common_tail);
        let (verb, chars) = if before_changed > after_changed {
            ("restored", before_changed)
        } else if after_changed > before_changed {
            ("deleted", after_changed)
        } else {
            ("restored", before_changed.max(1))
        };
        format!(
            "Undo [{}] {} {} chars at position {}",
            self.undo.len() + 1,
            verb,
            chars,
            prefix
        )
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.lines = snapshot.lines;
        self.row = snapshot.row.min(self.lines.len().saturating_sub(1));
        self.col = snapshot.col.min(self.lines[self.row].len());
        self.trailing_newline = snapshot.trailing_newline;
        self.modified = snapshot.modified;
    }

    fn begin_change(&mut self) {
        if !self.change_open {
            self.undo.push(self.snapshot());
            self.redo.clear();
            self.change_open = true;
        }
        self.modified = true;
    }

    fn end_change(&mut self) {
        self.change_open = false;
    }

    fn load_current(&mut self) -> io::Result<bool> {
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
                self.lines = vec![Vec::new()];
                self.trailing_newline = true;
                self.clear_status();
                new_file = true;
            }
        }
        self.row = 0;
        self.col = 0;
        self.modified = false;
        Ok(new_file)
    }

    fn update_readonly(&mut self, path: &Path) {
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

    fn current_file_status(&self, new_file: bool, show_readonly: bool) -> String {
        let name = self
            .filename
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let data = self.bytes();
        format_file_status(&name, &data, new_file, show_readonly && self.readonly)
    }

    fn expand_filename(&self, argument: &str) -> io::Result<PathBuf> {
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

    fn write_file(&mut self, requested: Option<&Path>, force: bool) -> io::Result<()> {
        self.write_file_range(requested, force, None)
    }

    fn write_file_range(
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

    fn line_display_width(&self, line: &[u8], upto: usize) -> usize {
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

    fn refresh_size(&mut self) -> bool {
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

    fn body_rows(&self) -> usize {
        self.screen_rows.saturating_sub(1).max(1)
    }

    fn horizontal_width(&self) -> usize {
        self.screen_cols
            .saturating_sub(self.number_column_width())
            .max(1)
    }

    fn number_column_width(&self) -> usize {
        if self.number {
            self.lines.len().to_string().len().max(3) + 1
        } else {
            0
        }
    }

    fn sync_screen(&mut self) {
        let last_line = self.lines.len().saturating_sub(1);
        let body = self.body_rows();
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

    fn scroll_screen(&mut self, count: usize, direction: i32) {
        let last_line = self.lines.len().saturating_sub(1);
        if direction < 0 {
            self.screen_top = self.screen_top.saturating_sub(count);
        } else {
            self.screen_top = self.screen_top.saturating_add(count).min(last_line);
        }
        let end = self
            .screen_top
            .saturating_add(self.body_rows().saturating_sub(1))
            .min(last_line);
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

    fn formatted_line(&self, line: &[u8]) -> Vec<u8> {
        let mut formatted = Vec::new();
        let mut width = 0;
        for byte in String::from_utf8_lossy(line).bytes() {
            if byte == b'\t' {
                let count = self.tabstop - (width % self.tabstop);
                formatted.extend(std::iter::repeat_n(b' ', count));
                width += count;
            } else if byte.is_ascii_control() {
                formatted.extend([b'^', byte ^ 0x40]);
                width += 2;
            } else {
                formatted.push(byte);
                width += 1;
            }
        }
        formatted
    }

    fn edit_status(&self) -> String {
        let current = self.row + 1;
        let total = self.lines.len();
        let percent = if total == 0 {
            100
        } else {
            current * 100 / total
        };
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

    fn render(&mut self, prompt: Option<&str>) -> io::Result<()> {
        self.sync_screen();
        let mut out = io::stdout().lock();
        write!(out, "\x1b[2J\x1b[H")?;
        let body = self.body_rows();
        let width = self.horizontal_width();
        for screen_line in 0..body {
            let index = self.screen_top + screen_line;
            write!(out, "\x1b[{};1H", screen_line + 1)?;
            if index >= self.lines.len() {
                write!(out, "~\x1b[K")?;
                continue;
            }
            let prefix = if self.number {
                let width = self.number_column_width().saturating_sub(1);
                format!("{:>width$} ", index + 1, width = width).into_bytes()
            } else {
                Vec::new()
            };
            out.write_all(&prefix)?;
            let formatted = self.formatted_line(&self.lines[index]);
            let start = self.screen_left.min(formatted.len());
            let end = start.saturating_add(width).min(formatted.len());
            out.write_all(&formatted[start..end])?;
            write!(out, "\x1b[K")?;
        }
        write!(out, "\x1b[{};1H\x1b[K", self.screen_rows)?;
        if let Some(prompt) = prompt {
            let visible = prompt.chars().take(self.screen_cols.saturating_sub(1));
            for character in visible {
                write!(out, "{}", character)?;
            }
        } else if !self.status.is_empty() {
            if self.status_highlighted {
                write!(out, "\x1b[7m")?;
            }
            out.write_all(&self.status_bytes)?;
            if self.status_highlighted {
                write!(out, "\x1b[m")?;
            }
        } else {
            for character in self
                .edit_status()
                .chars()
                .take(self.screen_cols.saturating_sub(1))
            {
                write!(out, "{}", character)?;
            }
        }
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
            let screen_row = self.row.saturating_sub(self.screen_top) + 1;
            let mut screen_col = self
                .line_display_width(&self.lines[self.row], self.col)
                .saturating_sub(self.screen_left);
            screen_col += self.number_column_width();
            (
                screen_row.min(body).max(1),
                screen_col.min(self.screen_cols.saturating_sub(1)) + 1,
            )
        };
        write!(out, "\x1b[?25h\x1b[{};{}H", cursor_row, cursor_col)?;
        out.flush()
    }

    fn prompt(&mut self, reader: &mut KeyReader, prefix: &str) -> io::Result<Option<Vec<u8>>> {
        let mut value = Vec::new();
        let mut prompt = prefix.to_owned();
        self.render(Some(&prompt))?;
        loop {
            let Some(key) = reader.try_key()? else {
                if self.refresh_size() {
                    self.render(Some(&prompt))?;
                }
                continue;
            };
            let _ = reader.take_special();
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
            self.render(Some(&prompt))?;
        }
    }

    fn delete_lines(&mut self, start: usize, end: usize) {
        self.begin_change();
        let end = end.min(self.lines.len().saturating_sub(1));
        if start <= end {
            self.yank = self.lines[start..=end].to_vec();
            self.yank_linewise = true;
            self.lines.drain(start..=end);
        }
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
        self.row = start.min(self.lines.len() - 1);
        self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
    }

    fn motion(&self, command: u8, count: usize) -> (usize, usize) {
        let mut row = self.row;
        let mut col = self.col;
        match command {
            b'h' | 0x83 => col = col.saturating_sub(count),
            b'l' | 0x82 => col = (col + count).min(self.lines[row].len().saturating_sub(1)),
            b'j' | 0x81 => row = (row + count).min(self.lines.len() - 1),
            b'k' | 0x80 => row = row.saturating_sub(count),
            b'0' | 0x84 => col = 0,
            b'^' => {
                col = self.lines[row]
                    .iter()
                    .position(|byte| !byte.is_ascii_whitespace())
                    .unwrap_or(0);
            }
            b'$' | 0x85 => col = self.lines[row].len().saturating_sub(1),
            b'w' => {
                for _ in 0..count {
                    while col + 1 < self.lines[row].len()
                        && !self.lines[row][col].is_ascii_whitespace()
                    {
                        col += 1;
                    }
                    while col + 1 < self.lines[row].len()
                        && self.lines[row][col].is_ascii_whitespace()
                    {
                        col += 1;
                    }
                }
            }
            b'W' => {
                for _ in 0..count {
                    while col + 1 < self.lines[row].len()
                        && !self.lines[row][col].is_ascii_whitespace()
                    {
                        col += 1;
                    }
                    while col + 1 < self.lines[row].len()
                        && self.lines[row][col].is_ascii_whitespace()
                    {
                        col += 1;
                    }
                }
            }
            b'e' => {
                for _ in 0..count {
                    while col + 1 < self.lines[row].len()
                        && self.lines[row][col].is_ascii_whitespace()
                    {
                        col += 1;
                    }
                    while col + 1 < self.lines[row].len()
                        && !self.lines[row][col + 1].is_ascii_whitespace()
                    {
                        col += 1;
                    }
                }
            }
            b'E' => {
                for _ in 0..count {
                    while col + 1 < self.lines[row].len()
                        && self.lines[row][col].is_ascii_whitespace()
                    {
                        col += 1;
                    }
                    while col + 1 < self.lines[row].len()
                        && !self.lines[row][col + 1].is_ascii_whitespace()
                    {
                        col += 1;
                    }
                }
            }
            b'b' => {
                for _ in 0..count {
                    col = col.saturating_sub(1);
                    while col > 0 && self.lines[row][col].is_ascii_whitespace() {
                        col -= 1;
                    }
                    while col > 0 && !self.lines[row][col - 1].is_ascii_whitespace() {
                        col -= 1;
                    }
                }
            }
            b'B' => {
                for _ in 0..count {
                    col = col.saturating_sub(1);
                    while col > 0 && self.lines[row][col].is_ascii_whitespace() {
                        col -= 1;
                    }
                    while col > 0 && !self.lines[row][col - 1].is_ascii_whitespace() {
                        col -= 1;
                    }
                }
            }
            _ => {}
        }
        (row, col.min(self.lines[row].len().saturating_sub(1)))
    }

    fn put(&mut self, after: bool, register: Option<u8>) {
        if self.yank.is_empty() {
            self.set_error(format!("Nothing in register {}", register_name(register)));
            return;
        }
        let status = register_status(
            "Put",
            &self.yank,
            self.yank_linewise,
            1,
            register_name(register),
        );
        self.begin_change();
        let at = if after { self.row + 1 } else { self.row };
        if self.yank_linewise {
            for (offset, line) in self.yank.clone().into_iter().enumerate() {
                self.lines.insert((at + offset).min(self.lines.len()), line);
            }
            self.row = at.min(self.lines.len() - 1);
            self.col = 0;
        } else {
            let col = if after { self.col + 1 } else { self.col };
            let line = &mut self.lines[self.row];
            line.splice(
                col.min(line.len())..col.min(line.len()),
                self.yank[0].iter().copied(),
            );
            self.col = col.min(line.len().saturating_sub(1));
        }
        self.set_status(status);
    }

    fn save_register(&mut self, register: Option<u8>) {
        if let Some(name) = register {
            self.registers.insert(
                name,
                Register {
                    lines: self.yank.clone(),
                    linewise: self.yank_linewise,
                },
            );
        }
    }

    fn search_from(&mut self, pattern: &[u8], direction: i32) -> Option<bool> {
        if pattern.is_empty() {
            return None;
        }
        if direction > 0 {
            if let Some(col) = self.find_pattern(&self.lines[self.row], pattern, self.col + 1) {
                self.col = col;
                return Some(false);
            }
        } else if let Some(col) =
            self.find_pattern_before(&self.lines[self.row], pattern, self.col.saturating_sub(1))
        {
            self.col = col;
            return Some(false);
        }
        let mut row = self.row as i32;
        for _ in 0..self.lines.len() {
            row = (row + direction).rem_euclid(self.lines.len() as i32);
            if self
                .find_pattern(&self.lines[row as usize], pattern, 0)
                .is_some()
            {
                self.row = row as usize;
                self.col = self
                    .find_pattern(&self.lines[self.row], pattern, 0)
                    .unwrap_or(0);
                return Some(true);
            }
        }
        None
    }

    fn report_search(&mut self, result: Option<bool>, direction: i32) {
        match result {
            Some(true) => self.set_error(if direction > 0 {
                "search hit BOTTOM, continuing at TOP"
            } else {
                "search hit TOP, continuing at BOTTOM"
            }),
            Some(false) => {}
            None => self.set_error("Pattern not found"),
        }
    }

    fn find_pattern(&self, text: &[u8], pattern: &[u8], from: usize) -> Option<usize> {
        if self.ignorecase {
            let folded_text = fold_ascii(text);
            let folded_pattern = fold_ascii(pattern);
            find_pattern(&folded_text, &folded_pattern, from)
        } else {
            find_pattern(text, pattern, from)
        }
    }

    fn find_pattern_before(&self, text: &[u8], pattern: &[u8], before: usize) -> Option<usize> {
        if self.ignorecase {
            let folded_text = fold_ascii(text);
            let folded_pattern = fold_ascii(pattern);
            find_pattern_before(&folded_text, &folded_pattern, before)
        } else {
            find_pattern_before(text, pattern, before)
        }
    }

    fn find_character(&self, target: u8, direction: i32, till: bool) -> Option<usize> {
        let line = &self.lines[self.row];
        if direction > 0 {
            let found = ((self.col + 1)..line.len()).find(|index| line[*index] == target)?;
            Some(if till { found.saturating_sub(1) } else { found })
        } else {
            let found = (0..self.col).rev().find(|index| line[*index] == target)?;
            Some(if till {
                (found + 1).min(line.len().saturating_sub(1))
            } else {
                found
            })
        }
    }

    fn matching_delimiter(&self) -> Option<usize> {
        let line = &self.lines[self.row];
        let current = *line.get(self.col)?;
        let (open, close, direction) = match current {
            b'(' => (b'(', b')', 1),
            b'[' => (b'[', b']', 1),
            b'{' => (b'{', b'}', 1),
            b')' => (b'(', b')', -1),
            b']' => (b'[', b']', -1),
            b'}' => (b'{', b'}', -1),
            _ => return None,
        };
        let mut depth = 0i32;
        if direction > 0 {
            for (index, byte) in line.iter().enumerate().skip(self.col) {
                if *byte == open {
                    depth += 1;
                } else if *byte == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
            }
        } else {
            for (index, byte) in line.iter().enumerate().take(self.col + 1).rev() {
                if *byte == close {
                    depth += 1;
                } else if *byte == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
            }
        }
        None
    }

    fn read_motion(
        &mut self,
        reader: &mut KeyReader,
        command: u8,
        count: usize,
    ) -> io::Result<(usize, usize)> {
        match command {
            b'f' | b'F' | b't' | b'T' => {
                let target = reader.key()?;
                let direction = if matches!(command, b'f' | b't') {
                    1
                } else {
                    -1
                };
                let till = matches!(command, b't' | b'T');
                self.char_search = Some((target, direction, till));
                Ok((
                    self.row,
                    self.find_character(target, direction, till)
                        .unwrap_or(self.col),
                ))
            }
            b'%' => Ok((self.row, self.matching_delimiter().unwrap_or(self.col))),
            b'G' => Ok((
                if count == 1 {
                    self.lines.len() - 1
                } else {
                    (count - 1).min(self.lines.len() - 1)
                },
                0,
            )),
            b'g' => {
                if reader.key()? == b'g' {
                    Ok((0, 0))
                } else {
                    Ok((self.row, self.col))
                }
            }
            _ => Ok(self.motion(command, count)),
        }
    }

    fn command_prompt(&mut self, reader: &mut KeyReader) -> io::Result<()> {
        let Some(raw) = self.prompt(reader, ":")? else {
            return Ok(());
        };
        let command = String::from_utf8_lossy(&raw);
        self.execute_ex(&command);
        Ok(())
    }

    fn no_write_since_last_change(&mut self, command: &str) {
        self.set_error(format!(
            "No write since last change (:{}! overrides)",
            command
        ));
    }

    pub fn execute_ex(&mut self, command: &str) {
        let command = command.trim_start_matches(':').trim();
        if command.is_empty() {
            return;
        }
        self.clear_status();
        let selected_register = self.selected_register.take();
        let (force, body) = command
            .strip_suffix('!')
            .map(|s| (true, s))
            .unwrap_or((false, command));
        let (addresses, name, parsed_args) = match parse_ex(
            body,
            self.row,
            self.lines.len(),
            &self.lines,
            self.ignorecase,
            &self.marks,
            &mut self.search,
        ) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.set_error(error);
                return;
            }
        };
        let (force, args) = if let Some(rest) = parsed_args.strip_prefix('!') {
            (true, rest.trim_start())
        } else {
            (force, parsed_args)
        };
        match name {
            name if command_prefix(name, "quit") => {
                if self.modified && !force {
                    self.no_write_since_last_change(name);
                } else {
                    if self.remaining_files() > 0 && !force {
                        self.set_error(format!("{} more file(s) to edit", self.remaining_files()));
                    } else {
                        self.quit = true;
                        self.force_quit = force;
                    }
                }
            }
            name if command_prefix(name, "next")
                || command_prefix(name, "prev")
                || command_prefix(name, "rewind") =>
            {
                let is_next = command_prefix(name, "next");
                let is_previous = command_prefix(name, "prev");
                if self.modified && !force {
                    self.no_write_since_last_change(name);
                } else if !force && is_next && self.remaining_files() == 0 {
                    self.set_error("No more files to edit");
                } else if !force && is_previous && self.file_index == 0 {
                    self.set_error("No previous files to edit");
                } else {
                    self.quit = true;
                    self.next_file = is_next;
                    self.previous_file = is_previous;
                    self.rewind_files = command_prefix(name, "rewind");
                    self.force_quit = force;
                }
            }
            name if command_prefix(name, "write") => {
                let expanded = if args.is_empty() {
                    Ok(None)
                } else {
                    self.expand_filename(args).map(Some)
                };
                match expanded {
                    Ok(path) => match self.write_file_range(path.as_deref(), force, addresses) {
                        Ok(()) => {}
                        Err(e) => self.set_error(e.to_string()),
                    },
                    Err(e) => self.set_error(e.to_string()),
                }
            }
            "wq" | "x" | "wn" => {
                let expanded = if args.is_empty() {
                    Ok(None)
                } else {
                    self.expand_filename(args).map(Some)
                };
                match expanded {
                    Ok(path) => match self.write_file_range(path.as_deref(), force, addresses) {
                        Ok(()) => self.finish_write_command(name, force),
                        Err(e) => self.set_error(e.to_string()),
                    },
                    Err(e) => self.set_error(e.to_string()),
                }
            }
            name if command_prefix(name, "edit") => {
                if self.modified && !force {
                    self.no_write_since_last_change(name);
                } else {
                    if !args.is_empty() {
                        self.alternate_filename = self.filename.clone();
                        match self.expand_filename(args) {
                            Ok(path) => self.filename = Some(path),
                            Err(e) => {
                                self.set_error(e.to_string());
                                self.end_change();
                                return;
                            }
                        }
                    }
                    if self.filename.is_none() {
                        self.set_error("No current filename");
                        self.end_change();
                        return;
                    }
                    match self.load_current() {
                        Ok(new_file) => {
                            self.set_status(self.current_file_status(new_file, true));
                        }
                        Err(e) => self.set_error(format!(
                            "'{}' {}",
                            self.filename
                                .as_deref()
                                .map(|path| path.display().to_string())
                                .unwrap_or_default(),
                            e
                        )),
                    }
                }
            }
            name if name.len() > 1 && command_prefix(name, "set") => self.set_options(args),
            name if command_prefix(name, "file") => {
                if addresses.is_some() {
                    self.set_error("No address allowed on this command");
                    self.end_change();
                    return;
                }
                if !args.is_empty() {
                    self.alternate_filename = self.filename.clone();
                    match self.expand_filename(args) {
                        Ok(path) => self.filename = Some(path),
                        Err(e) => {
                            self.set_error(e.to_string());
                            self.end_change();
                            return;
                        }
                    }
                }
                self.clear_status();
            }
            "" if args == "=" => {
                let line = if is_bare_zero_address(body) {
                    0
                } else {
                    addresses.map(|(_, end)| end + 1).unwrap_or(self.row + 1)
                };
                self.set_status(line.to_string());
            }
            "" => {
                if let Some((start, _)) = addresses {
                    self.row = start.min(self.lines.len() - 1);
                    self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
                }
            }
            name if command_prefix(name, "version") => self.set_status("standalone"),
            name if command_prefix(name, "features") => {
                self.hit_return = true;
                self.set_status(format!(
                    "{}\x1b[7m[Hit return to continue]\x1b[m",
                    HELP.trim_end()
                ));
            }
            name if command_prefix(name, "list") => {
                let (start, _) = addresses.unwrap_or((self.row, self.row));
                let status = display_literal_bytes(&self.lines[start.min(self.lines.len() - 1)]);
                self.set_status_bytes(status);
            }
            name if command_prefix(name, "delete") => {
                let (start, end) = addresses.unwrap_or((self.row, self.row));
                self.delete_lines(start, end);
                self.end_change();
            }
            name if command_prefix(name, "yank") => {
                let (start, end) = addresses.unwrap_or((self.row, self.row));
                self.yank = self.lines[start..=end].to_vec();
                self.yank_linewise = true;
                self.save_register(selected_register);
                let chars = self.yank.iter().map(|line| line.len() + 1).sum::<usize>();
                self.set_status(format!(
                    "Yank {} lines ({} chars) into [{}]",
                    self.yank.len(),
                    chars,
                    register_name(selected_register)
                ));
            }
            name if command_prefix(name, "read") => {
                let path = if args.is_empty() {
                    self.filename.clone()
                } else {
                    match self.expand_filename(args) {
                        Ok(path) => {
                            self.alternate_filename = self.filename.clone();
                            self.filename = Some(path.clone());
                            Some(path)
                        }
                        Err(e) => {
                            self.set_error(e.to_string());
                            self.end_change();
                            return;
                        }
                    }
                };
                let Some(path) = path else {
                    self.set_error("No current filename");
                    self.end_change();
                    return;
                };
                match fs::metadata(&path) {
                    Ok(metadata) if !metadata.is_file() => {
                        self.set_error(format!("'{}' is not a regular file", path.display()));
                    }
                    Ok(_) => match fs::read(&path) {
                        Ok(data) => {
                            let mut inserted = data
                                .split(|b| *b == b'\n')
                                .map(|x| x.to_vec())
                                .collect::<Vec<_>>();
                            if inserted.last() == Some(&Vec::new()) {
                                inserted.pop();
                            }
                            let at = if is_zero_read_address(body) {
                                0
                            } else {
                                addresses
                                    .map(|(_, end)| end.saturating_add(1))
                                    .unwrap_or(self.row + 1)
                            };
                            for (i, line) in inserted.into_iter().enumerate() {
                                self.lines.insert((at + i).min(self.lines.len()), line);
                            }
                            self.modified = true;
                            let name = path.display().to_string();
                            self.set_status(format_file_status(&name, &data, false, self.readonly));
                        }
                        Err(e) => self.set_error(format!("'{}' {}", path.display(), e)),
                    },
                    Err(e) => self.set_error(format!("'{}' {}", path.display(), e)),
                }
            }
            name if name.starts_with('s') => self.substitute(args, addresses),
            _ => self.set_error(format!("'{}' is not implemented", name)),
        }
        self.end_change();
    }

    fn set_options(&mut self, args: &str) {
        if args.is_empty() || args == "all" {
            self.set_error(format!(
                "{}autoindent {}expandtab {}flash {}ignorecase {}showmatch tabstop={}",
                if self.autoindent { "" } else { "no" },
                if self.expandtab { "" } else { "no" },
                if self.flash { "" } else { "no" },
                if self.ignorecase { "" } else { "no" },
                if self.showmatch { "" } else { "no" },
                self.tabstop
            ));
            return;
        }
        for option in args.split_whitespace() {
            match option {
                "number" | "nu" => self.number = true,
                "nonumber" | "nonu" => self.number = false,
                "autoindent" | "ai" => self.autoindent = true,
                "noautoindent" | "noai" => self.autoindent = false,
                "expandtab" | "et" => self.expandtab = true,
                "noexpandtab" | "noet" => self.expandtab = false,
                "flash" | "fl" => self.flash = true,
                "noflash" | "nofl" => self.flash = false,
                "ignorecase" | "ic" => self.ignorecase = true,
                "noignorecase" | "noic" => self.ignorecase = false,
                "showmatch" | "sm" => self.showmatch = true,
                "noshowmatch" | "nosm" => self.showmatch = false,
                "readonly" | "ro" => self.readonly = true,
                "noreadonly" | "noro" => self.readonly = false,
                value if value.starts_with("tabstop=") => {
                    if let Ok(n) = value[8..].parse::<usize>() {
                        if (1..=32).contains(&n) {
                            self.tabstop = n;
                        } else {
                            self.set_error(format!("bad option: {}", option));
                        }
                    } else {
                        self.set_error(format!("bad option: {}", option));
                    }
                }
                _ => self.set_error(format!("bad option: {}", option)),
            }
        }
    }

    fn substitute(&mut self, args: &str, addresses: Option<(usize, usize)>) {
        let bytes = args.as_bytes();
        let Some(&delimiter) = bytes.first() else {
            self.set_status(":s expression missing delimiters");
            return;
        };
        let Some(split) = find_delimiter(bytes, 1, delimiter) else {
            self.set_status(":s expression missing delimiters");
            return;
        };
        let rest_start = split + 1;
        let Some(end) = find_delimiter(bytes, rest_start, delimiter) else {
            self.set_status(":s expression missing delimiters");
            return;
        };
        let old = if split == 1 {
            let Some((pattern, _)) = self.search.clone() else {
                self.set_error("No previous search");
                return;
            };
            pattern
        } else {
            let pattern = unescape_pattern_with_delimiter(&bytes[1..split], delimiter);
            self.search = Some((pattern.clone(), 1));
            pattern
        };
        let new = unescape_replacement_with_delimiter(&bytes[rest_start..end], delimiter);
        let global = bytes[end + 1..].contains(&b'g');
        if !pattern_is_valid(&old) {
            self.set_status(":s bad search pattern");
            return;
        }
        let mut changed = 0;
        let mut changed_lines = 0;
        let (start, end) = addresses.unwrap_or((self.row, self.row));
        let last_line = self.lines.len() - 1;
        let first_line = start.min(last_line);
        let final_line = end.min(last_line);
        let ignorecase = self.ignorecase;
        let will_change = self.lines[first_line..=final_line]
            .iter()
            .any(|line| replace_pattern_case(line, &old, &new, global, ignorecase).1 != 0);
        if will_change {
            self.begin_change();
        }
        for line in &mut self.lines[first_line..=final_line] {
            let (replacement, count) = replace_pattern_case(line, &old, &new, global, ignorecase);
            if count != 0 {
                *line = replacement;
                changed += count;
                changed_lines += 1;
            }
        }
        if changed == 0 {
            self.set_error("No match");
        } else if changed > 1 {
            self.set_status(format!(
                "{} substitutions on {} lines",
                changed, changed_lines
            ));
        }
    }

    fn handle_command(&mut self, reader: &mut KeyReader, key: u8) -> io::Result<()> {
        if key == 3 {
            self.set_error("Interrupted");
            return Ok(());
        }
        if key == 26 {
            self.set_error("Suspend is unavailable in safe standalone mode");
            return Ok(());
        }
        if key == b'"' {
            let register = reader.key()?;
            if register.is_ascii_lowercase() {
                self.selected_register = Some(register);
            }
            return Ok(());
        }
        if key == b':' {
            return self.command_prompt(reader);
        }
        let selected_register = self.selected_register.take();
        if key == b'/' || key == b'?' {
            let direction = if key == b'/' { 1 } else { -1 };
            if let Some(input) = self.prompt(reader, if key == b'/' { "/" } else { "?" })? {
                let pattern = if input.is_empty() {
                    let Some((pattern, _)) = self.search.clone() else {
                        self.set_error("No previous search");
                        return Ok(());
                    };
                    pattern
                } else {
                    self.search = Some((input.clone(), direction));
                    input
                };
                self.search = Some((pattern.clone(), direction));
                if let Some(error) = pattern_error(&pattern) {
                    self.set_error(format!(
                        "bad search pattern '{}': {}",
                        String::from_utf8_lossy(&pattern),
                        error
                    ));
                    return Ok(());
                }
                let result = self.search_from(&pattern, direction);
                self.report_search(result, direction);
            }
            return Ok(());
        }
        if key == b'n' || key == b'N' {
            let Some((pattern, direction)) = self.search.clone() else {
                self.set_error("No previous search");
                return Ok(());
            };
            let dir = if key == b'n' { direction } else { -direction };
            let result = self.search_from(&pattern, dir);
            self.report_search(result, dir);
            return Ok(());
        }
        if matches!(key, b'f' | b'F' | b't' | b'T') {
            let target = reader.key()?;
            let direction = if matches!(key, b'f' | b't') { 1 } else { -1 };
            let till = matches!(key, b't' | b'T');
            self.char_search = Some((target, direction, till));
            if let Some(col) = self.find_character(target, direction, till) {
                self.col = col;
            }
            return Ok(());
        }
        if key == b';' || key == b',' {
            if let Some((target, direction, till)) = self.char_search {
                let direction = if key == b';' { direction } else { -direction };
                if let Some(col) = self.find_character(target, direction, till) {
                    self.col = col;
                }
            }
            return Ok(());
        }
        if key == b'u' {
            if let Some(previous) = self.undo.pop() {
                self.redo.push(self.snapshot());
                let status = self.undo_status(&previous);
                self.restore(previous);
                self.set_status(status);
            } else {
                self.set_status("Already at oldest change");
            }
            return Ok(());
        }
        if key == 18 {
            if let Some(next) = self.redo.pop() {
                self.undo.push(self.snapshot());
                self.restore(next);
                self.set_status("1 change redone");
            } else {
                self.set_status("Already at newest change");
            }
            return Ok(());
        }
        if key == b'.' && !self.last_change.is_empty() {
            let change = self.last_change.clone();
            self.replaying = true;
            if let Some((&first, rest)) = change.split_first() {
                self.handle_command(reader, first)?;
                for &input in rest {
                    self.handle_insert(reader, input, false)?;
                }
                if self.mode != Mode::Command {
                    self.handle_insert(reader, 0x1b, false)?;
                }
            }
            self.replaying = false;
            return Ok(());
        }

        let mut count = 0usize;
        let mut command = key;
        if key.is_ascii_digit() && key != b'0' {
            count = (key - b'0') as usize;
            loop {
                let next = reader.key()?;
                if next.is_ascii_digit() {
                    count = count * 10 + (next - b'0') as usize;
                } else {
                    command = next;
                    break;
                }
            }
        }
        let count = count.max(1);
        match command {
            b'i' => {
                self.mode = Mode::Insert;
                if !self.replaying {
                    self.last_change = vec![b'i'];
                }
            }
            b'a' => {
                self.col = (self.col + 1).min(self.lines[self.row].len());
                self.mode = Mode::Insert;
                if !self.replaying {
                    self.last_change = vec![b'a'];
                }
            }
            b'A' => {
                self.col = self.lines[self.row].len();
                self.mode = Mode::Insert;
                if !self.replaying {
                    self.last_change = vec![b'A'];
                }
            }
            b'I' => {
                self.col = self.lines[self.row]
                    .iter()
                    .position(|byte| !byte.is_ascii_whitespace())
                    .unwrap_or(0);
                self.mode = Mode::Insert;
                if !self.replaying {
                    self.last_change = vec![b'I'];
                }
            }
            b'R' => {
                self.mode = Mode::Replace;
                if !self.replaying {
                    self.last_change = vec![b'R'];
                }
            }
            b'C' => {
                self.begin_change();
                let removed = self.lines[self.row].split_off(self.col);
                if !removed.is_empty() {
                    self.yank = vec![removed];
                    self.yank_linewise = false;
                    self.save_register(selected_register);
                }
                self.mode = Mode::Insert;
                if !self.replaying {
                    self.last_change = vec![b'C'];
                }
            }
            b'S' => {
                self.begin_change();
                self.yank = vec![std::mem::take(&mut self.lines[self.row])];
                self.yank_linewise = true;
                self.save_register(selected_register);
                self.col = 0;
                self.mode = Mode::Insert;
                if !self.replaying {
                    self.last_change = vec![b'S'];
                }
            }
            b's' => {
                self.begin_change();
                let mut removed = Vec::new();
                for _ in 0..count {
                    if self.col < self.lines[self.row].len() {
                        removed.push(self.lines[self.row].remove(self.col));
                    }
                }
                if !removed.is_empty() {
                    self.yank = vec![removed];
                    self.yank_linewise = false;
                    self.save_register(selected_register);
                }
                self.mode = Mode::Insert;
                if !self.replaying {
                    self.last_change = vec![b's'];
                }
            }
            b'o' | b'O' => {
                self.begin_change();
                let at = if command == b'o' {
                    self.row + 1
                } else {
                    self.row
                };
                self.lines.insert(at, Vec::new());
                self.row = at;
                self.col = 0;
                self.mode = Mode::Insert;
                if !self.replaying {
                    self.last_change = vec![command];
                }
            }
            b'r' => {
                let replacement = reader.key()?;
                if replacement != 0x1b && replacement.is_ascii() && !replacement.is_ascii_control()
                {
                    self.begin_change();
                    for _ in 0..count {
                        if self.col < self.lines[self.row].len() {
                            self.lines[self.row][self.col] = replacement;
                            self.col += 1;
                        }
                    }
                    self.col = self.col.saturating_sub(1);
                    self.end_change();
                }
            }
            b'x' => {
                self.begin_change();
                let mut removed = Vec::new();
                for _ in 0..count {
                    if self.col < self.lines[self.row].len() {
                        removed.push(self.lines[self.row].remove(self.col));
                    }
                }
                if !removed.is_empty() {
                    self.yank = vec![removed];
                    self.yank_linewise = false;
                    self.save_register(selected_register);
                }
                self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
                self.end_change();
            }
            b'D' => {
                self.begin_change();
                let removed = self.lines[self.row].split_off(self.col);
                if !removed.is_empty() {
                    self.yank = vec![removed];
                    self.yank_linewise = false;
                    self.save_register(selected_register);
                }
                self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
                self.end_change();
            }
            b'X' => {
                self.begin_change();
                if self.col > 0 {
                    self.col -= 1;
                    self.lines[self.row].remove(self.col);
                }
                self.end_change();
            }
            b'J' => {
                self.begin_change();
                if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    if !self.lines[self.row].is_empty() {
                        self.lines[self.row].push(b' ');
                    }
                    self.lines[self.row].extend(next);
                }
                self.end_change();
            }
            b'Y' => {
                self.yank = vec![self.lines[self.row].clone()];
                self.yank_linewise = true;
                self.save_register(selected_register);
                self.set_status(register_status(
                    "Yank",
                    &self.yank,
                    true,
                    1,
                    register_name(selected_register),
                ));
            }
            b'~' => {
                self.begin_change();
                for _ in 0..count {
                    if let Some(byte) = self.lines[self.row].get_mut(self.col) {
                        if byte.is_ascii_lowercase() {
                            *byte = byte.to_ascii_uppercase();
                        } else if byte.is_ascii_uppercase() {
                            *byte = byte.to_ascii_lowercase();
                        }
                        self.col = (self.col + 1).min(self.lines[self.row].len().saturating_sub(1));
                    }
                }
                self.end_change();
            }
            b'>' | b'<' => {
                let next = reader.key()?;
                if next == command {
                    self.begin_change();
                    let last = (self.row + count).min(self.lines.len());
                    for line in &mut self.lines[self.row..last] {
                        if command == b'>' {
                            line.splice(0..0, [b'\t']);
                        } else if line.first() == Some(&b'\t') {
                            line.remove(0);
                        } else {
                            let spaces = line.iter().take_while(|byte| **byte == b' ').count();
                            let remove = spaces.min(self.tabstop);
                            line.drain(..remove);
                        }
                    }
                    self.end_change();
                }
            }
            b'%' => {
                if let Some(col) = self.matching_delimiter() {
                    self.col = col;
                }
            }
            b'|' => {
                self.col = count
                    .saturating_sub(1)
                    .min(self.lines[self.row].len().saturating_sub(1));
            }
            b'H' => {
                self.row = self.screen_top;
                self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
            }
            b'M' => {
                self.row = (self.screen_top + self.body_rows() / 2).min(self.lines.len() - 1);
                self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
            }
            b'L' => {
                self.row = (self.screen_top + self.body_rows().saturating_sub(1))
                    .min(self.lines.len() - 1);
                self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
            }
            b'+' => {
                self.row = (self.row + count).min(self.lines.len() - 1);
                self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
            }
            b'-' => {
                self.row = self.row.saturating_sub(count);
                self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
            }
            b'Z' => match reader.key()? {
                b'Q' => {
                    self.quit = true;
                    self.force_quit = true;
                }
                b'Z' => {
                    if !self.modified {
                        self.finish_zz_command();
                    } else {
                        match self.write_file(None, false) {
                            Ok(()) => self.finish_zz_command(),
                            Err(error) => {
                                let message = error.to_string();
                                if message == "No current filename" {
                                    self.clear_status();
                                } else if message.contains(" is read only") {
                                    self.set_error(message);
                                } else {
                                    let path_prefix = self
                                        .filename
                                        .as_deref()
                                        .map(|path| format!("'{}' ", path.display()))
                                        .unwrap_or_default();
                                    let detail = message
                                        .strip_prefix(&path_prefix)
                                        .unwrap_or(message.as_str());
                                    self.set_error(format!("Write error: {}", detail));
                                }
                            }
                        }
                    }
                }
                _ => {}
            },
            b'U' => {
                if let Some(previous) = self.undo.pop() {
                    self.redo.push(self.snapshot());
                    self.restore(previous);
                    let restored = self.lines.get(self.row).cloned().unwrap_or_default();
                    self.set_status(register_status("Undo", &[restored], true, 1, 'U'));
                }
            }
            b'{' | b'}' => {
                let direction: i32 = if command == b'}' { 1 } else { -1 };
                let mut row = self.row as i32;
                let mut crossed_blank = false;
                while (0..self.lines.len() as i32).contains(&(row + direction)) {
                    row += direction;
                    let blank = self.lines[row as usize]
                        .iter()
                        .all(|byte| byte.is_ascii_whitespace());
                    if blank {
                        crossed_blank = true;
                    } else if crossed_blank {
                        break;
                    }
                }
                self.row = row as usize;
                self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
            }
            b'z' => match reader.key()? {
                b'.' => {
                    self.screen_top = self.row;
                    self.scroll_screen(self.screen_rows.saturating_sub(2) / 2, -1);
                }
                b'-' => {
                    self.screen_top = self.row;
                    self.scroll_screen(self.screen_rows.saturating_sub(2), -1);
                }
                b'\r' | b'\n' => {}
                _ => {}
            },
            5 => {
                self.scroll_screen(1, 1);
            }
            8 | 127 | b' ' => {
                (self.row, self.col) =
                    self.motion(if command == b' ' { b'l' } else { b'h' }, count);
            }
            12 => {}
            25 => {
                self.scroll_screen(1, -1);
            }
            b'p' => {
                if let Some(register) = selected_register {
                    if let Some(value) = self.registers.get(&register).cloned() {
                        self.yank = value.lines;
                        self.yank_linewise = value.linewise;
                    } else {
                        self.yank.clear();
                    }
                }
                self.put(true, selected_register);
                self.end_change();
            }
            b'P' => {
                if let Some(register) = selected_register {
                    if let Some(value) = self.registers.get(&register).cloned() {
                        self.yank = value.lines;
                        self.yank_linewise = value.linewise;
                    } else {
                        self.yank.clear();
                    }
                }
                self.put(false, selected_register);
                self.end_change();
            }
            b'd' | b'y' | b'c' => {
                let op = match command {
                    b'd' => Operator::Delete,
                    b'y' => Operator::Yank,
                    _ => Operator::Change,
                };
                let next = reader.key()?;
                if next == command {
                    self.apply_operator(
                        op,
                        self.row..=(self.row + count - 1).min(self.lines.len() - 1),
                        self.col,
                        selected_register,
                    );
                } else {
                    let (r, c) = self.read_motion(reader, next, count)?;
                    self.apply_char_operator(op, self.row, self.col, r, c, selected_register);
                }
                if op != Operator::Yank || selected_register.is_some() {
                    self.save_register(selected_register);
                }
            }
            b'G' => {
                self.row = if count == 1 {
                    self.lines.len() - 1
                } else {
                    (count - 1).min(self.lines.len() - 1)
                };
                self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
            }
            b'g' => {
                let next = reader.key()?;
                if next == b'g' {
                    self.row = 0;
                    self.col = 0;
                } else {
                    self.set_error(format!(
                        "'g{}' is not implemented",
                        display_command_byte(next)
                    ));
                }
            }
            b'm' => {
                let mark = reader.key()?;
                if mark.is_ascii_lowercase() {
                    self.marks[(mark - b'a') as usize] = Some((self.row, self.col));
                }
            }
            b'\'' | b'`' => {
                let mark = reader.key()?;
                if mark.is_ascii_lowercase() {
                    if let Some((row, col)) = self.marks[(mark - b'a') as usize] {
                        self.row = row.min(self.lines.len() - 1);
                        self.col = if command == b'\'' {
                            0
                        } else {
                            col.min(self.lines[self.row].len().saturating_sub(1))
                        };
                    } else {
                        self.set_error("Mark not set");
                    }
                }
            }
            4 => {
                self.scroll_screen(self.screen_rows.saturating_sub(2) / 2, 1);
            }
            2 | 0x88 => {
                self.scroll_screen(self.screen_rows.saturating_sub(2), -1);
            }
            6 | 0x89 => {
                self.scroll_screen(self.screen_rows.saturating_sub(2), 1);
            }
            21 => {
                self.scroll_screen(self.screen_rows.saturating_sub(2) / 2, -1);
            }
            b'k'
            | b'j'
            | b'h'
            | b'l'
            | b'w'
            | b'W'
            | b'e'
            | b'E'
            | b'b'
            | b'B'
            | b'0'
            | b'^'
            | b'$'
            | 0x80..=0x85 => {
                (self.row, self.col) = self.motion(command, count);
            }
            b'\r' => {
                self.row = (self.row + 1).min(self.lines.len() - 1);
                self.col = 0;
            }
            0x86 => {
                self.set_error("Insert mode is not enabled by this key");
            }
            0x87 => {
                self.begin_change();
                if self.col < self.lines[self.row].len() {
                    self.lines[self.row].remove(self.col);
                }
                self.end_change();
            }
            _ => self.set_error(format!(
                "'{}' is not implemented",
                display_command_byte(command)
            )),
        }
        Ok(())
    }

    fn apply_operator(
        &mut self,
        op: Operator,
        rows: std::ops::RangeInclusive<usize>,
        _col: usize,
        register: Option<u8>,
    ) {
        let start = *rows.start();
        let end = *rows.end();
        match op {
            Operator::Yank => {
                self.yank = self.lines[start..=end].to_vec();
                self.yank_linewise = true;
                self.set_status(register_status(
                    "Yank",
                    &self.yank,
                    true,
                    1,
                    register_name(register),
                ));
            }
            Operator::Delete | Operator::Change => {
                self.delete_lines(start, end);
                self.set_status(register_status(
                    "Delete",
                    &self.yank,
                    true,
                    1,
                    register_name(register),
                ));
                self.end_change();
                if op == Operator::Change {
                    self.mode = Mode::Insert;
                    self.begin_change();
                }
            }
        }
    }

    fn apply_char_operator(
        &mut self,
        op: Operator,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
        register: Option<u8>,
    ) {
        if start_row != end_row {
            self.apply_operator(
                op,
                start_row.min(end_row)..=start_row.max(end_row),
                0,
                register,
            );
            return;
        }
        let row = start_row;
        let (first, last) = (start_col.min(end_col), start_col.max(end_col));
        if op == Operator::Yank {
            if !self.lines[row].is_empty() && first < self.lines[row].len() {
                self.yank =
                    vec![self.lines[row][first..=last.min(self.lines[row].len() - 1)].to_vec()];
                self.yank_linewise = false;
                self.set_status(register_status(
                    "Yank",
                    &self.yank,
                    false,
                    1,
                    register_name(register),
                ));
            }
            return;
        }
        self.begin_change();
        let line_len = self.lines[row].len();
        if line_len > 0 && first < line_len {
            self.yank = vec![self.lines[row][first..=last.min(line_len - 1)].to_vec()];
            self.yank_linewise = false;
            self.lines[row].drain(first..=last.min(line_len - 1));
            self.set_status(register_status(
                "Delete",
                &self.yank,
                false,
                1,
                register_name(register),
            ));
        }
        self.row = row;
        self.col = first.min(self.lines[row].len().saturating_sub(1));
        self.end_change();
        if op == Operator::Change {
            self.begin_change();
            self.mode = Mode::Insert;
        }
    }

    fn handle_insert(&mut self, reader: &mut KeyReader, key: u8, special: bool) -> io::Result<()> {
        if key == 3 || key == 26 {
            self.mode = Mode::Command;
            self.end_change();
            self.set_error("Interrupted");
            return Ok(());
        }
        if key == 0x1b {
            self.mode = Mode::Command;
            self.end_change();
            self.col = self
                .col
                .saturating_sub(1)
                .min(self.lines[self.row].len().saturating_sub(1));
            return Ok(());
        }
        if key == b'\r' || key == b'\n' {
            self.begin_change();
            let rest = self.lines[self.row].split_off(self.col);
            let indent = if self.autoindent {
                self.lines[self.row]
                    .iter()
                    .take_while(|byte| **byte == b' ' || **byte == b'\t')
                    .copied()
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            self.row += 1;
            let mut next_line = indent.clone();
            next_line.extend(rest);
            self.lines.insert(self.row, next_line);
            self.col = indent.len();
            if !self.replaying {
                self.last_change.push(key);
            }
            return Ok(());
        }
        if key == 8 || key == 127 {
            self.begin_change();
            if self.col > 0 {
                self.col -= 1;
                self.lines[self.row].remove(self.col);
            } else if self.row > 0 {
                let current = self.lines.remove(self.row);
                self.row -= 1;
                self.col = self.lines[self.row].len();
                self.lines[self.row].extend(current);
            }
            if !self.replaying {
                self.last_change.push(key);
            }
            return Ok(());
        }
        if special && key == 0x80 {
            self.row = self.row.saturating_sub(1);
            self.col = self.col.min(self.lines[self.row].len());
            return Ok(());
        }
        if special && key == 0x81 {
            self.row = (self.row + 1).min(self.lines.len() - 1);
            self.col = self.col.min(self.lines[self.row].len());
            return Ok(());
        }
        if special && key == 0x82 {
            self.col = (self.col + 1).min(self.lines[self.row].len());
            return Ok(());
        }
        if special && key == 0x83 {
            self.col = self.col.saturating_sub(1);
            return Ok(());
        }
        if special && key == 0x84 {
            self.col = 0;
            return Ok(());
        }
        if special && key == 0x85 {
            self.col = self.lines[self.row].len();
            return Ok(());
        }
        if special && key == 0x86 {
            self.mode = Mode::Replace;
            return Ok(());
        }
        if special && key == 0x87 {
            self.begin_change();
            if self.col < self.lines[self.row].len() {
                self.lines[self.row].remove(self.col);
            }
            self.end_change();
            return Ok(());
        }
        if special && (key == 0x88 || key == 0x89) {
            self.scroll_screen(
                self.screen_rows.saturating_sub(2),
                if key == 0x88 { -1 } else { 1 },
            );
            return Ok(());
        }
        if key == b'\t' && self.expandtab {
            self.begin_change();
            let width = self.line_display_width(&self.lines[self.row], self.col);
            let spaces = self.tabstop - (width % self.tabstop);
            for _ in 0..spaces {
                self.lines[self.row].insert(self.col, b' ');
                self.col += 1;
            }
            if !self.replaying {
                self.last_change.push(key);
            }
            return Ok(());
        }
        if key >= 0x20 && key != 0x7f && key != 0x9b {
            let byte = key;
            self.begin_change();
            if self.mode == Mode::Replace && self.col < self.lines[self.row].len() {
                self.lines[self.row][self.col] = byte;
            } else {
                self.lines[self.row].insert(self.col, byte);
            }
            self.col += 1;
            if !self.replaying {
                self.last_change.push(byte);
            }
            if self.showmatch
                && matches!(byte, b')' | b']' | b'}')
                && matching_position(&self.lines[self.row], self.col.saturating_sub(1)).is_some()
            {
                // The C implementation flashes the screen here; it does not
                // replace the status line with a message.
            }
        }
        let _ = reader;
        Ok(())
    }

    pub fn run(&mut self) -> io::Result<()> {
        let terminal = Terminal::enter()?;
        let mut reader = KeyReader::new(terminal.interactive());
        self.refresh_size();
        self.render(None)?;
        while !self.quit {
            let Some(key) = reader.try_key()? else {
                if self.refresh_size() {
                    self.render(None)?;
                }
                continue;
            };
            let special = reader.take_special();
            if self.hit_return {
                if matches!(key, b'\r' | b'\n') {
                    self.hit_return = false;
                    self.clear_status();
                }
                self.refresh_size();
                self.render(None)?;
                continue;
            }
            self.clear_status();
            match self.mode {
                Mode::Command => self.handle_command(&mut reader, key)?,
                Mode::Insert | Mode::Replace => self.handle_insert(&mut reader, key, special)?,
            }
            self.refresh_size();
            self.render(None)?;
        }
        let mut out = io::stdout().lock();
        write!(out, "\x1b[?25h\x1b[{};1H\x1b[K", self.screen_rows)?;
        out.flush()
    }
}

fn find_pattern_before(text: &[u8], pattern: &[u8], before: usize) -> Option<usize> {
    (0..=before.min(text.len())).rfind(|start| match_pattern_at(text, *start, pattern, 0).is_some())
}

fn fold_ascii(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|byte| byte.to_ascii_lowercase()).collect()
}

#[derive(Clone)]
enum PatternToken {
    Literal(u8),
    Any,
    Class(Vec<(u8, u8)>, bool),
}

fn pattern_token(pattern: &[u8], at: usize) -> Option<(PatternToken, usize)> {
    let byte = *pattern.get(at)?;
    if byte == b'\\' {
        return pattern
            .get(at + 1)
            .map(|value| (PatternToken::Literal(*value), at + 2));
    }
    if byte == b'.' {
        return Some((PatternToken::Any, at + 1));
    }
    if byte != b'[' {
        return Some((PatternToken::Literal(byte), at + 1));
    }
    let mut index = at + 1;
    let negated = pattern.get(index) == Some(&b'^');
    if negated {
        index += 1;
    }
    let mut ranges = Vec::new();
    while index < pattern.len() && pattern[index] != b']' {
        let first = if pattern[index] == b'\\' && index + 1 < pattern.len() {
            index += 1;
            pattern[index]
        } else {
            pattern[index]
        };
        index += 1;
        if pattern.get(index) == Some(&b'-') && pattern.get(index + 1).is_some_and(|b| *b != b']') {
            index += 1;
            let last = if pattern[index] == b'\\' && index + 1 < pattern.len() {
                index += 1;
                pattern[index]
            } else {
                pattern[index]
            };
            index += 1;
            ranges.push((first, last));
        } else {
            ranges.push((first, first));
        }
    }
    if pattern.get(index) != Some(&b']') {
        return Some((PatternToken::Literal(b'['), at + 1));
    }
    Some((PatternToken::Class(ranges, negated), index + 1))
}

fn token_matches(token: &PatternToken, byte: u8) -> bool {
    match token {
        PatternToken::Literal(value) => *value == byte,
        PatternToken::Any => true,
        PatternToken::Class(ranges, negated) => {
            let found = ranges
                .iter()
                .any(|(first, last)| *first <= byte && byte <= *last);
            if *negated {
                !found
            } else {
                found
            }
        }
    }
}

type Captures = [Option<(usize, usize)>; 10];

fn group_number(pattern: &[u8], at: usize) -> usize {
    pattern[..at]
        .windows(2)
        .filter(|pair| *pair == [b'\\', b'('])
        .count()
        .saturating_add(1)
        .min(9)
}

fn match_pattern_captures(
    text: &[u8],
    text_at: usize,
    pattern: &[u8],
    pattern_at: usize,
    captures: Captures,
    stack: Vec<usize>,
) -> Option<(usize, Captures)> {
    if pattern_at >= pattern.len() {
        return Some((text_at, captures));
    }
    if pattern.get(pattern_at..pattern_at + 2) == Some(b"\\(") {
        let mut captures = captures;
        let mut stack = stack;
        let group = group_number(pattern, pattern_at);
        captures[group] = Some((text_at, text_at));
        stack.push(group);
        return match_pattern_captures(text, text_at, pattern, pattern_at + 2, captures, stack);
    }
    if pattern.get(pattern_at..pattern_at + 2) == Some(b"\\)") {
        let mut captures = captures;
        let mut stack = stack;
        if let Some(group) = stack.pop() {
            if let Some((start, _)) = captures[group] {
                captures[group] = Some((start, text_at));
            }
        }
        return match_pattern_captures(text, text_at, pattern, pattern_at + 2, captures, stack);
    }
    if pattern.get(pattern_at) == Some(&b'\\')
        && pattern
            .get(pattern_at + 1)
            .is_some_and(|byte| byte.is_ascii_digit())
    {
        let group = (pattern[pattern_at + 1] - b'0') as usize;
        let (start, end) = captures[group]?;
        let captured = &text[start..end];
        if text.get(text_at..text_at + captured.len()) == Some(captured) {
            return match_pattern_captures(
                text,
                text_at + captured.len(),
                pattern,
                pattern_at + 2,
                captures,
                stack,
            );
        }
        return None;
    }
    if pattern[pattern_at] == b'^' {
        return if text_at == 0 {
            match_pattern_captures(text, text_at, pattern, pattern_at + 1, captures, stack)
        } else {
            None
        };
    }
    if pattern[pattern_at] == b'$' && pattern_at + 1 == pattern.len() {
        return (text_at == text.len()).then_some((text_at, captures));
    }
    let (token, next) = pattern_token(pattern, pattern_at)?;
    if pattern.get(next) == Some(&b'*') {
        let mut positions = vec![text_at];
        let mut current = text_at;
        while let Some(byte) = text.get(current) {
            if !token_matches(&token, *byte) {
                break;
            }
            current += 1;
            positions.push(current);
        }
        for position in positions.into_iter().rev() {
            if let Some(result) =
                match_pattern_captures(text, position, pattern, next + 1, captures, stack.clone())
            {
                return Some(result);
            }
        }
        return None;
    }
    if text
        .get(text_at)
        .is_some_and(|byte| token_matches(&token, *byte))
    {
        match_pattern_captures(text, text_at + 1, pattern, next, captures, stack)
    } else {
        None
    }
}

fn match_pattern_at(
    text: &[u8],
    text_at: usize,
    pattern: &[u8],
    pattern_at: usize,
) -> Option<usize> {
    match_pattern_captures(text, text_at, pattern, pattern_at, [None; 10], Vec::new())
        .map(|(end, _)| end)
}

fn find_pattern(text: &[u8], pattern: &[u8], from: usize) -> Option<usize> {
    if pattern.is_empty() {
        return Some(from.min(text.len()));
    }
    let anchored = pattern.first() == Some(&b'^');
    let end = text.len();
    let mut starts = if anchored {
        from.min(1)..=from.min(1)
    } else {
        from.min(end)..=end
    };
    starts.find(|&start| match_pattern_at(text, start, pattern, 0).is_some())
}

fn replace_pattern_case(
    line: &[u8],
    pattern: &[u8],
    replacement: &[u8],
    global: bool,
    ignorecase: bool,
) -> (Vec<u8>, usize) {
    let matching_line;
    let matching_pattern;
    let line_for_matching = if ignorecase {
        matching_line = fold_ascii(line);
        &matching_line
    } else {
        line
    };
    let pattern_for_matching = if ignorecase {
        matching_pattern = fold_ascii(pattern);
        &matching_pattern
    } else {
        pattern
    };
    let mut output = Vec::with_capacity(line.len());
    let mut source_at = 0;
    let mut changed = 0;
    while source_at <= line.len() {
        let Some(start) = find_pattern(line_for_matching, pattern_for_matching, source_at) else {
            output.extend_from_slice(&line[source_at..]);
            break;
        };
        let Some((end, captures)) = match_pattern_captures(
            line_for_matching,
            start,
            pattern_for_matching,
            0,
            [None; 10],
            Vec::new(),
        ) else {
            break;
        };
        output.extend_from_slice(&line[source_at..start]);
        append_replacement(&mut output, replacement, line, start, end, captures);
        changed += 1;
        source_at = end;
        if !global {
            output.extend_from_slice(&line[source_at..]);
            break;
        }
        if end == start {
            if source_at == line.len() {
                break;
            }
            output.push(line[source_at]);
            source_at += 1;
        }
    }
    (output, changed)
}

fn find_delimiter(bytes: &[u8], from: usize, delimiter: u8) -> Option<usize> {
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate().skip(from) {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if *byte == delimiter {
            return Some(index);
        }
    }
    None
}

fn unescape_pattern(bytes: &[u8]) -> Vec<u8> {
    unescape_pattern_with_delimiter(bytes, b'/')
}

fn unescape_pattern_with_delimiter(bytes: &[u8], delimiter: u8) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
            if bytes[index + 1] == delimiter {
                result.push(delimiter);
            } else {
                result.push(b'\\');
                result.push(bytes[index + 1]);
            }
            index += 2;
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }
    result
}

fn unescape_replacement_with_delimiter(bytes: &[u8], delimiter: u8) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
            if bytes[index + 1].is_ascii_digit() {
                result.push(b'\\');
                result.push(bytes[index + 1]);
            } else if bytes[index + 1] == delimiter {
                result.push(delimiter);
            } else {
                result.push(bytes[index + 1]);
            }
            index += 2;
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }
    result
}

fn append_replacement(
    output: &mut Vec<u8>,
    replacement: &[u8],
    line: &[u8],
    start: usize,
    end: usize,
    captures: Captures,
) {
    let mut index = 0;
    while index < replacement.len() {
        if replacement[index] == b'&' {
            output.extend_from_slice(&line[start..end]);
            index += 1;
        } else if replacement[index] == b'\\'
            && replacement
                .get(index + 1)
                .is_some_and(|byte| byte.is_ascii_digit())
        {
            let group = (replacement[index + 1] - b'0') as usize;
            if let Some((capture_start, capture_end)) = captures[group] {
                output.extend_from_slice(&line[capture_start..capture_end]);
            }
            index += 2;
        } else if replacement[index] == b'\\' && index + 1 < replacement.len() {
            output.push(replacement[index + 1]);
            index += 2;
        } else {
            output.push(replacement[index]);
            index += 1;
        }
    }
}

fn display_literal_bytes(line: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    for byte in line {
        if *byte >= 0x80 && (*byte < b' ' || *byte == 0x9b || *byte == 0x7f) {
            result.extend_from_slice(b"\x1b[7m.\x1b[m");
        } else if byte.is_ascii_control() {
            result.push(b'^');
            result.push(byte ^ 0x40);
        } else {
            result.push(*byte);
        }
    }
    result.push(b'$');
    result
}

fn display_command_byte(byte: u8) -> String {
    if byte.is_ascii_control() {
        format!("^{}", (byte ^ 0x40) as char)
    } else if byte == 0x7f {
        "^?".into()
    } else if byte.is_ascii() {
        (byte as char).to_string()
    } else {
        "?".into()
    }
}

fn pattern_error(pattern: &[u8]) -> Option<&'static str> {
    if pattern == b"[" {
        return Some("Invalid regular expression");
    }
    let mut groups = 0usize;
    let mut index = 0;
    while index < pattern.len() {
        match pattern[index] {
            b'\\' => {
                let Some(&next) = pattern.get(index + 1) else {
                    return Some("Trailing backslash");
                };
                if next == b'(' {
                    groups += 1;
                } else if next == b')' {
                    if groups == 0 {
                        return Some("Unmatched ) or \\)");
                    }
                    groups -= 1;
                }
                index += 2;
            }
            b'[' => {
                index += 1;
                let mut closed = false;
                while index < pattern.len() {
                    if pattern[index] == b'\\' {
                        index += 2;
                    } else if pattern[index] == b']' {
                        closed = true;
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
                if !closed {
                    return Some("Unmatched [, [^, [:, [., or [=");
                }
            }
            _ => index += 1,
        }
    }
    if groups == 0 {
        None
    } else {
        Some("Unmatched ( or \\(")
    }
}

fn pattern_is_valid(pattern: &[u8]) -> bool {
    pattern_error(pattern).is_none()
}

fn line_count(data: &[u8]) -> usize {
    if data.is_empty() {
        0
    } else {
        data.split(|byte| *byte == b'\n').count() - usize::from(data.ends_with(b"\n"))
    }
}

fn snapshot_bytes(snapshot: &Snapshot) -> Vec<u8> {
    let mut data = Vec::new();
    for (index, line) in snapshot.lines.iter().enumerate() {
        data.extend_from_slice(line);
        if index + 1 < snapshot.lines.len() || snapshot.trailing_newline {
            data.push(b'\n');
        }
    }
    data
}

fn format_file_status(name: &str, data: &[u8], new_file: bool, readonly: bool) -> String {
    format!(
        "'{}'{}{} {}L, {}C",
        name,
        if new_file { " [New file]" } else { "" },
        if readonly { " [Readonly]" } else { "" },
        line_count(data),
        data.len()
    )
}

fn register_name(register: Option<u8>) -> char {
    register.map(char::from).unwrap_or('D')
}

fn command_prefix(input: &str, command: &str) -> bool {
    !input.is_empty() && command.starts_with(input)
}

fn register_status(
    operation: &str,
    lines: &[Vec<u8>],
    linewise: bool,
    count: usize,
    register: char,
) -> String {
    let line_count = if linewise { lines.len() * count } else { 0 };
    let char_count = lines
        .iter()
        .map(|line| line.len() + usize::from(linewise))
        .sum::<usize>()
        * count;
    format!(
        "{} {} lines ({} chars) from [{}]",
        operation, line_count, char_count, register
    )
}

fn matching_position(line: &[u8], position: usize) -> Option<usize> {
    let close = *line.get(position)?;
    let open = match close {
        b')' => b'(',
        b']' => b'[',
        b'}' => b'{',
        _ => return None,
    };
    let mut depth = 0usize;
    for index in (0..position).rev() {
        if line[index] == close {
            depth += 1;
        } else if line[index] == open {
            if depth == 0 {
                return Some(index);
            }
            depth -= 1;
        }
    }
    None
}

fn parse_address(
    rest: &str,
    current_row: usize,
    line_count: usize,
    lines: &[Vec<u8>],
    ignorecase: bool,
    marks: &[Option<(usize, usize)>; 26],
    search: &mut Option<(Vec<u8>, i32)>,
) -> Result<Option<(usize, usize)>, String> {
    let last = line_count.saturating_sub(1);
    let bytes = rest.as_bytes();
    let mut position = 0;
    while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    let address_start = position;
    let mut address = current_row as isize + 1;
    let mut got_address = false;
    let mut sign = 0isize;

    match bytes.get(position).copied() {
        Some(b'.') => {
            position += 1;
            got_address = true;
        }
        Some(b'$') => {
            position += 1;
            address = line_count as isize;
            got_address = true;
        }
        Some(b'\'') => {
            let mark = bytes.get(position + 1).copied().unwrap_or_default();
            if !mark.is_ascii_lowercase() {
                return Err("Mark not set".to_owned());
            }
            let Some((row, _)) = marks[(mark - b'a') as usize] else {
                return Err("Mark not set".to_owned());
            };
            address = row as isize + 1;
            position += 2;
            got_address = true;
        }
        Some(b'/') | Some(b'?') => {
            let delimiter = bytes[position];
            let mut escaped = false;
            let end = bytes
                .iter()
                .enumerate()
                .skip(position + 1)
                .find_map(|(index, byte)| {
                    if escaped {
                        escaped = false;
                    } else if *byte == b'\\' {
                        escaped = true;
                    } else if *byte == delimiter {
                        return Some(index);
                    }
                    None
                })
                .unwrap_or(bytes.len());
            let raw_pattern = unescape_pattern(&bytes[position + 1..end]);
            let pattern = if raw_pattern.is_empty() {
                search
                    .as_ref()
                    .map(|(pattern, _)| pattern.clone())
                    .unwrap_or(raw_pattern)
            } else {
                raw_pattern
            };
            let direction: i32 = if delimiter == b'/' { 1 } else { -1 };
            *search = Some((pattern.clone(), direction));
            if !pattern_is_valid(&pattern) {
                return Err(format!(
                    "bad search pattern '{}': {}",
                    String::from_utf8_lossy(&pattern),
                    pattern_error(&pattern).unwrap_or("invalid pattern")
                ));
            }
            let mut row = current_row as i32;
            for _ in 0..line_count {
                row = (row + direction).rem_euclid(line_count as i32);
                let text = &lines[row as usize];
                let found = if ignorecase {
                    find_pattern(&fold_ascii(text), &fold_ascii(&pattern), 0)
                } else {
                    find_pattern(text, &pattern, 0)
                };
                if found.is_some() {
                    address = row as isize + 1;
                    got_address = true;
                    break;
                }
            }
            if !got_address {
                return Err("Pattern not found".to_owned());
            }
            position = (end + usize::from(end < bytes.len())).min(bytes.len());
        }
        Some(byte) if byte.is_ascii_digit() => {
            let start = position;
            while bytes.get(position).is_some_and(u8::is_ascii_digit) {
                position += 1;
            }
            address = rest[start..position]
                .parse::<isize>()
                .map_err(|_| "Invalid range".to_owned())?;
            got_address = true;
        }
        Some(b'+') | Some(b'-') => {
            got_address = true;
            sign = if bytes[position] == b'+' { 1 } else { -1 };
            position += 1;
        }
        _ => return Ok(None),
    }

    while position < bytes.len() {
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            address += sign;
            sign = 0;
            position += 1;
        }
        let Some(byte) = bytes.get(position).copied() else {
            break;
        };
        if byte == b'+' || byte == b'-' {
            address += sign;
            sign = if byte == b'+' { 1 } else { -1 };
            position += 1;
        } else if byte.is_ascii_digit() {
            let start = position;
            while bytes.get(position).is_some_and(u8::is_ascii_digit) {
                position += 1;
            }
            let value = rest[start..position]
                .parse::<isize>()
                .map_err(|_| "Invalid range".to_owned())?;
            address += if sign < 0 { -value } else { value };
            sign = 0;
        } else {
            address += sign;
            break;
        }
    }
    if !got_address {
        return Ok(None);
    }
    if address < 0 || address > line_count as isize {
        return Err("Invalid range".to_owned());
    }
    let row = if address == 0 {
        0
    } else {
        (address as usize - 1).min(last)
    };
    Ok(Some((row, position.max(address_start))))
}

type ParsedEx<'a> = (Option<(usize, usize)>, &'a str, &'a str);

fn parse_ex<'a>(
    command: &'a str,
    current_row: usize,
    line_count: usize,
    lines: &[Vec<u8>],
    ignorecase: bool,
    marks: &[Option<(usize, usize)>; 26],
    search: &mut Option<(Vec<u8>, i32)>,
) -> Result<ParsedEx<'a>, String> {
    let mut rest = command;
    let mut start = if rest.starts_with('%') {
        rest = &rest[1..];
        Some((0, line_count.saturating_sub(1)))
    } else {
        match parse_address(
            rest,
            current_row,
            line_count,
            lines,
            ignorecase,
            marks,
            search,
        )? {
            Some((line, consumed)) => {
                rest = &rest[consumed..];
                Some((line, line))
            }
            None => None,
        }
    };
    if rest.starts_with(',') || rest.starts_with(';') {
        let semicolon = rest.starts_with(';');
        rest = &rest[1..];
        let second_current = if semicolon {
            start.map(|(line, _)| line).unwrap_or(current_row)
        } else {
            current_row
        };
        let end = match parse_address(
            rest,
            second_current,
            line_count,
            lines,
            ignorecase,
            marks,
            search,
        )? {
            Some((line, consumed)) => {
                rest = &rest[consumed..];
                line
            }
            None => line_count.saturating_sub(1),
        };
        if let Some((start, _)) = start {
            if start > end {
                return Err("Invalid range".to_owned());
            }
        }
        start = start.map(|(a, _)| (a, end));
    }
    let split = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    let (name, args) = rest.split_at(split);
    Ok((start, name, args.trim()))
}

fn is_zero_base(command: &str) -> Option<&[u8]> {
    let command = command.trim_start().as_bytes();
    (command.first() == Some(&b'0') && !command.get(1).is_some_and(u8::is_ascii_digit))
        .then_some(command)
}

fn is_bare_zero_address(command: &str) -> bool {
    let Some(command) = is_zero_base(command) else {
        return false;
    };
    let mut position = 1;
    while command.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    command.get(position) == Some(&b'=')
}

fn is_zero_read_address(command: &str) -> bool {
    let Some(command) = is_zero_base(command) else {
        return false;
    };
    let mut position = 1;
    while command.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    matches!(command.get(position), Some(b'r') | Some(b',') | Some(b';'))
}

fn startup_commands() -> (Vec<String>, Option<String>) {
    if let Ok(exinit) = std::env::var("EXINIT") {
        return (
            exinit
                .split('\n')
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .map(str::to_owned)
                .collect(),
            None,
        );
    }
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return (Vec::new(), None);
    };
    let path = PathBuf::from(home).join(".exrc");
    let Ok(metadata) = fs::metadata(&path) else {
        return (Vec::new(), None);
    };
    #[cfg(unix)]
    if std::os::unix::fs::MetadataExt::mode(&metadata) & 0o022 != 0
        || platform_user::current_uid()
            .is_none_or(|uid| std::os::unix::fs::MetadataExt::uid(&metadata) != uid)
    {
        return (Vec::new(), Some(".exrc: permission denied".into()));
    }
    #[cfg(not(unix))]
    let _ = &metadata;
    (
        fs::read_to_string(path)
            .map(|contents| {
                contents
                    .split('\n')
                    .map(str::trim)
                    .filter(|command| !command.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        None,
    )
}

pub fn run(arguments: Vec<String>) -> i32 {
    let mut readonly = false;
    let mut help = false;
    let mut commands = Vec::new();
    let mut files = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "-R" => readonly = true,
            "-H" => help = true,
            "-h" | "--help" => {
                help = true;
            }
            "-c" => {
                index += 1;
                if let Some(command) = arguments.get(index) {
                    commands.push(command.clone());
                } else {
                    eprintln!("vi: option requires an argument -- c");
                    return 1;
                }
            }
            argument if argument.starts_with("-c") && argument.len() > 2 => {
                commands.push(argument[2..].to_owned())
            }
            argument if argument.starts_with('-') => {
                eprintln!("vi: unknown option: {}", argument);
                return 1;
            }
            argument => files.push(PathBuf::from(argument)),
        }
        index += 1;
    }
    if help {
        print!("{}", HELP);
        eprintln!("usage: vi [-c command] [-R] [-H] [file ...]");
        return if arguments.iter().any(|x| x == "-h" || x == "--help") {
            0
        } else {
            1
        };
    }
    if files.is_empty() {
        files.push(PathBuf::from(""));
    }
    let (startup, startup_error) = startup_commands();
    let mut file_index = 0usize;
    while file_index < files.len() {
        let path = files[file_index].clone();
        let filename = if path.as_os_str().is_empty() {
            None
        } else {
            Some(path)
        };
        let mut editor = match Editor::new(filename, readonly) {
            Ok(editor) => editor,
            Err(error) => {
                eprintln!("vi: {}", error);
                return 1;
            }
        };
        editor.set_file_context(file_index, files.len());
        if file_index == 0 {
            if let Some(error) = &startup_error {
                editor.set_error(error);
            }
            for command in &startup {
                editor.execute_ex(command);
                if editor.quit || editor.hit_return {
                    break;
                }
            }
        }
        for command in &commands {
            editor.execute_ex(command);
            if editor.quit || editor.hit_return {
                break;
            }
        }
        if editor.quit {
            if editor.rewind_files {
                file_index = 0;
                continue;
            }
            if editor.previous_file {
                file_index = file_index.saturating_sub(1);
                continue;
            }
            if editor.next_file {
                file_index += 1;
                continue;
            }
            break;
        }
        if let Err(error) = editor.run() {
            eprintln!("vi: {}", error);
            return 1;
        }
        if editor.rewind_files {
            file_index = 0;
        } else if editor.previous_file {
            file_index = file_index.saturating_sub(1);
        } else if editor.next_file {
            file_index += 1;
        } else {
            break;
        }
    }
    0
}

#[cfg(test)]
mod terminal_event_tests {
    use super::*;

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
    }
}

// Core behavioral tests live in tests/editor_core.rs so the editor source
// remains focused on implementation and terminal behavior.
