use super::*;

pub(super) fn command_prefix(input: &str, command: &str) -> bool {
    !input.is_empty() && command.starts_with(input)
}

pub(super) fn register_status(
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

pub(super) fn matching_position(line: &[u8], position: usize) -> Option<usize> {
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

pub(super) fn parse_address(
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

pub(super) type ParsedEx<'a> = (Option<(usize, usize)>, &'a str, &'a str);

pub(super) fn parse_ex<'a>(
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

pub(super) fn is_zero_base(command: &str) -> Option<&[u8]> {
    let command = command.trim_start().as_bytes();
    (command.first() == Some(&b'0') && !command.get(1).is_some_and(u8::is_ascii_digit))
        .then_some(command)
}

pub(super) fn is_bare_zero_address(command: &str) -> bool {
    let Some(command) = is_zero_base(command) else {
        return false;
    };
    let mut position = 1;
    while command.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    command.get(position) == Some(&b'=')
}

pub(super) fn is_zero_read_address(command: &str) -> bool {
    let Some(command) = is_zero_base(command) else {
        return false;
    };
    let mut position = 1;
    while command.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    matches!(command.get(position), Some(b'r') | Some(b',') | Some(b';'))
}
