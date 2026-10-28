use super::*;

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
