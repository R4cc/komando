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

    let options = tty::Options {
        shell: Some(Shell::new(shell.program.clone(), shell.args.clone())),
        working_directory: dirs::home_dir(),
        drain_on_exit: true,
        env,
        #[cfg(windows)]
        escape_args: true,
    };
    let pty = tty::new(&options, size.window_size(), 0)
        .with_context(|| format!("could not start {}", shell.program))?;
    let event_loop = EventLoop::new(term, proxy.clone(), pty, true, false)?;
    let sink = InputSink::Local(event_loop.channel());
    proxy.set_responder(sink.clone());
    event_loop.spawn();
    Ok(sink)
}
