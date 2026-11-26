//! One primary window per data folder and Windows login session. Relaunches restore it,
//! including from the tray, before allocating another GUI or loading the Vault.

use std::path::Path;
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, SetLastError, WPARAM,
};
use windows::Win32::System::Threading::{
    CreateMutexExW, OpenProcess, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::{
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
    WindowsAndMessaging::*,
};
use windows::core::{BOOL, PCWSTR};

const SUBCLASS_ID: usize = 0x4b4f49;
/// Followed by the process ID of the Komando being restarted.
const RESTART_FLAG: &str = "--restart-after";

/// Start a new Komando to replace this one; the caller then exits. The new one waits for
/// this process to end before it claims the startup gate. Otherwise it would find this
/// window still closing, hand over to it, and quit, leaving no Komando open.
pub fn relaunch() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe).arg(RESTART_FLAG).arg(std::process::id().to_string()).spawn()?;
    Ok(())
}

/// The Komando this launch replaces, when started by [`relaunch`].
fn restart_parent(args: impl IntoIterator<Item = String>) -> Option<u32> {
    args.into_iter().skip_while(|arg| arg != RESTART_FLAG).nth(1)?.parse().ok()
}

/// When this launch is a restart, wait for the old Komando to exit (for at most 10 s).
pub fn wait_for_restart(args: impl IntoIterator<Item = String>) {
    let Some(pid) = restart_parent(args) else { return };
    unsafe {
        // Already gone: nothing to wait for.
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else { return };
        let _ = WaitForSingleObject(process, 10_000);
        let _ = CloseHandle(process);
    }
}

fn identity(dir: &Path) -> String {
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let digest = Sha256::digest(absolute.to_string_lossy().to_lowercase().as_bytes());
    format!("Komando.MainWindow.{}", crate::text::hex(&digest))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn restore_message() -> u32 {
    static MESSAGE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(windows::core::w!("Komando.RestorePrimaryWindow.v1")) })
}

/// Retain this for the application's lifetime. An open named object is the startup gate;
/// it requires no owned mutex lock and is released by Windows if the process crashes.
pub struct Instance(HANDLE);

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl Instance {
    /// `None` means an existing primary was notified and this launch should exit.
    pub fn claim(dir: &Path) -> anyhow::Result<Option<Self>> {
        let key = identity(dir);
        let name = wide(&format!("Local\\{key}"));
        // Synchronize access suffices to keep the object alive. Requesting ALL_ACCESS would
        // prevent a normal shortcut from finding a primary started as administrator.
        let (handle, exists) = unsafe {
            SetLastError(ERROR_SUCCESS);
            let handle = CreateMutexExW(None, PCWSTR(name.as_ptr()), 0, SYNCHRONIZATION_SYNCHRONIZE.0)?;
            (handle, GetLastError() == ERROR_ALREADY_EXISTS)
        };
        let gate = Self(handle);
        if !exists {
            return Ok(Some(gate));
        }

        let key = wide(&key);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(hwnd) = find_window(&key) {
                unsafe {
                    let mut pid = 0;
                    GetWindowThreadProcessId(hwnd, Some(&mut pid));
                    // The shortcut owns foreground permission; grant it to this exact app.
                    let _ = AllowSetForegroundWindow(pid);
                    PostMessageW(Some(hwnd), restore_message(), WPARAM(0), LPARAM(0))?;
                }
                return Ok(None);
            }
            // Another launch can win the gate before its UI has finished starting.
            if Instant::now() >= deadline {
                anyhow::bail!("The running Komando window did not become ready. Try launching it again.");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn find_window(key: &[u16]) -> Option<HWND> {
    struct Search {
        key: *const u16,
        found: Option<HWND>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        let search = unsafe { &mut *(data.0 as *mut Search) };
        if !unsafe { GetPropW(hwnd, PCWSTR(search.key)) }.is_invalid() {
            search.found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut search = Search { key: key.as_ptr(), found: None };
    // EnumWindows also enumerates hidden top-level windows. Its intentional early stop
    // reports failure, so the callback's result is the source of truth.
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}

pub struct WindowBinding {
    hwnd: HWND,
    key: Vec<u16>,
}

impl WindowBinding {
    pub fn new(window: &gpui::Window, dir: &Path) -> anyhow::Result<Self> {
        let hwnd = crate::platform::window_handle(window).ok_or_else(|| anyhow::anyhow!("No native window handle"))?;
        Self::attach(HWND(hwnd as *mut _), dir)
    }

    fn attach(hwnd: HWND, dir: &Path) -> anyhow::Result<Self> {
        let key = wide(&identity(dir));
        let message = restore_message();
        anyhow::ensure!(message != 0, "Could not register the activation message");
        unsafe {
            // Permit only this argument-free restore request across UAC integrity levels.
            // No commands, credentials or shell requests pass through this message.
            ChangeWindowMessageFilterEx(hwnd, message, MSGFLT_ALLOW, None)?;
            anyhow::ensure!(
                SetWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID, 0).as_bool(),
                "Could not install the activation handler"
            );
            if let Err(error) = SetPropW(hwnd, PCWSTR(key.as_ptr()), Some(HANDLE(1usize as *mut _))) {
                let _ = RemoveWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID);
                return Err(error.into());
            }
        }
        Ok(Self { hwnd, key })
    }
}

impl Drop for WindowBinding {
    fn drop(&mut self) {
        unsafe {
            let _ = RemovePropW(self.hwnd, PCWSTR(self.key.as_ptr()));
            let _ = RemoveWindowSubclass(self.hwnd, Some(window_proc), SUBCLASS_ID);
        }
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if message == restore_message() {
        unsafe {
            // SW_SHOW preserves a visible maximized window. SW_RESTORE restores a hidden,
            // minimized one.
            let show = if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW };
            let _ = ShowWindow(hwnd, show);
            let _ = SetForegroundWindow(hwnd);
        }
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("komando-instance-{}", crate::model::new_id()))
    }

    fn test_window() -> HWND {
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                windows::core::w!("STATIC"),
                windows::core::w!("Komando activation test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                100,
                100,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        }
    }

    #[test]
    fn a_restart_names_the_komando_it_replaces() {
        let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        assert_eq!(restart_parent(args(&["--restart-after", "4242"])), Some(4242));
        assert_eq!(restart_parent(args(&["--shell", "cmd", "--restart-after", "7"])), Some(7));
        assert_eq!(restart_parent(args(&["--shell", "cmd"])), None);
        assert_eq!(restart_parent(args(&["--restart-after"])), None);
        assert_eq!(restart_parent(args(&["--restart-after", "x"])), None);
    }

    fn pump() {
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    #[test]
    fn a_second_process_restores_the_existing_hidden_window() {
        const CHILD: &str = "KOMANDO_INSTANCE_TEST_PROFILE";
        if let Some(dir) = std::env::var_os(CHILD) {
            assert!(Instance::claim(Path::new(&dir)).unwrap().is_none(), "a relaunch must not become a second primary");
            return;
        }
        let dir = profile();
        let primary = Instance::claim(&dir).unwrap().unwrap();
        let hwnd = test_window();
        let binding = WindowBinding::attach(hwnd, &dir).unwrap();
        unsafe {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
            let _ = ShowWindow(hwnd, SW_HIDE);
            assert!(!IsWindowVisible(hwnd).as_bool());
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["platform::instance::tests::a_second_process_restores_the_existing_hidden_window", "--exact", "--nocapture"])
            .env(CHILD, &dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            pump();
            if child.try_wait().unwrap().is_some() && unsafe { IsWindowVisible(hwnd).as_bool() } {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                drop(binding);
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                panic!(
                    "secondary launch did not restore the primary: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.wait_with_output().unwrap().status.success());
        unsafe {
            assert!(!IsIconic(hwnd).as_bool());
        }
        drop(binding);
        unsafe {
            DestroyWindow(hwnd).unwrap();
        }
        drop(primary);
        assert!(Instance::claim(&dir).unwrap().is_some(), "exit releases the startup gate");
    }

    #[test]
    fn launching_while_the_primary_starts_waits_for_its_window() {
        let dir = profile();
        let _primary = Instance::claim(&dir).unwrap().unwrap();
        let second_dir = dir.clone();
        let second = std::thread::spawn(move || Instance::claim(&second_dir).unwrap().is_none());
        std::thread::sleep(Duration::from_millis(100));
        let hwnd = test_window();
        let binding = WindowBinding::attach(hwnd, &dir).unwrap();
        assert!(second.join().unwrap());
        pump();
        unsafe {
            assert!(IsWindowVisible(hwnd).as_bool());
        }
        drop(binding);
        unsafe {
            DestroyWindow(hwnd).unwrap();
        }
    }

    #[test]
    fn separate_profiles_have_independent_primary_windows() {
        let first = profile();
        let second = profile();
        let _first = Instance::claim(&first).unwrap().unwrap();
        assert!(Instance::claim(&second).unwrap().is_some());
        assert_eq!(identity(&first), identity(&std::path::PathBuf::from(first.to_string_lossy().to_uppercase())));
    }
}
