use super::*;

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

pub(super) struct KeyReader {
    pub(super) input: io::Stdin,
    pub(super) pending: VecDeque<u8>,
    pub(super) timed_input: bool,
    pub(super) buffered_only: bool,
    pub(super) special: bool,
    pub(super) events: bool,
    pub(super) resized: bool,
    pub(super) mouse: VecDeque<MouseEvent>,
}

impl KeyReader {
    pub(super) fn new(timed_input: bool) -> Self {
        Self {
            input: io::stdin(),
            pending: VecDeque::new(),
            timed_input,
            buffered_only: false,
            special: false,
            events: timed_input,
            resized: false,
            mouse: VecDeque::new(),
        }
    }

    pub(super) fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            input: io::stdin(),
            pending: bytes.iter().copied().collect(),
            timed_input: false,
            buffered_only: true,
            special: false,
            events: false,
            resized: false,
            mouse: VecDeque::new(),
        }
    }

    pub(super) fn take_special(&mut self) -> bool {
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
    pub(super) fn take_resized(&mut self) -> bool {
        let resized = self.resized;
        self.resized = false;
        resized
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(super) fn take_mouse(&mut self) -> Option<MouseEvent> {
        self.mouse.pop_front()
    }

    pub(super) fn push_event(&mut self, event: Event) -> Option<u8> {
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

    pub(super) fn read_more(&mut self) -> io::Result<bool> {
        let mut bytes = [0u8; 64];
        let size = self.input.read(&mut bytes)?;
        if size == 0 {
            return Ok(false);
        }
        self.pending.extend(bytes[..size].iter().copied());
        Ok(true)
    }

    pub(super) fn try_byte(&mut self) -> io::Result<Option<u8>> {
        if let Some(byte) = self.pending.pop_front() {
            return Ok(Some(byte));
        }
        if self.buffered_only {
            return Ok(None);
        }
        if !self.read_more()? {
            if self.timed_input {
                return Ok(None);
            }
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "input closed"));
        }
        Ok(self.pending.pop_front())
    }

    pub(super) fn timed_byte(&mut self) -> io::Result<Option<u8>> {
        if let Some(byte) = self.pending.pop_front() {
            return Ok(Some(byte));
        }
        if self.buffered_only {
            return Ok(None);
        }
        if self.read_more()? {
            Ok(self.pending.pop_front())
        } else {
            Ok(None)
        }
    }

    pub(super) fn try_key(&mut self) -> io::Result<Option<u8>> {
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

    pub(super) fn key(&mut self) -> io::Result<u8> {
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

    pub(super) fn try_event_key(&mut self, blocking: bool) -> io::Result<Option<u8>> {
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

    pub(super) fn read_after_escape(&mut self) -> io::Result<Option<u8>> {
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

    pub(super) fn read_split_escape_sequence(&mut self, prefix: u8) -> io::Result<Option<u8>> {
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

    pub(super) fn push_event_key(&mut self, key: KeyEvent) -> Option<u8> {
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

pub(super) fn decode_escape_sequence(sequence: &[u8], final_byte: u8) -> u8 {
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

impl Editor {
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
