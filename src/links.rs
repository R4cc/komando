//! Links in terminal output that open in the browser.
//!
//! Only text that says it is a link counts: a web address with its scheme (`http://`,
//! `https://`, `ftp://`). Bare host names and IP addresses in logs are left alone, since most
//! of them aren't meant to be opened. Programs can also mark text as a link themselves (OSC 8);
//! those open when their address has one of the same schemes.

use std::ops::Range;

/// The schemes a link may start with, lower case.
const SCHEMES: [&str; 3] = ["https://", "http://", "ftp://"];

/// Whether `url` is an address Komando opens: one of `SCHEMES` followed by something.
pub fn is_openable(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    SCHEMES.iter().any(|scheme| lower.starts_with(scheme) && lower.len() > scheme.len())
}

/// The link in `line` covering byte `at`, if any: its byte range in `line`.
pub fn link_at(line: &str, at: usize) -> Option<Range<usize>> {
    links(line).into_iter().find(|range| range.contains(&at))
}

/// Every link in `line`, as byte ranges, in order.
pub fn links(line: &str) -> Vec<Range<usize>> {
    let lower = line.to_ascii_lowercase();
    let mut found = vec![];
    let mut from = 0;
    while from < line.len() {
        let Some((start, scheme)) =
            SCHEMES.iter().filter_map(|s| lower[from..].find(s).map(|i| (from + i, *s))).min_by_key(|(i, _)| *i)
        else {
            break;
        };
        // "xhttp://" is not a link; "(http://", "'http://" and "=http://" are.
        let glued = line[..start].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_');
        let end = start + scheme.len() + url_len(&line[start + scheme.len()..]);
        if !glued && end > start + scheme.len() {
            found.push(start..end);
        }
        from = end.max(start + scheme.len());
    }
    found
}

/// How long the address after a scheme is: up to whitespace or a character that can't be in
/// one, without punctuation that ends the sentence around it, and without a closing bracket
/// it doesn't open.
fn url_len(rest: &str) -> usize {
    let mut end = rest
        .char_indices()
        .find(|(_, c)| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '`' | '{' | '}' | '|' | '\\' | '^'))
        .map_or(rest.len(), |(i, _)| i);
    loop {
        let Some(last) = rest[..end].chars().next_back() else { return 0 };
        let unopened = |open: char| rest[..end].matches(open).count() < rest[..end].matches(last).count();
        let trim = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' => true,
            ')' => unopened('('),
            ']' => unopened('['),
            _ => false,
        };
        if !trim {
            return end;
        }
        end -= last.len_utf8();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(line: &str) -> Vec<&str> {
        links(line).into_iter().map(|r| &line[r]).collect()
    }

    #[test]
    fn finds_addresses_with_a_scheme() {
        assert_eq!(all("see https://example.com/docs?a=1&b=2#top now"), ["https://example.com/docs?a=1&b=2#top"]);
        assert_eq!(all("HTTP://Example.com and ftp://files.lan/pub"), ["HTTP://Example.com", "ftp://files.lan/pub"]);
        assert_eq!(all("http://10.0.0.5:8080/status"), ["http://10.0.0.5:8080/status"]);
        assert_eq!(all("http://[fe80::1]:80/"), ["http://[fe80::1]:80/"]);
    }

    #[test]
    fn leaves_bare_addresses_and_ips_alone() {
        assert!(all("Accepted publickey for ops from 10.0.0.5 port 51234").is_empty());
        assert!(all("connecting to example.com:443").is_empty());
        assert!(all("www.example.com").is_empty());
        assert!(all("https://").is_empty());
        assert!(all("xhttps://example.com").is_empty());
    }

    #[test]
    fn leaves_out_punctuation_around_it() {
        assert_eq!(all("Open https://example.com."), ["https://example.com"]);
        assert_eq!(all("(see https://example.com/a)"), ["https://example.com/a"]);
        assert_eq!(all("https://en.wikipedia.org/wiki/Rust_(language)."), ["https://en.wikipedia.org/wiki/Rust_(language)"]);
        assert_eq!(all("\"https://example.com/x\","), ["https://example.com/x"]);
        assert_eq!(all("<https://example.com>"), ["https://example.com"]);
        assert_eq!(all("url='https://example.com/q'"), ["https://example.com/q"]);
    }

    #[test]
    fn finds_the_link_under_a_spot() {
        let line = "a https://one.lan b http://two.lan";
        assert_eq!(link_at(line, 4).map(|r| &line[r]), Some("https://one.lan"));
        assert_eq!(link_at(line, 18), None);
        assert_eq!(link_at(line, line.len() - 1).map(|r| &line[r]), Some("http://two.lan"));
    }

    #[test]
    fn only_web_schemes_open() {
        assert!(is_openable("https://example.com"));
        assert!(is_openable(" HTTP://x "));
        assert!(!is_openable("file:///etc/passwd"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("komando:command-mark"));
        assert!(!is_openable("http://"));
    }
}
