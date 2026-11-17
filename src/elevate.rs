//! Running a Local Shell as administrator.
//!
//! A running process can't gain administrator rights, so Komando starts a second copy of
//! itself through UAC (`ShellExecuteExW` with the `runas` verb). That copy opens the shell
//! in its own window, and every Session started there is elevated too. The window that asked
//! keeps running as it was; cancelling the prompt changes nothing. This is how Windows
//! Terminal does it: one window never mixes elevated and normal Sessions, so a program
//! without administrator rights can't type into an elevated shell through Komando's window.

/// Command-line flag naming the Local Shell to open at launch.
const SHELL_FLAG: &str = "--shell";
/// Command-line flag naming the Project that Local Shell belongs to.
const PROJECT_FLAG: &str = "--project";

/// Whether this Komando runs with administrator rights: started from the shell menu here, or
/// by the user with Explorer's "Run as administrator".
pub fn is_elevated() -> bool {
    static ELEVATED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ELEVATED.get_or_init(query_elevated)
}

#[cfg(windows)]
fn query_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut written = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut written,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
fn query_elevated() -> bool {
    false
}

/// How asking Windows to start the elevated copy went.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Started,
    /// The user said no in the UAC prompt.
    Cancelled,
}

/// Start an elevated Komando that opens `shell` (of `project`, or standalone). Blocks while
/// the UAC prompt is up, so call it off the UI thread. `parent` is the asking window's
/// handle: the prompt comes up in front of it instead of blinking in the taskbar.
#[cfg(windows)]
pub fn open_shell_elevated(parent: Option<isize>, project: Option<&str>, shell: &str) -> anyhow::Result<Outcome> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};
    use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{PCWSTR, w};

    let exe: Vec<u16> = std::env::current_exe()?.as_os_str().encode_wide().chain([0]).collect();
    let params: Vec<u16> = launch_command_line(project, shell).encode_utf16().chain([0]).collect();
    // ShellExecuteEx may use COM; this thread is ours alone, so it gets its own apartment.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // Return only once the new process exists, as this thread ends right after.
        fMask: SEE_MASK_NOASYNC,
        hwnd: HWND(parent.unwrap_or(0) as *mut core::ffi::c_void),
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    let result = unsafe { ShellExecuteExW(&mut info) };
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
    match result {
        Ok(()) => Ok(Outcome::Started),
        Err(err) if err.code() == ERROR_CANCELLED.to_hresult() => Ok(Outcome::Cancelled),
        Err(err) => Err(anyhow::anyhow!(err.message())),
    }
}

#[cfg(not(windows))]
pub fn open_shell_elevated(_parent: Option<isize>, _project: Option<&str>, _shell: &str) -> anyhow::Result<Outcome> {
    anyhow::bail!("running as administrator is only available on Windows")
}

/// The command line (after the program) that makes a new Komando open a Local Shell;
/// `shell_from_args` reads it back.
#[cfg(windows)]
fn launch_command_line(project: Option<&str>, shell: &str) -> String {
    let mut args = vec![SHELL_FLAG, shell];
    if let Some(project) = project {
        args.extend([PROJECT_FLAG, project]);
    }
    command_line(&args)
}

/// The Local Shell to open at launch, from the command line (program name already skipped):
/// its Project's id, if any, and its name.
pub fn shell_from_args(args: impl IntoIterator<Item = String>) -> Option<(Option<String>, String)> {
    let (mut shell, mut project) = (None, None);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            SHELL_FLAG => shell = args.next(),
            PROJECT_FLAG => project = args.next(),
            _ => {}
        }
    }
    shell.map(|shell| (project, shell))
}

/// Join arguments into a Windows command line that splits back into the same arguments
/// (the rules of `CommandLineToArgvW`, which Rust's `std::env::args` follows too). Shell and
/// Project names may hold spaces, quotes and backslashes.
#[cfg(windows)]
fn command_line(args: &[&str]) -> String {
    args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ")
}

#[cfg(windows)]
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Each backslash before a quote is doubled, and the quote itself escaped.
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote are doubled so it stays a closing quote.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

#[cfg(all(test, windows))]
mod tests {
    use super::{launch_command_line, shell_from_args};

    /// What the elevated Komando reads back from the command line the asking one built,
    /// split by Windows' own rules.
    fn round_trip(project: Option<&str>, shell: &str) -> Option<(Option<String>, String)> {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::UI::Shell::CommandLineToArgvW;
        use windows::core::PCWSTR;

        // The program comes first on a real command line, and follows other rules.
        let line: Vec<u16> =
            format!("komando.exe {}", launch_command_line(project, shell)).encode_utf16().chain([0]).collect();
        let mut count = 0;
        let args = unsafe {
            let argv = CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut count);
            assert!(!argv.is_null());
            let args: Vec<String> = (1..count as usize).map(|i| (*argv.add(i)).to_string().unwrap()).collect();
            let _ = LocalFree(Some(HLOCAL(argv as *mut core::ffi::c_void)));
            args
        };
        shell_from_args(args)
    }

    #[test]
    fn a_standalone_shell_reaches_the_elevated_komando() {
        assert_eq!(round_trip(None, "PowerShell"), Some((None, "PowerShell".into())));
    }

    #[test]
    fn a_project_shell_reaches_the_elevated_komando_with_its_project() {
        assert_eq!(
            round_trip(Some("6f1c2a9e-0d4b-4c39-9a51-2b7e8d3f4a10"), "cmd"),
            Some((Some("6f1c2a9e-0d4b-4c39-9a51-2b7e8d3f4a10".into()), "cmd".into()))
        );
    }

    #[test]
    fn shell_names_with_spaces_quotes_and_backslashes_arrive_unchanged() {
        for name in ["Git Bash", r#"say "hi""#, r"tools\", r"C:\Program Files\x\", r#"a\\"b"#, "Ünïcode shell", "--project"] {
            assert_eq!(round_trip(Some("p 1"), name), Some((Some("p 1".into()), name.into())), "{name}");
        }
    }

    #[test]
    fn a_normal_launch_opens_no_shell() {
        assert_eq!(shell_from_args(Vec::<String>::new()), None);
    }
}
