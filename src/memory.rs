//! Handing memory Komando no longer uses back to the operating system.
//!
//! A full Scrollback is hundreds of MB in rows of a few KB each. When a Session closes or its
//! rows are reallocated for a new width, glibc's allocator frees the rows but keeps most of
//! the pages for later allocations. After closing five Sessions that each held 100,000 lines
//! of logs, Komando still held about 280 MB more than before they opened; a Session with a full
//! Scrollback, made narrower and wider again four times, went from 320 MB to 925 MB.
//! [`release_soon`] asks the allocator to return its free pages, once, a moment after the last
//! request and off the UI thread: with it, those cases end at 72 MB and 325 MB. A trim takes a
//! few milliseconds.
//!
//! Elsewhere it does nothing. This was found and measured with glibc only; Windows' and
//! macOS's allocators manage their free pages differently.

/// Ask the allocator to return its free pages a moment from now. Requests that arrive while
/// one is waiting are answered by it, so a burst (several Sessions closing at once) is one
/// trim.
pub fn release_soon() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    glibc::release_soon();
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    /// Long enough for whatever freed the memory to finish (dropping a full Scrollback's rows
    /// takes a few milliseconds) and for a burst of requests to collect.
    const DELAY: Duration = Duration::from_secs(1);

    static PENDING: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" {
        /// glibc: return free memory at the top of each heap and every free page inside them.
        fn malloc_trim(pad: usize) -> i32;
    }

    pub fn release_soon() {
        if PENDING.swap(true, Ordering::AcqRel) {
            return;
        }
        let spawned = std::thread::Builder::new().name("release memory".into()).spawn(|| {
            std::thread::sleep(DELAY);
            // Cleared first: memory freed while trimming asks for a trim of its own.
            PENDING.store(false, Ordering::Release);
            let start = Instant::now();
            // SAFETY: malloc_trim only changes the allocator's own bookkeeping and is
            // thread-safe; it takes each arena's lock while it works on that arena.
            unsafe { malloc_trim(0) };
            log::debug!("Returned free memory to the system in {:?}", start.elapsed());
        });
        if spawned.is_err() {
            PENDING.store(false, Ordering::Release);
        }
    }
}
