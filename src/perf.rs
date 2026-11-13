//! Development profiling: counts and timings of hot paths, printed to stderr every 5 seconds.
//! Off unless `KOMANDO_PERF` is set; when off, each probe is a single relaxed atomic load.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static ENABLED: AtomicBool = AtomicBool::new(false);

fn stats() -> &'static Mutex<HashMap<&'static str, (u64, Duration)>> {
    static STATS: OnceLock<Mutex<HashMap<&'static str, (u64, Duration)>>> = OnceLock::new();
    STATS.get_or_init(Default::default)
}

/// Turn profiling on if `KOMANDO_PERF` is set, and start the reporter.
pub fn init() {
    if std::env::var_os("KOMANDO_PERF").is_none() {
        return;
    }
    ENABLED.store(true, Ordering::Relaxed);
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(5));
        let mut stats = stats().lock().unwrap();
        let mut rows: Vec<_> = stats.drain().collect();
        rows.sort_by(|a, b| b.1.1.cmp(&a.1.1));
        let mut out = String::from("perf (last 5 s):\n");
        for (name, (count, total)) in rows {
            let avg = if count > 0 { total / count as u32 } else { Duration::ZERO };
            out.push_str(&format!(
                "  {name:<28} {:>7.1}/s  total {:>8.1} ms/s  avg {avg:?}\n",
                count as f64 / 5.0,
                total.as_secs_f64() * 1000.0 / 5.0
            ));
        }
        eprint!("{out}");
    });
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Count one occurrence of `name`.
pub fn count(name: &'static str) {
    if enabled() {
        stats().lock().unwrap().entry(name).or_default().0 += 1;
    }
}

/// Time `f` under `name`.
pub fn time<R>(name: &'static str, f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let start = Instant::now();
    let result = f();
    let elapsed = start.elapsed();
    let mut stats = stats().lock().unwrap();
    let entry = stats.entry(name).or_default();
    entry.0 += 1;
    entry.1 += elapsed;
    result
}

/// Startup trace for development: with `KOMANDO_STARTUP_TRACE` set, each `startup` mark prints
/// how long after the process was created it was reached (so the time Windows spends loading
/// the executable is included), and how long since the previous mark.
pub fn startup(mark: &'static str) {
    static TRACE: OnceLock<Option<Mutex<f64>>> = OnceLock::new();
    let Some(previous) = TRACE.get_or_init(|| std::env::var_os("KOMANDO_STARTUP_TRACE").map(|_| Mutex::new(0.0))) else {
        return;
    };
    let now = since_process_start_ms();
    let mut previous = previous.lock().unwrap();
    eprintln!("startup {now:>8.1} ms  (+{:>6.1})  {mark}", now - *previous);
    log::info!("startup {now:.1} ms (+{:.1}) {mark}", now - *previous);
    *previous = now;
}

#[cfg(windows)]
fn since_process_start_ms() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64;
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    // SAFETY: GetCurrentProcess is a pseudo handle; the out-pointers are valid locals.
    unsafe {
        if GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user).is_err() {
            return 0.0;
        }
        (ticks(GetSystemTimePreciseAsFileTime()) - ticks(created)) / 10_000.0
    }
}

#[cfg(not(windows))]
fn since_process_start_ms() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}
