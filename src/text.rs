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

/// Whether `text` matches `pattern`, where `*` is any run of characters and `?` any one.
/// Case-sensitive: callers that ignore case lowercase both sides first.
pub fn wildcard(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        // A `*` is always a wildcard, even where the text has a `*` too.
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// `bytes` in lowercase hex, two digits each.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::wildcard;

    #[test]
    fn wildcards_match_as_globs_do() {
        assert!(wildcard("*.example.com", "web.example.com"));
        assert!(wildcard("web-??", "web-01"));
        assert!(!wildcard("web-??", "web-1"));
        assert!(wildcard("a*b*c", "aXbYc"));
        assert!(!wildcard("a*b", "ac"));
        assert!(wildcard("*", ""));
        assert!(!wildcard("", "x"));
    }

    #[test]
    fn a_star_in_the_text_does_not_stop_a_star_in_the_pattern() {
        // A `*` in the pattern used to be taken literally where the text had one too.
        assert!(wildcard("*", "*x"));
        assert!(wildcard("a*c", "a*xc"));
        assert!(wildcard("*.conf", "*.d.conf"));
        assert!(!wildcard("*.conf", "*.d.cnf"));
    }
}
