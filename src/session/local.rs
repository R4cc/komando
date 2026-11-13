//! Local Sessions: a shell on this machine (PowerShell, cmd) through the pseudo-console.
//!
//! On Windows this is ConPTY; alacritty_terminal's PTY event loop reads it on its own
//! thread and parses straight into the shared grid.

use std::collections::HashMap;

use alacritty_terminal::event_loop::EventLoop;
use alacritty_terminal::tty::{self, Shell};
use anyhow::{Context as _, Result};

use super::{EventProxy, GridSize, InputSink, SharedTerm};
use crate::model::LocalShell;

pub fn spawn(shell: &LocalShell, size: GridSize, term: SharedTerm, proxy: EventProxy) -> Result<InputSink> {
    let mut env = HashMap::new();
    env.insert("TERM".to_string(), "xterm-256color".to_string());
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    env.insert("TERM_PROGRAM".to_string(), "Komando".to_string());

    #[cfg(windows)]
    let program = quote_windows_program(&shell.program);
    #[cfg(not(windows))]
    let program = shell.program.clone();

    let options = tty::Options {
        shell: Some(Shell::new(program, shell.args.clone())),
        working_directory: dirs::home_dir(),
        drain_on_exit: true,
        env,
        #[cfg(windows)]
        escape_args: true,
    };
    let pty = tty::new(&options, size.window_size(), 0)
        .with_context(|| format!("Couldn't start {}", shell.program))?;
    let event_loop = EventLoop::new(term, proxy.clone(), pty, true, false)?;
    let sink = InputSink::Local(event_loop.channel());
    proxy.set_responder(sink.clone());
    event_loop.spawn();
    Ok(sink)
}

/// alacritty_terminal joins the program and its arguments into one command line for
/// CreateProcessW without quoting the program, so a path with spaces (`C:\Program Files\...`)
/// is split at the first space. Quote it unless it is quoted already or has no spaces.
#[cfg(windows)]
fn quote_windows_program(program: &str) -> String {
    let quoted = program.len() >= 2 && program.starts_with('"') && program.ends_with('"');
    if quoted || !program.contains(char::is_whitespace) {
        program.to_string()
    } else {
        format!("\"{program}\"")
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::quote_windows_program;

    #[test]
    fn quotes_program_paths_with_spaces() {
        assert_eq!(
            quote_windows_program(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            r#""C:\Program Files\PowerShell\7\pwsh.exe""#
        );
    }

    #[test]
    fn leaves_plain_and_already_quoted_programs_alone() {
        assert_eq!(quote_windows_program("powershell.exe"), "powershell.exe");
        assert_eq!(quote_windows_program(r"C:\Windows\System32\cmd.exe"), r"C:\Windows\System32\cmd.exe");
        assert_eq!(quote_windows_program(r#""C:\Program Files\pwsh.exe""#), r#""C:\Program Files\pwsh.exe""#);
    }
}
