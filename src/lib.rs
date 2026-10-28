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
    wrap: bool,
    linebreak: bool,
    textwidth: usize,
    autoindent: bool,
    expandtab: bool,
    flash: bool,
    ignorecase: bool,
    showmatch: bool,
    tabstop: usize,
    modeline: bool,
    modelines: usize,
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
    /// Rows of the `screen_top` line hidden above the screen. Only a wrapped
    /// cursor line taller than the screen scrolls within itself.
    wrap_skip: usize,
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

mod app;
mod commands;
mod ex;
mod helpers;
mod input;
mod patterns;
mod state;
mod view;

use ex::*;
use helpers::*;
use input::*;
use patterns::*;
use view::*;

pub use app::{run, run_with_editor_setup};

#[cfg(test)]
#[path = "terminal_event_tests.rs"]
mod terminal_event_tests;

// Core behavioral tests live in tests/editor_core.rs so the editor source
// remains focused on implementation and terminal behavior.
