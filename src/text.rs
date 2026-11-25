//! Small wording and formatting helpers shared by the windows and by code that has none.

/// `n` and a count noun with a plain plural: "1 host", "3 hosts".
pub fn plural(n: usize, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

/// Unix seconds as a local date and time, "2026-11-25 14:03". `None` for a time past what a
/// date can hold.
pub fn local_date_time(unix: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(unix, 0).map(|at| at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
}
