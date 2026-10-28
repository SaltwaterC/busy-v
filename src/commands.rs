use super::*;

impl Editor {
    pub(super) fn delete_lines(&mut self, start: usize, end: usize) {
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

    pub(super) fn motion(&self, command: u8, count: usize) -> (usize, usize) {
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

    pub(super) fn put(&mut self, after: bool, register: Option<u8>) {
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

    pub(super) fn save_register(&mut self, register: Option<u8>) {
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

    pub(super) fn search_from(&mut self, pattern: &[u8], direction: i32) -> Option<bool> {
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

    pub(super) fn report_search(&mut self, result: Option<bool>, direction: i32) {
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

    pub(super) fn find_pattern(&self, text: &[u8], pattern: &[u8], from: usize) -> Option<usize> {
        if self.ignorecase {
            let folded_text = fold_ascii(text);
            let folded_pattern = fold_ascii(pattern);
            find_pattern(&folded_text, &folded_pattern, from)
        } else {
            find_pattern(text, pattern, from)
        }
    }

    pub(super) fn find_pattern_before(
        &self,
        text: &[u8],
        pattern: &[u8],
        before: usize,
    ) -> Option<usize> {
        if self.ignorecase {
            let folded_text = fold_ascii(text);
            let folded_pattern = fold_ascii(pattern);
            find_pattern_before(&folded_text, &folded_pattern, before)
        } else {
            find_pattern_before(text, pattern, before)
        }
    }

    pub(super) fn find_character(&self, target: u8, direction: i32, till: bool) -> Option<usize> {
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

    pub(super) fn matching_delimiter(&self) -> Option<usize> {
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

    pub(super) fn read_motion(
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

    pub(super) fn command_prompt(&mut self, reader: &mut KeyReader) -> io::Result<()> {
        let Some(raw) = self.prompt(reader, ":")? else {
            return Ok(());
        };
        let command = String::from_utf8_lossy(&raw);
        self.execute_ex(&command);
        Ok(())
    }

    pub(super) fn no_write_since_last_change(&mut self, command: &str) {
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
                            if !self.status_highlighted {
                                self.set_status(self.current_file_status(new_file, true));
                            }
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
            name if name.len() > 1 && command_prefix(name, "set") => {
                // A trailing `!` toggles the last option (`:set wrap!`)
                // rather than forcing the command.
                if force && !args.is_empty() && command.ends_with('!') {
                    self.set_options(&format!("{args}!"));
                } else {
                    self.set_options(args);
                }
            }
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

    pub(super) fn set_options(&mut self, args: &str) {
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
        let mut queries = Vec::new();
        for option in args.split_whitespace() {
            if let Some(name) = option.strip_suffix('?') {
                if let Some((name, flag)) = self.boolean_option(name) {
                    queries.push(format!("{}{name}", if *flag { "" } else { "no" }));
                } else {
                    match name {
                        "tabstop" | "ts" => queries.push(format!("tabstop={}", self.tabstop)),
                        "textwidth" | "tw" => queries.push(format!("textwidth={}", self.textwidth)),
                        "modelines" | "mls" => {
                            queries.push(format!("modelines={}", self.modelines))
                        }
                        _ => self.set_error(format!("bad option: {}", option)),
                    }
                }
                continue;
            }
            // `name` sets a boolean option, `noname` clears it and `name!`
            // toggles it, as in Vim.
            if let Some((_, flag)) = self.boolean_option(option) {
                *flag = true;
                continue;
            }
            if let Some((_, flag)) = option
                .strip_suffix('!')
                .and_then(|name| self.boolean_option(name))
            {
                *flag = !*flag;
                continue;
            }
            if let Some((_, flag)) = option
                .strip_prefix("no")
                .and_then(|name| self.boolean_option(name))
            {
                *flag = false;
                continue;
            }
            match option {
                value if value.starts_with("modelines=") || value.starts_with("mls=") => {
                    let (_, count) = value.split_once('=').unwrap_or_default();
                    if let Ok(n) = count.parse::<usize>() {
                        self.modelines = n;
                    } else {
                        self.set_error(format!("bad option: {}", option));
                    }
                }
                value if value.starts_with("textwidth=") || value.starts_with("tw=") => {
                    let (_, width) = value.split_once('=').unwrap_or_default();
                    if let Ok(n) = width.parse::<usize>() {
                        self.textwidth = n;
                    } else {
                        self.set_error(format!("bad option: {}", option));
                    }
                }
                value if value.starts_with("tabstop=") || value.starts_with("ts=") => {
                    let (_, width) = value.split_once('=').unwrap_or_default();
                    if let Ok(n) = width.parse::<usize>() {
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
        if !queries.is_empty() && !self.status_highlighted {
            self.set_status(queries.join(" "));
        }
    }

    pub(super) fn boolean_option(&mut self, name: &str) -> Option<(&'static str, &mut bool)> {
        Some(match name {
            "number" | "nu" => ("number", &mut self.number),
            "wrap" => ("wrap", &mut self.wrap),
            "linebreak" | "lbr" => ("linebreak", &mut self.linebreak),
            "autoindent" | "ai" => ("autoindent", &mut self.autoindent),
            "expandtab" | "et" => ("expandtab", &mut self.expandtab),
            "flash" | "fl" => ("flash", &mut self.flash),
            "ignorecase" | "ic" => ("ignorecase", &mut self.ignorecase),
            "showmatch" | "sm" => ("showmatch", &mut self.showmatch),
            "readonly" | "ro" => ("readonly", &mut self.readonly),
            "modeline" | "ml" => ("modeline", &mut self.modeline),
            _ => return None,
        })
    }

    pub(super) fn substitute(&mut self, args: &str, addresses: Option<(usize, usize)>) {
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

    pub(super) fn handle_command(&mut self, reader: &mut KeyReader, key: u8) -> io::Result<()> {
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
                self.row = self
                    .screen_row_source(self.body_rows() / 2)
                    .map_or(self.lines.len() - 1, |(line, _, _)| line);
                self.col = self.col.min(self.lines[self.row].len().saturating_sub(1));
            }
            b'L' => {
                self.row = self.last_visible_line();
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

    pub(super) fn apply_operator(
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

    pub(super) fn apply_char_operator(
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

    pub(super) fn handle_insert(
        &mut self,
        reader: &mut KeyReader,
        key: u8,
        special: bool,
    ) -> io::Result<()> {
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
            if self.textwidth > 0 && byte != b' ' {
                self.wrap_at_textwidth();
            }
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

    /// Vim's auto-wrap for `:set textwidth`: once the character just typed
    /// ends past the margin, break the line at the last blank before the
    /// cursor that keeps the line within the margin (or the first blank, when
    /// none does), carrying the rest with any autoindent onto a new line.
    pub(super) fn wrap_at_textwidth(&mut self) {
        let blank = |byte: u8| byte == b' ' || byte == b'\t';
        loop {
            let line = &self.lines[self.row];
            if self.line_display_width(line, self.col) <= self.textwidth {
                return;
            }
            let indent = line.iter().take_while(|byte| blank(**byte)).count();
            let mut first = None;
            let mut fitting = None;
            let mut display_column = self.line_display_width(line, indent);
            let mut offset = indent;
            while offset < self.col {
                if blank(line[offset]) {
                    let start = offset;
                    let fits = display_column <= self.textwidth;
                    while offset < self.col && blank(line[offset]) {
                        display_column += fragment_len(line[offset], display_column, self.tabstop);
                        offset += 1;
                    }
                    first.get_or_insert((start, offset));
                    if fits {
                        fitting = Some((start, offset));
                    }
                } else {
                    display_column += fragment_len(line[offset], display_column, self.tabstop);
                    offset += 1;
                }
            }
            // A blank right before the cursor has nothing after it to carry.
            let Some((start, end)) = fitting.or(first).filter(|(_, end)| *end < self.col) else {
                return;
            };
            let head = line[..start].to_vec();
            let mut tail = if self.autoindent {
                line[..indent].to_vec()
            } else {
                Vec::new()
            };
            let carried = tail.len();
            tail.extend_from_slice(&line[end..]);
            let old_row = self.row;
            self.row += 1;
            self.col = self.col - end + carried;
            self.replace_lines(old_row..old_row + 1, vec![head, tail]);
        }
    }
}
