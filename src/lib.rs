//! A small, dependency-free vi editor core.
//!
//! The editor owns its text and never exposes pointers into the buffer.  This
//! is intentionally byte-oriented (like the original BusyBox editor), while
//! displaying non-UTF-8 input lossily at the terminal.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
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

#[derive(Clone, Copy)]
struct EditorState {
    row: usize,
    col: usize,
    trailing_newline: bool,
    modified: bool,
}

#[derive(Clone)]
enum Edit {
    Bytes {
        row: usize,
        start: usize,
        removed: Vec<u8>,
        inserted: Vec<u8>,
    },
    Lines {
        start: usize,
        removed: Vec<Vec<u8>>,
        inserted: Vec<Vec<u8>>,
    },
}

#[derive(Clone)]
struct Change {
    edits: Vec<Edit>,
    before: EditorState,
    after: EditorState,
}

struct PendingChange {
    edits: Vec<Edit>,
    before: EditorState,
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

/// A terminal color supplied by an embedding application's syntax theme.
///
/// The editor does not know anything about programming languages or token
/// classes.  It only uses these colors when an embedding application installs
/// a [`SyntaxHighlighter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightColor {
    /// An entry in the terminal's 256-color palette.
    Ansi(u8),
    /// A true-color RGB value.
    Rgb { red: u8, green: u8, blue: u8 },
}

/// Presentation attributes for one syntax-highlighted range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HighlightStyle {
    pub foreground: Option<HighlightColor>,
    pub background: Option<HighlightColor>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl HighlightStyle {
    /// A style that leaves the terminal in its normal presentation.
    pub const fn plain() -> Self {
        Self {
            foreground: None,
            background: None,
            bold: false,
            italic: false,
            underline: false,
        }
    }

    /// Construct a style with only a foreground color.
    pub const fn foreground(color: HighlightColor) -> Self {
        Self {
            foreground: Some(color),
            ..Self::plain()
        }
    }

    /// Construct a style with only a background color.
    pub const fn background(color: HighlightColor) -> Self {
        Self {
            background: Some(color),
            ..Self::plain()
        }
    }

    pub const fn with_foreground(mut self, color: HighlightColor) -> Self {
        self.foreground = Some(color);
        self
    }

    pub const fn with_background(mut self, color: HighlightColor) -> Self {
        self.background = Some(color);
        self
    }

    pub const fn with_bold(mut self, bold: bool) -> Self {
        self.bold = bold;
        self
    }

    pub const fn with_italic(mut self, italic: bool) -> Self {
        self.italic = italic;
        self
    }

    pub const fn with_underline(mut self, underline: bool) -> Self {
        self.underline = underline;
        self
    }

    fn is_plain(self) -> bool {
        self == Self::plain()
    }

    fn write_sgr<W: Write>(self, out: &mut W) -> io::Result<()> {
        write!(out, "\x1b[")?;
        let mut separator = "";
        if self.bold {
            write!(out, "1")?;
            separator = ";";
        }
        if self.italic {
            write!(out, "{separator}3")?;
            separator = ";";
        }
        if self.underline {
            write!(out, "{separator}4")?;
            separator = ";";
        }
        if let Some(color) = self.foreground {
            color.write_sgr(out, separator, 38)?;
            separator = ";";
        }
        if let Some(color) = self.background {
            color.write_sgr(out, separator, 48)?;
        }
        write!(out, "m")
    }
}

impl HighlightColor {
    fn write_sgr<W: Write>(self, out: &mut W, separator: &str, channel: u8) -> io::Result<()> {
        match self {
            Self::Ansi(value) => write!(out, "{separator}{channel};5;{value}"),
            Self::Rgb { red, green, blue } => {
                write!(out, "{separator}{channel};2;{red};{green};{blue}")
            }
        }
    }
}

/// A half-open byte range in the complete editor buffer and its terminal
/// presentation. Ranges should be sorted, non-overlapping, and within the
/// buffer passed to the highlighter. Invalid or overlapping ranges are safely
/// clipped when the editor renders them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub style: HighlightStyle,
}

impl HighlightSpan {
    pub const fn new(start: usize, end: usize, style: HighlightStyle) -> Self {
        Self { start, end, style }
    }
}

/// A host-provided syntax highlighter.
///
/// The base editor deliberately contains no language grammars, parser, or
/// theme. The embedding application can install any highlighter it already
/// uses (for example, an adapter around Zed's syntax service). The callback
/// receives the complete current buffer and returns byte ranges relative to
/// that buffer. It is called lazily on the next redraw or
/// [`Editor::syntax_highlights`] access after edits, and can retain parser
/// state between calls if that is useful.
pub trait SyntaxHighlighter {
    fn highlight(&mut self, buffer: &[u8]) -> Vec<HighlightSpan>;

    /// Return a synchronous syntax preview for the requested visible byte
    /// range. The complete buffer is supplied so the embedding highlighter
    /// can use line-aligned context around the viewport while keeping the
    /// returned spans relative to the complete buffer.
    ///
    /// The default leaves the preview disabled. Full-buffer highlighting and
    /// asynchronous highlighters continue to use [`Self::highlight`] and
    /// [`Self::poll`].
    fn highlight_visible(
        &mut self,
        _buffer: &[u8],
        _visible_range: Range<usize>,
    ) -> Option<Vec<HighlightSpan>> {
        None
    }

    /// Invalidate any state retained by [`Self::highlight_visible`] after a
    /// buffer edit or an embedding-side syntax change.
    fn invalidate_visible(&mut self) {}

    /// Return the optional `:set number` gutter styles.
    ///
    /// The default follows Vim's terminal `LineNr` and `CursorLineNr`
    /// treatment: cyan line numbers and a bold cyan number for the current
    /// line. The base editor does not use these styles unless a highlighter
    /// is installed, and an embedding application can return `None` to leave
    /// either form unstyled or supply colors from its own theme.
    fn line_number_style(&self, current_line: bool) -> Option<HighlightStyle> {
        Some(HighlightStyle::foreground(HighlightColor::Ansi(6)).with_bold(current_line))
    }

    /// Return a completed asynchronous highlight update, if the embedding
    /// application performs parsing away from the editor's input path.
    ///
    /// The default preserves the synchronous callback behavior used by
    /// existing embedders. Editors without a highlighter never call this.
    fn poll(&mut self) -> Option<Vec<HighlightSpan>> {
        None
    }

    /// Whether [`SyntaxHighlighter::highlight`] has work in flight.
    ///
    /// Asynchronous highlighters can return their existing ranges from
    /// `highlight` while a newer snapshot is being parsed. The editor then
    /// keeps those ranges visible until `poll` returns the replacement.
    fn has_pending_work(&self) -> bool {
        false
    }
}

impl<F> SyntaxHighlighter for F
where
    F: FnMut(&[u8]) -> Vec<HighlightSpan>,
{
    fn highlight(&mut self, buffer: &[u8]) -> Vec<HighlightSpan> {
        self(buffer)
    }
}

// Rebuilding a syntax tree can take much longer than processing a keystroke
// for large files. Coalesce a burst of edits before asking an optional
// highlighter for another snapshot. The highlighter can still be forced by
// `Editor::syntax_highlights` for non-terminal embedders.
const SYNTAX_HIGHLIGHT_DEBOUNCE: Duration = Duration::from_millis(75);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SelectionPoint {
    line: usize,
    offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionKind {
    Characters,
    Word,
    Lines,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MouseSelection {
    anchor: SelectionPoint,
    focus: SelectionPoint,
    kind: SelectionKind,
    dragging: bool,
}

struct Terminal {
    active: bool,
    mouse_capture: bool,
}

impl Terminal {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        if let Err(error) = out.execute(EnterAlternateScreen) {
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        if let Err(error) = out.execute(EnableMouseCapture) {
            let _ = out.execute(LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }
        Ok(Self {
            active: true,
            mouse_capture: true,
        })
    }

    fn interactive(&self) -> bool {
        self.active
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.active {
            let mut out = io::stdout();
            if self.mouse_capture {
                let _ = out.execute(DisableMouseCapture);
            }
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
    resized: bool,
    mouse: VecDeque<MouseEvent>,
}

impl KeyReader {
    fn new(timed_input: bool) -> Self {
        Self {
            input: io::stdin(),
            pending: VecDeque::new(),
            timed_input,
            special: false,
            events: timed_input,
            resized: false,
            mouse: VecDeque::new(),
        }
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            input: io::stdin(),
            pending: bytes.iter().copied().collect(),
            timed_input: false,
            special: false,
            events: false,
            resized: false,
            mouse: VecDeque::new(),
        }
    }

    fn take_special(&mut self) -> bool {
        let special = self.special;
        self.special = false;
        special
    }

    /// Whether a resize was observed since this was last asked, clearing the
    /// record.
    ///
    /// The terminal destroys screen content on every resize — the alternate
    /// screen has no scrollback to restore rows from — so a caller that renders
    /// differentially has to repaint in full whenever this is true. Polling the
    /// size instead is not enough: a shrink and a matching grow that both land
    /// between two polls leave the size unchanged and the screen displaced.
    fn take_resized(&mut self) -> bool {
        let resized = self.resized;
        self.resized = false;
        resized
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn take_mouse(&mut self) -> Option<MouseEvent> {
        self.mouse.pop_front()
    }

    fn push_event(&mut self, event: Event) -> Option<u8> {
        match event {
            Event::Mouse(mouse) => {
                self.mouse.push_back(mouse);
                None
            }
            Event::Resize(_, _) => {
                self.resized = true;
                None
            }
            Event::Key(key) => self.push_event_key(key),
            _ => None,
        }
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
        let key = decode_escape_sequence(&sequence, final_byte);
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
        if let Some(byte) = self.pending.pop_front() {
            return Ok(Some(byte));
        }
        loop {
            if !blocking && !event::poll(std::time::Duration::from_millis(50))? {
                return Ok(None);
            }
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Esc {
                        return self.read_after_escape();
                    }
                    if let KeyCode::Char(prefix @ ('[' | 'O')) = key.code {
                        if key.modifiers.contains(KeyModifiers::ALT) {
                            return self.read_split_escape_sequence(prefix as u8);
                        }
                    }
                    if let Some(value) = self.push_event_key(key) {
                        return Ok(Some(value));
                    }
                    if !blocking {
                        return Ok(None);
                    }
                }
                Event::Mouse(mouse) => {
                    let _ = self.push_event(Event::Mouse(mouse));
                    if !blocking {
                        return Ok(None);
                    }
                }
                resize @ Event::Resize(_, _) => {
                    let _ = self.push_event(resize);
                    return Ok(None);
                }
                _ if !blocking => return Ok(None),
                _ => {}
            }
        }
    }

    fn read_after_escape(&mut self) -> io::Result<Option<u8>> {
        if !event::poll(std::time::Duration::from_millis(50))? {
            return Ok(Some(0x1b));
        }
        let event = event::read()?;
        if matches!(event, Event::Resize(_, _)) {
            let _ = self.push_event(event);
            return Ok(Some(0x1b));
        }
        if let Event::Mouse(mouse) = event {
            let _ = self.push_event(Event::Mouse(mouse));
            return Ok(Some(0x1b));
        }
        let Event::Key(key) = event else {
            return Ok(Some(0x1b));
        };
        if key.kind == KeyEventKind::Release {
            return Ok(Some(0x1b));
        }
        if let KeyCode::Char(prefix @ ('[' | 'O')) = key.code {
            return self.read_split_escape_sequence(prefix as u8);
        }
        if let Some(value) = self.push_event_key(key) {
            self.pending.push_back(value);
        }
        Ok(Some(0x1b))
    }

    fn read_split_escape_sequence(&mut self, prefix: u8) -> io::Result<Option<u8>> {
        let mut sequence = Vec::new();
        while sequence.len() < 32 && event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let KeyCode::Char(character) = key.code else {
                        break;
                    };
                    if !character.is_ascii() {
                        break;
                    }
                    let byte = character as u8;
                    sequence.push(byte);
                    if (0x40..=0x7e).contains(&byte) {
                        let key = decode_escape_sequence(&sequence, byte);
                        self.special = (0x80..=0x89).contains(&key);
                        return Ok(Some(key));
                    }
                }
                Event::Resize(_, _) => {
                    self.resized = true;
                    break;
                }
                Event::Mouse(mouse) => {
                    self.mouse.push_back(mouse);
                }
                _ => {}
            }
        }
        self.pending.push_back(prefix);
        self.pending.extend(sequence);
        Ok(Some(0x1b))
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
                if key.modifiers.contains(KeyModifiers::ALT) {
                    let mut encoded = [0u8; 4];
                    self.pending
                        .extend(character.encode_utf8(&mut encoded).bytes());
                    return Some(0x1b);
                } else if key.modifiers.contains(KeyModifiers::CONTROL) && character.is_ascii() {
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

fn decode_escape_sequence(sequence: &[u8], final_byte: u8) -> u8 {
    match final_byte {
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
    undo: Vec<Change>,
    redo: Vec<Change>,
    pending_change: Option<PendingChange>,
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
    rendered_rows: Vec<Vec<u8>>,
    rendered_size: (usize, usize),
    selection: Option<MouseSelection>,
    mouse_viewport_scrolled: bool,
    last_mouse_click: Option<(Instant, SelectionPoint, u8)>,
    syntax_highlighter: Option<Box<dyn SyntaxHighlighter>>,
    syntax_highlights: Vec<HighlightSpan>,
    syntax_preview_highlights: Vec<HighlightSpan>,
    syntax_line_offsets: Vec<usize>,
    syntax_line_offset_shifts: Vec<(usize, isize)>,
    syntax_highlights_dirty: bool,
    syntax_highlight_ready_at: Option<Instant>,
    syntax_highlight_request_pending: bool,
    syntax_preview_viewport: Option<(usize, usize)>,
    syntax_preview_range: Option<(usize, usize)>,
    syntax_buffer_snapshot: Option<Vec<u8>>,
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

    fn mark_syntax_highlighting_dirty(&mut self, defer: bool) {
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

    fn set_syntax_highlights(&mut self, highlights: Vec<HighlightSpan>) {
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

    fn set_syntax_preview(
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

    fn syntax_line_offset(&self, line: usize) -> usize {
        let offset = self.syntax_line_offsets[line];
        let adjustment = self
            .syntax_line_offset_shifts
            .iter()
            .filter(|(first_line, _)| *first_line <= line)
            .map(|(_, delta)| *delta)
            .sum();
        shift_offset(offset, adjustment)
    }

    fn adjust_syntax_highlights_for_edit(
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

    fn visible_byte_range(&self) -> Range<usize> {
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

    fn refresh_syntax_preview(&mut self, visible_range: Range<usize>) -> bool {
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

    fn refresh_syntax_preview_with_data(
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
    fn refresh_syntax_highlighting(
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

    fn state(&self) -> EditorState {
        EditorState {
            row: self.row,
            col: self.col,
            trailing_newline: self.trailing_newline,
            modified: self.modified,
        }
    }

    fn undo_status(&self, change: &Change) -> String {
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

    fn restore_state(&mut self, state: EditorState) {
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

    fn begin_change(&mut self) {
        if self.pending_change.is_none() {
            self.pending_change = Some(PendingChange {
                edits: Vec::new(),
                before: self.state(),
            });
        }
    }

    fn changed(&mut self) {
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

    fn end_change(&mut self) {
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

    fn replace_bytes(
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

    fn replace_lines(
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

    fn apply_change(&mut self, change: &Change, forward: bool) {
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
                self.set_bytes(&[]);
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
    fn force_redraw(&mut self) {
        self.rendered_rows.clear();
        self.rendered_size = (0, 0);
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

    fn clear_mouse_selection(&mut self) {
        self.selection = None;
        self.mouse_viewport_scrolled = false;
        self.last_mouse_click = None;
    }

    fn selection_bounds(&self) -> Option<(SelectionPoint, SelectionPoint)> {
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

    fn selection_offsets_on_line(&self, line: usize) -> Option<(usize, usize)> {
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

    fn selection_text(&self) -> Option<Vec<u8>> {
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

    fn mouse_point(&self, mouse: MouseEvent) -> Option<SelectionPoint> {
        let screen_row = mouse.row as usize;
        if screen_row >= self.body_rows() || self.lines.is_empty() {
            // The last row is the status row. Do not let a status click turn
            // into a selection of the last file line.
            return None;
        }
        let line = self.screen_top.saturating_add(screen_row);
        let line = line.min(self.lines.len().saturating_sub(1));
        if self.screen_top.saturating_add(screen_row) >= self.lines.len() {
            return Some(SelectionPoint {
                line,
                offset: self.lines[line].len(),
            });
        }
        let column = mouse.column as usize;
        let gutter = self.number_column_width();
        if column < gutter {
            return Some(SelectionPoint { line, offset: 0 });
        }
        let display_column = self
            .screen_left
            .saturating_add(column.saturating_sub(gutter));
        Some(SelectionPoint {
            line,
            offset: byte_offset_at_display_column(&self.lines[line], display_column, self.tabstop),
        })
    }

    fn mouse_click_count(&mut self, point: SelectionPoint) -> u8 {
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

    fn auto_scroll_for_mouse(&mut self, mouse: MouseEvent) {
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
        if column <= gutter {
            self.screen_left = self.screen_left.saturating_sub(1);
        } else if column.saturating_add(1) >= self.screen_cols {
            self.screen_left = self.screen_left.saturating_add(1);
        }
        self.mouse_viewport_scrolled = true;
    }

    fn word_bounds(&self, point: SelectionPoint) -> (SelectionPoint, SelectionPoint) {
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

    fn finish_mouse_selection(&mut self) -> Option<Vec<u8>> {
        let selection = self.selection.as_mut()?;
        if !selection.dragging {
            return None;
        }
        selection.dragging = false;
        self.selection_text()
    }

    fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Option<Vec<u8>> {
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

    fn sync_screen(&mut self) {
        let last_line = self.lines.len().saturating_sub(1);
        let body = self.body_rows();
        if self.selection.is_some() || self.mouse_viewport_scrolled {
            self.screen_top = self.screen_top.min(last_line);
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

    fn edit_status(&self) -> String {
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

    fn render(&mut self, prompt: Option<&str>) -> io::Result<()> {
        let mut out = io::stdout().lock();
        self.render_to(&mut out, prompt)
    }

    fn render_to<W: Write>(&mut self, out: &mut W, prompt: Option<&str>) -> io::Result<()> {
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
        for screen_line in 0..body {
            let index = self.screen_top + screen_line;
            let mut row = Vec::new();
            if index >= self.lines.len() {
                row.extend_from_slice(b"~\x1b[K");
                frame.push(row);
                continue;
            }
            if self.number {
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
            if syntax_enabled {
                write_highlighted_line_with_selection(
                    &mut row,
                    &self.lines[index],
                    self.syntax_line_offset(index),
                    &mut highlight_cursor,
                    &mut preview_cursor,
                    preview_range,
                    self.screen_left,
                    width,
                    self.tabstop,
                    selected,
                )?;
            } else {
                write_plain_line_with_selection(
                    &mut row,
                    &self.lines[index],
                    self.screen_left,
                    width,
                    self.tabstop,
                    selected,
                )?;
            }
            row.extend_from_slice(b"\x1b[K");
            frame.push(row);
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
            self.render(Some(&prompt))?;
        }
    }

    fn delete_lines(&mut self, start: usize, end: usize) {
        self.begin_change();
        let end = end.min(self.lines.len().saturating_sub(1));
        if start <= end {
            self.yank = self.lines[start..=end].to_vec();
            self.yank_linewise = true;
            let replacement = if start == 0 && end + 1 == self.lines.len() {
                vec![Vec::new()]
            } else {
                Vec::new()
            };
            self.replace_lines(start..end + 1, replacement);
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
            let at = at.min(self.lines.len());
            self.replace_lines(at..at, self.yank.clone());
            self.row = at.min(self.lines.len() - 1);
            self.col = 0;
        } else {
            let col = if after { self.col + 1 } else { self.col };
            let col = col.min(self.lines[self.row].len());
            self.replace_bytes(self.row, col..col, self.yank[0].clone());
            self.col = col.min(self.lines[self.row].len().saturating_sub(1));
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
                            let at = at.min(self.lines.len());
                            self.begin_change();
                            self.replace_lines(at..at, inserted);
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
        if !will_change {
            self.set_error("No match");
            return;
        }
        self.begin_change();
        for row in first_line..=final_line {
            let (replacement, count) =
                replace_pattern_case(&self.lines[row], &old, &new, global, ignorecase);
            if count != 0 {
                let length = self.lines[row].len();
                self.replace_bytes(row, 0..length, replacement);
                changed += count;
                changed_lines += 1;
            }
        }
        if changed > 1 {
            self.set_status(format!(
                "{} substitutions on {} lines",
                changed, changed_lines
            ));
        }
    }

    fn handle_command(&mut self, reader: &mut KeyReader, key: u8) -> io::Result<()> {
        if key == 0x1b {
            return Ok(());
        }
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
            if let Some(change) = self.undo.pop() {
                self.apply_change(&change, false);
                let status = self.undo_status(&change);
                self.redo.push(change);
                self.set_status(status);
            } else {
                self.set_status("Already at oldest change");
            }
            return Ok(());
        }
        if key == 18 {
            if let Some(change) = self.redo.pop() {
                self.apply_change(&change, true);
                self.undo.push(change);
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
                let removed =
                    self.replace_bytes(self.row, self.col..self.lines[self.row].len(), Vec::new());
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
                let length = self.lines[self.row].len();
                self.yank = vec![self.replace_bytes(self.row, 0..length, Vec::new())];
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
                let end = self
                    .col
                    .saturating_add(count)
                    .min(self.lines[self.row].len());
                let removed = self.replace_bytes(self.row, self.col..end, Vec::new());
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
                self.replace_lines(at..at, vec![Vec::new()]);
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
                    let end = self
                        .col
                        .saturating_add(count)
                        .min(self.lines[self.row].len());
                    if self.col < end {
                        self.replace_bytes(
                            self.row,
                            self.col..end,
                            vec![replacement; end - self.col],
                        );
                        self.col = end;
                    }
                    self.col = self.col.saturating_sub(1);
                    self.end_change();
                }
            }
            b'x' => {
                self.begin_change();
                let end = self
                    .col
                    .saturating_add(count)
                    .min(self.lines[self.row].len());
                let removed = self.replace_bytes(self.row, self.col..end, Vec::new());
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
                let removed =
                    self.replace_bytes(self.row, self.col..self.lines[self.row].len(), Vec::new());
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
                    self.replace_bytes(self.row, self.col..self.col + 1, Vec::new());
                }
                self.end_change();
            }
            b'J' => {
                self.begin_change();
                if self.row + 1 < self.lines.len() {
                    let mut joined = self.lines[self.row].clone();
                    if !joined.is_empty() {
                        joined.push(b' ');
                    }
                    joined.extend_from_slice(&self.lines[self.row + 1]);
                    self.replace_lines(self.row..self.row + 2, vec![joined]);
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
                    if let Some(&byte) = self.lines[self.row].get(self.col) {
                        let replacement = if byte.is_ascii_lowercase() {
                            byte.to_ascii_uppercase()
                        } else if byte.is_ascii_uppercase() {
                            byte.to_ascii_lowercase()
                        } else {
                            byte
                        };
                        if replacement != byte {
                            self.replace_bytes(self.row, self.col..self.col + 1, vec![replacement]);
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
                    for row in self.row..last {
                        let mut line = self.lines[row].clone();
                        if command == b'>' {
                            line.splice(0..0, *b"\t");
                        } else if line.first() == Some(&b'\t') {
                            line.remove(0);
                        } else {
                            let spaces = line.iter().take_while(|byte| **byte == b' ').count();
                            let remove = spaces.min(self.tabstop);
                            line.drain(..remove);
                        }
                        let old_len = self.lines[row].len();
                        self.replace_bytes(row, 0..old_len, line);
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
                if let Some(change) = self.undo.pop() {
                    self.apply_change(&change, false);
                    self.redo.push(change);
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
            // Vi's redraw. The screen is repainted by the render that follows
            // every command, so all this has to do is drop the differential
            // renderer's cache — which is also the only way out of a desync
            // this editor did not cause.
            12 => self.force_redraw(),
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
                    self.replace_bytes(self.row, self.col..self.col + 1, Vec::new());
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
            self.replace_bytes(row, first..last.min(line_len - 1) + 1, Vec::new());
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
            self.col = self
                .col
                .saturating_sub(1)
                .min(self.lines[self.row].len().saturating_sub(1));
            self.end_change();
            return Ok(());
        }
        if key == b'\r' || key == b'\n' {
            self.begin_change();
            let current = self.lines[self.row].clone();
            let rest = current[self.col..].to_vec();
            let indent = if self.autoindent {
                current
                    .iter()
                    .take_while(|byte| **byte == b' ' || **byte == b'\t')
                    .copied()
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let first_line = current[..self.col].to_vec();
            let old_row = self.row;
            self.row += 1;
            let mut next_line = indent.clone();
            next_line.extend(rest);
            self.replace_lines(old_row..old_row + 1, vec![first_line, next_line]);
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
                self.replace_bytes(self.row, self.col..self.col + 1, Vec::new());
            } else if self.row > 0 {
                let current = self.lines[self.row].clone();
                self.row -= 1;
                let mut joined = self.lines[self.row].clone();
                self.col = joined.len();
                joined.extend(current);
                self.replace_lines(self.row..self.row + 2, vec![joined]);
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
                self.replace_bytes(self.row, self.col..self.col + 1, Vec::new());
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
            self.replace_bytes(self.row, self.col..self.col, vec![b' '; spaces]);
            self.col += spaces;
            if !self.replaying {
                self.last_change.push(key);
            }
            return Ok(());
        }
        if key >= 0x20 && key != 0x7f && key != 0x9b {
            let byte = key;
            self.begin_change();
            if self.mode == Mode::Replace && self.col < self.lines[self.row].len() {
                self.replace_bytes(self.row, self.col..self.col + 1, vec![byte]);
            } else {
                self.replace_bytes(self.row, self.col..self.col, vec![byte]);
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
                let mut handled_mouse = false;
                while let Some(mouse) = reader.take_mouse() {
                    handled_mouse = true;
                    if let Some(text) = self.handle_mouse_event(mouse) {
                        copy_selection_to_clipboards(&text);
                    }
                }
                if handled_mouse {
                    if reader.take_resized() {
                        self.force_redraw();
                    }
                    self.refresh_size();
                    self.render(None)?;
                    continue;
                }
                let visible_range = (self.syntax_highlights_dirty
                    || self.syntax_highlight_request_pending)
                    .then(|| self.visible_byte_range());
                let syntax_updated = self.refresh_syntax_highlighting(false, visible_range);
                let resized = reader.take_resized();
                if resized {
                    self.force_redraw();
                }
                // `refresh_size` is deliberately not the only trigger: a resize
                // that ends where it started still moved the screen's content.
                if self.refresh_size() || resized || syntax_updated {
                    self.render(None)?;
                }
                continue;
            };
            // A release can be lost by a terminal or multiplexer while a
            // mouse drag is in progress. The next keyboard event is an
            // unambiguous end of that drag, so finalize it before handling
            // the key and retain the usual keyboard-only editor contract.
            if let Some(text) = self.finish_mouse_selection() {
                copy_selection_to_clipboards(&text);
            }
            if self.selection.is_some() || self.mouse_viewport_scrolled {
                self.clear_mouse_selection();
            }
            let special = reader.take_special();
            if self.hit_return {
                if matches!(key, b'\r' | b'\n') {
                    self.hit_return = false;
                    self.clear_status();
                }
                if reader.take_resized() {
                    self.force_redraw();
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
            // Taken after the command rather than before it, so a resize seen
            // while a multi-key command was blocking for its operand counts
            // towards this frame instead of the next one.
            if reader.take_resized() {
                self.force_redraw();
            }
            self.refresh_size();
            self.render(None)?;
        }
        let mut out = io::stdout().lock();
        write!(out, "\x1b[?25h\x1b[{};1H\x1b[K", self.screen_rows)?;
        out.flush()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClipboardDestination {
    Clipboard,
    Primary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClipboardHelperCommand {
    program: &'static str,
    args: &'static [&'static str],
}

fn clipboard_helper_commands(destination: ClipboardDestination) -> Vec<ClipboardHelperCommand> {
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

fn write_osc52_clipboard<W: Write>(out: &mut W, text: &[u8]) -> io::Result<()> {
    out.execute(CopyToClipboard::to_clipboard_from(text))?;
    #[cfg(unix)]
    out.execute(CopyToClipboard::to_primary_from(text))?;
    out.flush()
}

fn run_clipboard_helper(command: ClipboardHelperCommand, text: &[u8]) -> bool {
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

fn copy_selection_to_clipboards(text: &[u8]) {
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

fn line_offsets(lines: &[Vec<u8>]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for line in lines {
        offsets.push(offset);
        offset += line.len() + 1;
    }
    offsets
}

fn shift_offset(offset: usize, delta: isize) -> usize {
    if delta >= 0 {
        offset.saturating_add(delta as usize)
    } else {
        offset.saturating_sub(delta.unsigned_abs())
    }
}

struct HighlightCursor<'a> {
    spans: &'a [HighlightSpan],
    index: usize,
}

impl<'a> HighlightCursor<'a> {
    fn new(spans: &'a [HighlightSpan]) -> Self {
        Self { spans, index: 0 }
    }

    fn style_at(&mut self, offset: usize) -> Option<HighlightStyle> {
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

fn fragment_len(byte: u8, display_column: usize, tabstop: usize) -> usize {
    if byte == b'\t' {
        let tabstop = tabstop.max(1);
        tabstop - (display_column % tabstop)
    } else if byte.is_ascii_control() {
        2
    } else {
        1
    }
}

fn byte_offset_at_display_column(line: &[u8], target: usize, tabstop: usize) -> usize {
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

fn write_fragment<W: Write>(out: &mut W, byte: u8, start: usize, stop: usize) -> io::Result<()> {
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
fn write_rendered_row<W: Write>(out: &mut W, row: &[u8]) -> io::Result<()> {
    let row = String::from_utf8_lossy(row);
    out.write_all(row.as_bytes())
}

#[cfg(not(windows))]
fn write_rendered_row<W: Write>(out: &mut W, row: &[u8]) -> io::Result<()> {
    out.write_all(row)
}

fn write_line_number<W: Write>(
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
fn write_plain_line<W: Write>(
    out: &mut W,
    line: &[u8],
    screen_left: usize,
    width: usize,
    tabstop: usize,
) -> io::Result<()> {
    write_plain_line_with_selection(out, line, screen_left, width, tabstop, None)
}

fn write_plain_line_with_selection<W: Write>(
    out: &mut W,
    line: &[u8],
    screen_left: usize,
    width: usize,
    tabstop: usize,
    selection: Option<(usize, usize)>,
) -> io::Result<()> {
    let visible_end = screen_left.saturating_add(width);
    let mut display_column = 0;
    let mut selected = false;

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
        }
        display_column = fragment_end;
        if display_column >= visible_end {
            break;
        }
    }
    if selected {
        out.write_all(b"\x1b[0m")?;
    }
    Ok(())
}

#[cfg(test)]
fn write_highlighted_line<W: Write>(
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
fn write_highlighted_line_with_preview<W: Write>(
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
}

#[allow(clippy::too_many_arguments)]
fn write_highlighted_line_with_selection<W: Write>(
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
) -> io::Result<()> {
    let visible_end = screen_left.saturating_add(width);
    let mut display_column = 0;
    let mut active_style = None;
    let mut active_selection = false;

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
        }
        display_column = fragment_end;
        if display_column >= visible_end {
            break;
        }
    }
    if active_style.is_some() || active_selection {
        write!(out, "\x1b[0m")?;
    }
    Ok(())
}

fn find_pattern_before(text: &[u8], pattern: &[u8], before: usize) -> Option<usize> {
    let mut context = MatchContext::new(pattern);
    (0..=before.min(text.len())).rfind(|start| {
        match_pattern_at_with_context(text, *start, pattern, 0, &mut context).is_some()
    })
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

#[derive(Clone, PartialEq, Eq, Hash)]
struct MatchState {
    text_at: usize,
    pattern_at: usize,
    captures: Captures,
    stack: Vec<usize>,
}

struct MatchContext {
    failed: HashSet<MatchState>,
    #[cfg(test)]
    evaluated_states: usize,
}

impl MatchContext {
    fn new(_pattern: &[u8]) -> Self {
        Self {
            failed: HashSet::new(),
            #[cfg(test)]
            evaluated_states: 0,
        }
    }
}

fn group_number(pattern: &[u8], at: usize) -> usize {
    pattern[..at]
        .windows(2)
        .filter(|pair| *pair == *b"\\(")
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
    context: &mut MatchContext,
) -> Option<(usize, Captures)> {
    let state = MatchState {
        text_at,
        pattern_at,
        captures,
        stack: stack.clone(),
    };
    if context.failed.contains(&state) {
        return None;
    }
    #[cfg(test)]
    {
        context.evaluated_states += 1;
    }
    let result =
        match_pattern_captures_inner(text, text_at, pattern, pattern_at, captures, stack, context);
    if result.is_none() {
        context.failed.insert(state);
    }
    result
}

fn match_pattern_captures_inner(
    text: &[u8],
    text_at: usize,
    pattern: &[u8],
    pattern_at: usize,
    captures: Captures,
    stack: Vec<usize>,
    context: &mut MatchContext,
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
        return match_pattern_captures(
            text,
            text_at,
            pattern,
            pattern_at + 2,
            captures,
            stack,
            context,
        );
    }
    if pattern.get(pattern_at..pattern_at + 2) == Some(b"\\)") {
        let mut captures = captures;
        let mut stack = stack;
        if let Some(group) = stack.pop() {
            if let Some((start, _)) = captures[group] {
                captures[group] = Some((start, text_at));
            }
        }
        return match_pattern_captures(
            text,
            text_at,
            pattern,
            pattern_at + 2,
            captures,
            stack,
            context,
        );
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
                context,
            );
        }
        return None;
    }
    if pattern[pattern_at] == b'^' {
        return if text_at == 0 {
            match_pattern_captures(
                text,
                text_at,
                pattern,
                pattern_at + 1,
                captures,
                stack,
                context,
            )
        } else {
            None
        };
    }
    if pattern[pattern_at] == b'$' && pattern_at + 1 == pattern.len() {
        return (text_at == text.len()).then_some((text_at, captures));
    }
    let (token, next) = pattern_token(pattern, pattern_at)?;
    if pattern.get(next) == Some(&b'*') {
        if text
            .get(text_at)
            .is_some_and(|byte| token_matches(&token, *byte))
        {
            if let Some(result) = match_pattern_captures(
                text,
                text_at + 1,
                pattern,
                pattern_at,
                captures,
                stack.clone(),
                context,
            ) {
                return Some(result);
            }
        }
        return match_pattern_captures(text, text_at, pattern, next + 1, captures, stack, context);
    }
    if text
        .get(text_at)
        .is_some_and(|byte| token_matches(&token, *byte))
    {
        match_pattern_captures(text, text_at + 1, pattern, next, captures, stack, context)
    } else {
        None
    }
}

fn match_pattern_at_with_context(
    text: &[u8],
    text_at: usize,
    pattern: &[u8],
    pattern_at: usize,
    context: &mut MatchContext,
) -> Option<usize> {
    match_pattern_captures(
        text,
        text_at,
        pattern,
        pattern_at,
        [None; 10],
        Vec::new(),
        context,
    )
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
    let mut context = MatchContext::new(pattern);
    starts.find(|&start| {
        match_pattern_at_with_context(text, start, pattern, 0, &mut context).is_some()
    })
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
        let mut context = MatchContext::new(pattern_for_matching);
        let Some((end, captures)) = match_pattern_captures(
            line_for_matching,
            start,
            pattern_for_matching,
            0,
            [None; 10],
            Vec::new(),
            &mut context,
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

fn serialized_lines_len(lines: &[Vec<u8>]) -> usize {
    lines.iter().map(|line| line.len() + 1).sum()
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
    run_with_editor_setup(arguments, |_| {})
}

/// Run vi after allowing an embedding application to configure each editor.
///
/// The callback is intentionally expressed only in terms of Editor. This
/// keeps the vi core independent of parsers, grammars, and themes while still
/// allowing a host application to install an optional SyntaxHighlighter.
pub fn run_with_editor_setup(
    arguments: Vec<String>,
    mut setup_editor: impl FnMut(&mut Editor),
) -> i32 {
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
        setup_editor(&mut editor);
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

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
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
        assert_eq!(
            encoded,
            b"\x1b]52;c;b25lCnR3bw==\x1b\\\x1b]52;p;b25lCnR3bw==\x1b\\"
        );
    }

    #[test]
    fn platform_clipboard_helpers_use_argument_vectors() {
        let clipboard = clipboard_helper_commands(ClipboardDestination::Clipboard);
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
        write_rendered_row(&mut rendered, b"\x1b[1;1H\xe2\x9d")
            .expect("render a clipped unicode row");

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

        editor
            .execute_keys(b"jiXY\x1b")
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
}

// Core behavioral tests live in tests/editor_core.rs so the editor source
// remains focused on implementation and terminal behavior.
