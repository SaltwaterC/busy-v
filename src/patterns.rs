use super::*;

pub(super) fn find_pattern_before(text: &[u8], pattern: &[u8], before: usize) -> Option<usize> {
    let mut context = MatchContext::new(pattern);
    (0..=before.min(text.len())).rfind(|start| {
        match_pattern_at_with_context(text, *start, pattern, 0, &mut context).is_some()
    })
}

pub(super) fn fold_ascii(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|byte| byte.to_ascii_lowercase()).collect()
}

#[derive(Clone)]
pub(super) enum PatternToken {
    Literal(u8),
    Any,
    Class(Vec<(u8, u8)>, bool),
}

pub(super) fn pattern_token(pattern: &[u8], at: usize) -> Option<(PatternToken, usize)> {
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

pub(super) fn token_matches(token: &PatternToken, byte: u8) -> bool {
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

pub(super) type Captures = [Option<(usize, usize)>; 10];

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct MatchState {
    text_at: usize,
    pattern_at: usize,
    captures: Captures,
    stack: Vec<usize>,
}

pub(super) struct MatchContext {
    failed: HashSet<MatchState>,
    #[cfg(test)]
    pub(super) evaluated_states: usize,
}

impl MatchContext {
    pub(super) fn new(_pattern: &[u8]) -> Self {
        Self {
            failed: HashSet::new(),
            #[cfg(test)]
            evaluated_states: 0,
        }
    }
}

pub(super) fn group_number(pattern: &[u8], at: usize) -> usize {
    pattern[..at]
        .windows(2)
        .filter(|pair| *pair == *b"\\(")
        .count()
        .saturating_add(1)
        .min(9)
}

pub(super) fn match_pattern_captures(
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

pub(super) fn match_pattern_captures_inner(
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

pub(super) fn match_pattern_at_with_context(
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

pub(super) fn find_pattern(text: &[u8], pattern: &[u8], from: usize) -> Option<usize> {
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

pub(super) fn replace_pattern_case(
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

pub(super) fn find_delimiter(bytes: &[u8], from: usize, delimiter: u8) -> Option<usize> {
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

pub(super) fn unescape_pattern(bytes: &[u8]) -> Vec<u8> {
    unescape_pattern_with_delimiter(bytes, b'/')
}

pub(super) fn unescape_pattern_with_delimiter(bytes: &[u8], delimiter: u8) -> Vec<u8> {
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

pub(super) fn unescape_replacement_with_delimiter(bytes: &[u8], delimiter: u8) -> Vec<u8> {
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

pub(super) fn append_replacement(
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

pub(super) fn display_literal_bytes(line: &[u8]) -> Vec<u8> {
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

pub(super) fn display_command_byte(byte: u8) -> String {
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

pub(super) fn pattern_error(pattern: &[u8]) -> Option<&'static str> {
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

pub(super) fn pattern_is_valid(pattern: &[u8]) -> bool {
    pattern_error(pattern).is_none()
}
