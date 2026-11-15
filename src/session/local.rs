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
    // Closing a tab drops the Pty, which closes the pseudo-console; ConPTY then ends every
    // console process attached to it (checked with ping -t, cmd, background jobs, nested
    // PowerShell), like Windows Terminal. GUI apps started from the shell (notepad) survive.
    // No Job object is needed.
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
    fn windows_shells_deliver_output_and_exit_through_the_session_loop() {
        use crate::model::LocalShell;
        use crate::session::{EventProxy, GridSize, SessionEvent, Status, new_term};
        use std::time::{Duration, Instant};

        for (program, args) in [
            ("cmd.exe", vec!["/D", "/C", "echo KOMANDO_CONPTY_OK"]),
            ("powershell.exe", vec!["-NoLogo", "-NoProfile", "-Command", "Write-Output 'KOMANDO_CONPTY_OK'"]),
        ] {
            let shell = LocalShell { name: program.into(), program: program.into(), args: args.into_iter().map(str::to_owned).collect(), hidden: false };
            let size = GridSize { cols: 80, rows: 24, cell_width: 8, cell_height: 16 };
            let (tx, mut rx) = futures::channel::mpsc::unbounded();
            let proxy = EventProxy::new(tx);
            let term = new_term(size, 100, proxy.clone());
            let sink = super::spawn(&shell, size, term.clone(), proxy).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Ok(SessionEvent::Status(Status::Disconnected(_))) = rx.try_recv() {
                    break;
                }
                if Instant::now() >= deadline {
                    sink.close();
                    panic!("{program} did not report its exit");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output: String = term.lock().grid().display_iter().map(|cell| cell.c).collect();
            assert!(output.contains("KOMANDO_CONPTY_OK"), "{program} lost its output: {output:?}");
        }
    }

    #[test]
    fn conpty_releases_resources_after_failed_and_successful_launches() {
        const CHILD: &str = "KOMANDO_CONPTY_RESOURCE_TEST";
        // Isolate handle counts from other tests' files, sockets and threads.
        if std::env::var_os(CHILD).is_none() {
            use std::process::Stdio;
            use std::time::{Duration, Instant};
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["session::local::tests::conpty_releases_resources_after_failed_and_successful_launches", "--exact", "--nocapture"])
                .env(CHILD, "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("ConPTY cleanup did not finish in time");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        use alacritty_terminal::{event::WindowSize, tty::{self, Shell}};
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

        let handles = || {
            let mut count = 0;
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count).unwrap() };
            count
        };
        let options = |program: &str| tty::Options {
            shell: Some(Shell::new(program.into(), vec!["/D".into(), "/C".into(), "echo ConPTY".into()])),
            escape_args: true,
            ..Default::default()
        };
        let size = WindowSize { num_lines: 24, num_cols: 80, cell_width: 8, cell_height: 16 };
        let missing = options("komando-nonexistent-shell-for-test.exe");
        assert!(tty::new(&missing, WindowSize { num_cols: 0, ..size }, 0).is_err(), "invalid console sizes must return an error");
        assert!(tty::new(&missing, size, 0).is_err()); // Warm up DLL loading.
        let before = handles();
        for _ in 0..8 {
            assert!(tty::new(&missing, size, 0).is_err());
        }
        let after = handles();
        assert!(after <= before + 2, "failed launches leaked handles: {before} -> {after}");

        let cmd = options("cmd.exe");
        // Warm up the Windows wait pool and its cached worker resources as well.
        for _ in 0..8 {
            drop(tty::new(&cmd, size, 0).unwrap());
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let before = handles();
        for _ in 0..8 {
            drop(tty::new(&cmd, size, 0).unwrap());
        }
        // Pipe reader/writer threads finish asynchronously after the PTY closes.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let after = handles();
        assert!(after <= before + 2, "successful launches leaked handles: {before} -> {after}");
    }

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
