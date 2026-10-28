use super::*;

pub(super) fn line_offsets(lines: &[Vec<u8>]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for line in lines {
        offsets.push(offset);
        offset += line.len() + 1;
    }
    offsets
}

pub(super) fn shift_offset(offset: usize, delta: isize) -> usize {
    if delta >= 0 {
        offset.saturating_add(delta as usize)
    } else {
        offset.saturating_sub(delta.unsigned_abs())
    }
}

pub(super) fn line_count(data: &[u8]) -> usize {
    if data.is_empty() {
        0
    } else {
        data.split(|byte| *byte == b'\n').count() - usize::from(data.ends_with(b"\n"))
    }
}

pub(super) fn serialized_lines_len(lines: &[Vec<u8>]) -> usize {
    lines.iter().map(|line| line.len() + 1).sum()
}

pub(super) fn format_file_status(
    name: &str,
    data: &[u8],
    new_file: bool,
    readonly: bool,
) -> String {
    format!(
        "'{}'{}{} {}L, {}C",
        name,
        if new_file { " [New file]" } else { "" },
        if readonly { " [Readonly]" } else { "" },
        line_count(data),
        data.len()
    )
}

pub(super) fn register_name(register: Option<u8>) -> char {
    register.map(char::from).unwrap_or('D')
}

/// Extract only option settings; modelines never enter the Ex command parser.
pub(super) fn modeline_options(line: &[u8]) -> Option<Result<String, &'static str>> {
    let line = String::from_utf8_lossy(line);
    for (at, _) in line.char_indices() {
        if at > 0 && !matches!(line.as_bytes()[at - 1], b' ' | b'\t') {
            continue;
        }
        let rest = &line[at..];
        let (rest, require_set) = if let Some(rest) = rest.strip_prefix("vim:") {
            (rest, false)
        } else if let Some(rest) = rest.strip_prefix("vi:") {
            (rest, false)
        } else if at > 0 {
            if let Some(rest) = rest.strip_prefix("ex:") {
                (rest, false)
            } else if let Some(rest) = rest.strip_prefix("Vim:") {
                (rest, true)
            } else {
                continue;
            }
        } else {
            continue;
        };
        let rest = rest.trim_start_matches([' ', '\t']);
        let settings = rest.strip_prefix("set").or_else(|| {
            if require_set {
                None
            } else {
                rest.strip_prefix("se")
            }
        });
        if let Some(settings) = settings.filter(|s| s.starts_with([' ', '\t'])) {
            return Some(
                settings
                    .split_once(':')
                    .map(|(options, _)| options.to_owned())
                    .ok_or("missing ':' after set"),
            );
        }
        if !require_set {
            return Some(Ok(rest.replace(':', " ")));
        }
    }
    None
}

pub(super) fn startup_commands() -> (Vec<String>, Option<String>) {
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
