//! Native crashes: an access violation, a stack overflow and the like end the process
//! without a Rust panic, so the panic hook in `logging` never sees them and the log just
//! stops. This writes a minidump to the logs folder, and a line to the log saying what
//! crashed and where, before Windows Error Reporting takes over.
//!
//! The crashing thread may have overflowed its stack, leaving only the few kilobytes Rust
//! reserves for its stack-overflow message. So it does no work itself: it hands the exception
//! to a thread started for this at launch, and waits for that thread to write it all down.
//!
//! Fail-fast exits (`0xc0000409`, such as `std::process::abort`) and heap corruption skip
//! exception filters entirely. Only Windows' own record of the crash in Event Viewer covers
//! those.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use windows::Win32::Foundation::{HANDLE, HMODULE};
use windows::Win32::System::Diagnostics::Debug::{
    EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MiniDumpNormal, MiniDumpWithIndirectlyReferencedMemory,
    MiniDumpWithThreadInfo, MiniDumpWithUnloadedModules, MiniDumpWriteDump, SetUnhandledExceptionFilter,
};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW,
    GetModuleHandleExW,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, INFINITE, SetEvent, WaitForSingleObject,
};
use windows::core::PCWSTR;

/// Let Windows Error Reporting handle the crash next, as it would have without us.
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
/// How long the crashing thread waits for the dump; the process ends either way.
const REPORT_TIMEOUT_MS: u32 = 20_000;
/// Dumps kept in the logs folder; older ones are removed when a new one is written.
const KEEP_DUMPS: usize = 3;

struct Reporter {
    logs: PathBuf,
    /// Set by the crashing thread once the exception below is filled in.
    request: HANDLE,
    /// Set by the reporter thread once the dump and the log line are written.
    done: HANDLE,
}

// The event handles are only signalled and waited on, which any thread may do.
unsafe impl Send for Reporter {}
unsafe impl Sync for Reporter {}

static REPORTER: OnceLock<Reporter> = OnceLock::new();
static EXCEPTION: AtomicPtr<EXCEPTION_POINTERS> = AtomicPtr::new(std::ptr::null_mut());
static THREAD: AtomicU32 = AtomicU32::new(0);
static CRASHED: AtomicBool = AtomicBool::new(false);

/// Start the reporter thread and install the filter. Call once, after the logger is set up.
pub fn install(data_dir: &Path) {
    let logs = crate::logging::log_path(data_dir).parent().map_or_else(|| data_dir.to_path_buf(), Path::to_path_buf);
    // Auto-reset events, initially unset.
    let (Ok(request), Ok(done)) = (unsafe { CreateEventW(None, false, false, PCWSTR::null()) }, unsafe {
        CreateEventW(None, false, false, PCWSTR::null())
    }) else {
        log::warn!("Could not set up crash reports");
        return;
    };
    if REPORTER.set(Reporter { logs, request, done }).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new().name("crash reporter".into()).spawn(|| {
        let Some(reporter) = REPORTER.get() else { return };
        unsafe { WaitForSingleObject(reporter.request, INFINITE) };
        report(reporter);
        let _ = unsafe { SetEvent(reporter.done) };
    });
    if let Err(error) = spawned {
        log::warn!("Could not start the crash reporter: {error}");
        return;
    }
    unsafe { SetUnhandledExceptionFilter(Some(filter)) };
}

/// Runs on the crashing thread, perhaps with almost no stack left: hand over, wait, move on.
unsafe extern "system" fn filter(exception: *const EXCEPTION_POINTERS) -> i32 {
    if CRASHED.swap(true, Ordering::AcqRel) {
        return EXCEPTION_CONTINUE_SEARCH; // Another thread is already reporting.
    }
    let Some(reporter) = REPORTER.get() else { return EXCEPTION_CONTINUE_SEARCH };
    EXCEPTION.store(exception.cast_mut(), Ordering::Release);
    THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
    if unsafe { SetEvent(reporter.request) }.is_ok() {
        unsafe { WaitForSingleObject(reporter.done, REPORT_TIMEOUT_MS) };
    }
    EXCEPTION_CONTINUE_SEARCH
}

fn report(reporter: &Reporter) {
    let exception = EXCEPTION.load(Ordering::Acquire);
    let thread = THREAD.load(Ordering::Acquire);
    if exception.is_null() {
        return;
    }
    let (code, address) = unsafe {
        let record = &*(*exception).ExceptionRecord;
        (record.ExceptionCode.0 as u32, record.ExceptionAddress as usize)
    };
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let dump = reporter.logs.join(format!("komando-crash-{stamp}.dmp"));
    // The dump first: the log line takes the logger's lock, which the crashed thread may hold.
    let dumped = write_dump(&dump, exception, thread);
    let place = module_offset(address).unwrap_or_else(|| format!("{address:#x}"));
    let saved = match &dumped {
        Ok(()) => format!("minidump saved to {}", dump.display()),
        Err(error) => format!("no minidump ({error})"),
    };
    log::error!("Komando crashed: exception {code:#010x} ({}) at {place} on thread {thread}; {saved}", describe(code));
    log::logger().flush();
    if dumped.is_ok() {
        remove_old_dumps(&reporter.logs);
    }
}

fn write_dump(path: &Path, exception: *mut EXCEPTION_POINTERS, thread: u32) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle as _;
    let file = std::fs::File::create(path)?;
    let info = MINIDUMP_EXCEPTION_INFORMATION { ThreadId: thread, ExceptionPointers: exception, ClientPointers: false.into() };
    // Threads, their stacks and what those point to: enough for a backtrace and a look at
    // the locals, without the whole heap.
    let kind = MiniDumpNormal | MiniDumpWithThreadInfo | MiniDumpWithUnloadedModules | MiniDumpWithIndirectlyReferencedMemory;
    let written = unsafe {
        MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            HANDLE(file.as_raw_handle()),
            kind,
            Some(&info),
            None,
            None,
        )
    };
    written.map_err(|error| {
        let _ = std::fs::remove_file(path);
        std::io::Error::other(error.to_string())
    })
}

/// `komando.exe+0x1234`: the module the address is in, and where in it.
fn module_offset(address: usize) -> Option<String> {
    let mut module = HMODULE::default();
    let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
    unsafe { GetModuleHandleExW(flags, PCWSTR(address as *const u16), &mut module) }.ok()?;
    let mut name = [0u16; 260];
    let len = unsafe { GetModuleFileNameW(Some(module), &mut name) } as usize;
    let path = String::from_utf16_lossy(&name[..len]);
    let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
    Some(format!("{file}+{:#x}", address - module.0 as usize))
}

fn describe(code: u32) -> &'static str {
    match code {
        0xc000_0005 => "access violation",
        0xc000_00fd => "stack overflow",
        0xc000_0374 => "heap corruption",
        0xc000_0409 => "fail fast",
        0xc000_001d => "illegal instruction",
        0xc000_0094 => "integer division by zero",
        0x8000_0003 => "breakpoint",
        _ => "unknown",
    }
}

fn remove_old_dumps(logs: &Path) {
    let Ok(entries) = std::fs::read_dir(logs) else { return };
    let mut dumps: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("komando-crash-") && n.ends_with(".dmp")))
        .collect();
    // The timestamp in the name sorts oldest first.
    dumps.sort();
    let excess = dumps.len().saturating_sub(KEEP_DUMPS);
    for old in &dumps[..excess] {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_common_crash_codes() {
        assert_eq!(describe(0xc000_00fd), "stack overflow");
        assert_eq!(describe(0xc000_0005), "access violation");
        assert_eq!(describe(0x1234), "unknown");
    }

    #[test]
    fn finds_the_module_of_an_address_in_it() {
        let place = module_offset(names_the_common_crash_codes as fn() as usize).expect("own code is in a module");
        assert!(place.contains(".exe+0x"), "{place}");
    }

    #[test]
    fn keeps_only_the_newest_dumps() {
        let dir = std::env::temp_dir().join(format!("komando-crash-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for stamp in ["20260101-000000", "20260102-000000", "20260103-000000", "20260104-000000"] {
            std::fs::write(dir.join(format!("komando-crash-{stamp}.dmp")), b"").unwrap();
        }
        std::fs::write(dir.join("komando.log"), b"").unwrap();
        remove_old_dumps(&dir);
        let mut left: Vec<String> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["komando-crash-20260102-000000.dmp", "komando-crash-20260103-000000.dmp", "komando-crash-20260104-000000.dmp", "komando.log"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
