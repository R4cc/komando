//! Highlighting: colour text in terminal output that matches keyword rules.
//!
//! The file format is Tabby's highlight-set format, so sets can be moved between Tabby and
//! Komando unchanged: `{ "id", "name", "keywords": [ { "text", "isRegExp", ... } ] }`.
//! Komando stores all sets as a JSON array in `highlights.json`; export writes one set
//! object; import accepts a set object or an array of sets.
//!
//! Matching rules:
//! - Rules apply in order and the first rule to colour a character wins, so put specific
//!   rules ("WARNING", "Active: inactive") before general ones ("WARN", "Active").
//! - Plain text matches anywhere in the line; `isRegExp` uses Rust regex syntax, which
//!   covers the common JavaScript syntax but not look-around or back-references. Rules that
//!   don't compile are skipped and reported.
//! - With `isMatchGroup`, only the capture group named in `matchGroup` (number or name) is
//!   coloured.
//! - Matching is per screen row.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use regex::{Regex, RegexBuilder, RegexSet, RegexSetBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::{Unreadable, write_atomic};

pub const FILE: &str = "highlights.json";
const DEFAULT_SET: &str = include_str!("../assets/highlights/default.json");
/// Kept only to recognise saved, unedited copies when upgrading the bundled rules.
const LEGACY_DEFAULT_SET: &str = include_str!("../assets/highlights/legacy-default.json");
const MUTED_DEFAULT_SET: &str = include_str!("../assets/highlights/muted-default.json");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HighlightSet {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub keywords: Vec<Keyword>,
    /// Fields Komando doesn't use are kept so export gives back what was imported.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Keyword {
    pub text: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_reg_exp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_case_sensitive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_match_group: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_group: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn yes() -> bool {
    true
}

impl Keyword {
    pub fn is_regex(&self) -> bool {
        self.is_reg_exp.unwrap_or(false)
    }

    pub fn case_sensitive(&self) -> bool {
        self.is_case_sensitive.unwrap_or(false)
    }

    /// The foreground colour this rule sets, if any. A colour without an explicit
    /// `foreground` flag counts as on.
    pub fn fg(&self) -> Option<Rgb8> {
        if self.foreground.unwrap_or(self.foreground_color.is_some()) {
            self.foreground_color.as_deref().and_then(parse_hex)
        } else {
            None
        }
    }

    pub fn bg(&self) -> Option<Rgb8> {
        if self.background.unwrap_or(false) {
            self.background_color.as_deref().and_then(parse_hex)
        } else {
            None
        }
    }
}

/// The colours of the bundled set, by what they mean there, offered first in the rule editor.
pub const SWATCHES: [(&str, &str); 6] = [
    ("#FF4D4F", "Failure"),
    ("#FFAA00", "Warning"),
    ("#87C999", "Success"),
    ("#87BFEA", "Information"),
    ("#B6A0D9", "Component"),
    ("#A6ADB9", "Quiet"),
];

/// A Highlight Rule as the rule editor in Settings → Highlights holds it: what the form shows,
/// before it is checked. `build` turns it into a rule or says what's wrong with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleDraft {
    pub text: String,
    pub regex: bool,
    pub case_sensitive: bool,
    /// Regex only: colour just this capture group, by number or name. Empty colours the whole match.
    pub group: String,
    /// `#rgb` or `#rrggbb`; `None` (or empty) leaves the text colour alone.
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub bold: bool,
}

impl RuleDraft {
    pub fn from_keyword(kw: &Keyword) -> Self {
        let group = match (kw.is_regex() && kw.is_match_group.unwrap_or(false), kw.match_group.as_deref()) {
            (true, Some(g)) => g.trim().to_string(),
            _ => String::new(),
        };
        Self {
            text: kw.text.clone(),
            regex: kw.is_regex(),
            case_sensitive: kw.case_sensitive(),
            group,
            foreground: kw.fg().map(long_hex),
            background: kw.bg().map(long_hex),
            bold: kw.bold.unwrap_or(false),
        }
    }

    /// The rule, or why it can't be one, in words for the form. `base` is the rule being
    /// edited: whether it is on, and the fields Komando doesn't show, carry over, and fields
    /// the edit didn't change are left exactly as they were.
    pub fn build(&self, base: Option<&Keyword>) -> std::result::Result<Keyword, String> {
        if self.text.trim().is_empty() {
            return Err("Enter the text or regular expression to match.".into());
        }
        let group = self.group.trim();
        let use_group = self.regex && !group.is_empty();
        if self.regex {
            let regex = RegexBuilder::new(&self.text)
                .case_insensitive(!self.case_sensitive)
                .build()
                .map_err(|err| format!("This isn't a regular expression Komando can use: {}", first_line(&err.to_string())))?;
            let exists = match group.parse::<usize>() {
                _ if !use_group => true,
                Ok(index) => index < regex.captures_len(),
                Err(_) => regex.capture_names().flatten().any(|name| name == group),
            };
            if !exists {
                return Err(format!("The pattern has no group {group}. Number groups from 1, or name one with (?P<name>…)."));
            }
        }
        let colour = |value: Option<&str>, what: &str| match value.map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(v) => parse_hex(v).map(Some).ok_or_else(|| format!("{what} must be a hex colour such as #FF4D4F.")),
        };
        let fg = colour(self.foreground.as_deref(), "Text colour")?;
        let bg = colour(self.background.as_deref(), "Background")?;
        if fg.is_none() && bg.is_none() && !self.bold {
            return Err("Choose a text colour, a background or bold: without one the rule colours nothing.".into());
        }

        let mut kw = base.cloned().unwrap_or_else(|| Keyword {
            text: String::new(),
            enabled: true,
            is_reg_exp: Some(false),
            is_case_sensitive: Some(false),
            foreground: None,
            foreground_color: None,
            background: None,
            background_color: None,
            bold: None,
            is_match_group: None,
            match_group: None,
            extra: Map::new(),
        });
        // An unchanged flag keeps how it was written, including not at all.
        let keep = |old: Option<bool>, new: bool| if old.unwrap_or(false) == new { old } else { Some(new) };
        kw.text = self.text.clone();
        kw.is_reg_exp = keep(kw.is_reg_exp, self.regex);
        kw.is_case_sensitive = keep(kw.is_case_sensitive, self.case_sensitive);
        kw.bold = keep(kw.bold, self.bold);
        kw.is_match_group = keep(kw.is_match_group, use_group);
        if use_group && kw.match_group.as_deref().map(str::trim) != Some(group) {
            kw.match_group = Some(group.to_string());
        }
        if kw.fg() != fg {
            kw.foreground = Some(fg.is_some());
            if let Some(c) = fg {
                kw.foreground_color = Some(long_hex(c));
            }
        }
        if kw.bg() != bg {
            kw.background = Some(bg.is_some());
            if let Some(c) = bg {
                kw.background_color = Some(long_hex(c));
            }
        }
        // A JavaScript rule rewritten here is a plain or regex rule now.
        if kw.extra.get("isJS").and_then(Value::as_bool) == Some(true) {
            kw.extra.insert("isJS".into(), Value::Bool(false));
        }
        Ok(kw)
    }
}

/// An empty Highlight Set with a new id.
pub fn new_set(name: &str) -> HighlightSet {
    HighlightSet { id: uuid::Uuid::new_v4().to_string(), name: name.to_string(), keywords: vec![], extra: Map::new() }
}

/// `#RRGGBB`.
pub fn long_hex(c: Rgb8) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// `#rgb` or `#rrggbb`. Anything else (Tabby sometimes stores "0") means no colour.
pub fn parse_hex(value: &str) -> Option<Rgb8> {
    let hex = value.trim().strip_prefix('#')?;
    let expand = |c: u8| c * 17;
    match hex.len() {
        3 => {
            let v = u16::from_str_radix(hex, 16).ok()?;
            Some(Rgb8 { r: expand(((v >> 8) & 0xf) as u8), g: expand(((v >> 4) & 0xf) as u8), b: expand((v & 0xf) as u8) })
        }
        6 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            Some(Rgb8 { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 })
        }
        _ => None,
    }
}

/// What a match does to the characters it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Rgb8>,
    pub bg: Option<Rgb8>,
    pub bold: bool,
}

enum Group {
    Whole,
    Index(usize),
    Name(String),
}

struct Rule {
    regex: Regex,
    group: Group,
    style: Style,
    display: [Style; 2],
    /// The keyword it was compiled from, when it came from a set.
    source: Option<RuleSource>,
}

/// Where a rule came from: keyword `index` of the set `set`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleSource {
    pub set: String,
    pub index: usize,
}

/// The rule that colours a spot of a line, and the text it colours there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleHit {
    pub source: RuleSource,
    pub matched: String,
}

/// The highlighter every terminal uses; replaced when rules change.
#[derive(Default)]
pub struct ActiveHighlights(pub std::sync::Arc<Highlighter>);

impl gpui::Global for ActiveHighlights {}

/// Compiled form of all enabled rules, shared by every terminal.
#[derive(Default)]
pub struct Highlighter {
    set: Option<RegexSet>,
    rules: Vec<Rule>,
    /// Rules that could not be used, with the reason (shown in the Highlights panel).
    pub skipped: Vec<(String, String)>,
    /// Also colour full-screen programs (the alternate screen). Off, vim and htop are left alone.
    pub full_screen: bool,
}

impl Highlighter {
    pub fn new<'a>(keywords: impl IntoIterator<Item = &'a Keyword>) -> Self {
        Self::compile(keywords.into_iter().map(|kw| (None, kw)), true)
    }

    /// `ascii_boundaries`: compile word boundaries as ASCII ones (see `ascii_word_boundaries`);
    /// off only in tests, to compare with the rules as written.
    fn compile<'a>(keywords: impl IntoIterator<Item = (Option<RuleSource>, &'a Keyword)>, ascii_boundaries: bool) -> Self {
        let mut rules = Vec::new();
        let mut patterns = Vec::new();
        let mut skipped = Vec::new();
        for (source, kw) in keywords {
            if !kw.enabled || kw.text.is_empty() {
                continue;
            }
            if kw.extra.get("isJS").and_then(Value::as_bool) == Some(true) {
                skipped.push((kw.text.clone(), "JavaScript rules are not supported".into()));
                continue;
            }
            let style = Style { fg: kw.fg(), bg: kw.bg(), bold: kw.bold.unwrap_or(false) };
            if style.fg.is_none() && style.bg.is_none() && !style.bold {
                continue;
            }
            let pattern = match (kw.is_regex(), ascii_boundaries) {
                (true, true) => ascii_word_boundaries(&kw.text),
                (true, false) => kw.text.clone(),
                (false, _) => regex::escape(&kw.text),
            };
            let regex = match RegexBuilder::new(&pattern).case_insensitive(!kw.case_sensitive()).build() {
                Ok(regex) => regex,
                Err(err) => {
                    skipped.push((kw.text.clone(), first_line(&err.to_string())));
                    continue;
                }
            };
            let group = match (kw.is_match_group.unwrap_or(false), kw.match_group.as_deref().map(str::trim)) {
                (true, Some(g)) if !g.is_empty() => match g.parse::<usize>() {
                    Ok(i) => Group::Index(i),
                    Err(_) => Group::Name(g.to_string()),
                },
                _ => Group::Whole,
            };
            let flags = if kw.case_sensitive() { "" } else { "(?i)" };
            patterns.push(format!("{flags}{pattern}"));
            let display = [false, true].map(|dark| Style {
                fg: style.fg.map(|c| if style.bg.is_some() { c } else { readable_on(c, dark) }),
                ..style
            });
            rules.push(Rule { regex, group, style, display, source });
        }
        // Each pattern compiled on its own above; the combined set can still fail (size limit).
        // Without it nothing is highlighted, so say why instead of failing silently.
        let set = match (!patterns.is_empty()).then(|| RegexSetBuilder::new(&patterns).build()) {
            Some(Ok(set)) => Some(set),
            Some(Err(err)) => {
                log::error!("highlight rules: {err}");
                skipped.push(("All rules".into(), format!("Highlighting is off: {}", first_line(&err.to_string()))));
                None
            }
            None => None,
        };
        Self { set, rules, skipped, full_screen: false }
    }

    pub fn from_sets(sets: &[HighlightSet], disabled_sets: &[String]) -> Self {
        let keywords = sets.iter().filter(|s| !disabled_sets.contains(&s.id)).flat_map(|s| {
            s.keywords.iter().enumerate().map(|(index, kw)| (Some(RuleSource { set: s.id.clone(), index }), kw))
        });
        Self::compile(keywords, true)
    }

    /// The rule whose colour shows at byte `at` of `line` (the first one that covers it, as
    /// when painting), with the text it matched there. `None` when nothing colours it, or the
    /// rule came from no set.
    pub fn rule_at(&self, line: &str, at: usize) -> Option<RuleHit> {
        let set = self.set.as_ref()?;
        for index in set.matches(line).iter() {
            let rule = &self.rules[index];
            let mut hit = None;
            rule.spans(line, |start, end| {
                if hit.is_none() && (start..end).contains(&at) {
                    hit = Some(line[start..end].to_string());
                }
            });
            if let Some(matched) = hit {
                return rule.source.clone().map(|source| RuleHit { source, matched });
            }
        }
        None
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// One entry per byte of `line`: the style of the first rule that covers it.
    pub fn styles(&self, line: &str) -> Option<Vec<Option<Style>>> {
        let mut out = Vec::new();
        self.match_styles(line, |rule| rule.style, &mut out).then_some(out)
    }

    /// Colours adjusted once when rules compile, rather than for every matched character
    /// on every repaint. Rules with their own background retain their original colours.
    pub fn styles_for_background(&self, line: &str, dark: bool) -> Option<Vec<Option<Style>>> {
        let mut out = Vec::new();
        self.styles_for_background_into(line, dark, &mut out).then_some(out)
    }

    /// `styles_for_background` into `out`, a buffer reused from line to line. False when no
    /// rule matches, `out` then left as it was.
    pub fn styles_for_background_into(&self, line: &str, dark: bool, out: &mut Vec<Option<Style>>) -> bool {
        self.match_styles(line, |rule| rule.display[usize::from(dark)], out)
    }

    fn match_styles(&self, line: &str, style: impl Fn(&Rule) -> Style, out: &mut Vec<Option<Style>>) -> bool {
        let Some(set) = self.set.as_ref() else { return false };
        let matched = set.matches(line);
        if !matched.matched_any() {
            return false;
        }
        out.clear();
        out.resize(line.len(), None);
        for index in matched.iter() {
            let rule = &self.rules[index];
            rule.spans(line, |start, end| {
                for slot in &mut out[start..end] {
                    if slot.is_none() {
                        *slot = Some(style(rule));
                    }
                }
            });
        }
        true
    }
}

impl Rule {
    /// Each stretch of `line` this rule colours, as byte ranges.
    fn spans(&self, line: &str, mut each: impl FnMut(usize, usize)) {
        match &self.group {
            Group::Whole => {
                for m in self.regex.find_iter(line) {
                    each(m.start(), m.end());
                }
            }
            Group::Index(i) => {
                for caps in self.regex.captures_iter(line) {
                    if let Some(m) = caps.get(*i) {
                        each(m.start(), m.end());
                    }
                }
            }
            Group::Name(name) => {
                for caps in self.regex.captures_iter(line) {
                    if let Some(m) = caps.name(name) {
                        each(m.start(), m.end());
                    }
                }
            }
        }
    }
}

/// `pattern` with each word boundary (`\b`, `\B`, and their `{...}` forms) made an ASCII one,
/// `(?-u:\b)`, as in JavaScript, where Tabby's rules come from. Everything else stays Unicode.
///
/// A Unicode word boundary keeps the regex engine from using its fast DFA on text with any
/// non-ASCII character: it falls back to a far slower engine for the whole rule set, which
/// made highlighting rows with accented, Cyrillic or CJK text several milliseconds per frame.
/// On ASCII text the two kinds of boundary match the same places.
fn ascii_word_boundaries(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut chars = pattern.chars().peekable();
    // Nesting depth of character classes (`[a[b]]`, `[[:alpha:]]`); there `\b` is no boundary.
    let mut class = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(b @ ('b' | 'B')) if class == 0 => {
                    out.push_str("(?-u:\\");
                    out.push(b);
                    if chars.peek() == Some(&'{') {
                        for c in chars.by_ref() {
                            out.push(c);
                            if c == '}' {
                                break;
                            }
                        }
                    }
                    out.push(')');
                }
                Some(next) => {
                    out.push('\\');
                    out.push(next);
                }
                None => out.push('\\'),
            },
            '[' => {
                out.push('[');
                // POSIX classes (`[:alpha:]`) are not nested classes.
                if class > 0 && chars.peek() == Some(&':') {
                    for c in chars.by_ref() {
                        out.push(c);
                        if c == ']' {
                            break;
                        }
                    }
                    continue;
                }
                class += 1;
                // A `]` straight after `[` or `[^` is a literal one.
                if chars.peek() == Some(&'^') {
                    out.push(chars.next().unwrap());
                }
                if chars.peek() == Some(&']') {
                    out.push(chars.next().unwrap());
                }
            }
            ']' if class > 0 => {
                class -= 1;
                out.push(']');
            }
            _ => out.push(c),
        }
    }
    out
}

fn first_line(s: &str) -> String {
    s.lines().last().unwrap_or(s).trim().to_string()
}

/// Parse an imported file: one set object, or an array of sets.
pub fn parse(text: &str) -> Result<Vec<HighlightSet>> {
    let sets = parse_sets(text)?;
    if sets.is_empty() {
        bail!("the file contains no highlight sets");
    }
    Ok(sets)
}

/// One set object, or an array of sets, which may be empty: that's the saved file once the
/// user deletes every set.
fn parse_sets(text: &str) -> Result<Vec<HighlightSet>> {
    let value: Value = serde_json::from_str(text).context("not valid JSON")?;
    Ok(match value {
        Value::Array(items) => items
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<HighlightSet>, _>>()
            .context("not a list of highlight sets")?,
        Value::Object(_) => vec![serde_json::from_value(value).context("not a highlight set")?],
        _ => bail!("expected a highlight set"),
    })
}

/// The saved highlight sets, or the bundled set on first run or when the file can't be read.
pub fn load(dir: &Path) -> (Vec<HighlightSet>, Option<Unreadable>) {
    let path = dir.join(FILE);
    let err = match fs::read_to_string(&path) {
        Ok(text) => match parse_sets(&text) {
            Ok(mut sets) => {
                // Compare the whole set, not just its id: toggles, renames, imported fields
                // and edited rules all belong to the user and must survive an update.
                if let (Ok(legacy), Ok(muted), Ok(bundled)) =
                    (parse(LEGACY_DEFAULT_SET), parse(MUTED_DEFAULT_SET), parse(DEFAULT_SET))
                {
                    for set in &mut sets {
                        if legacy.first() == Some(set) || muted.first() == Some(set) {
                            *set = bundled[0].clone();
                        }
                    }
                }
                return (sets, None);
            }
            Err(err) => err,
        },
        // First run: start with the bundled set.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return (parse(DEFAULT_SET).unwrap_or_default(), None),
        Err(err) => anyhow::Error::from(err),
    };
    // Kept aside: the next save would otherwise replace the user's rules with the bundled set.
    (parse(DEFAULT_SET).unwrap_or_default(), Some(Unreadable::keep(&path, &err, "your highlights", "highlights")))
}

pub fn save(dir: &Path, sets: &[HighlightSet]) -> Result<()> {
    crate::model::create_private_dir(dir)?;
    write_atomic(&dir.join(FILE), serde_json::to_string_pretty(sets)?.as_bytes())
}

/// Export one set in the exchange format (a single set object).
pub fn export(set: &HighlightSet) -> Result<String> {
    Ok(serde_json::to_string_pretty(set)?)
}

/// Add imported sets; a set with the same id as an existing one replaces it.
/// Returns (added, replaced).
pub fn merge(existing: &mut Vec<HighlightSet>, imported: Vec<HighlightSet>) -> (usize, usize) {
    let (mut added, mut replaced) = (0, 0);
    for set in imported {
        if let Some(slot) = existing.iter_mut().find(|s| s.id == set.id) {
            *slot = set;
            replaced += 1;
        } else {
            existing.push(set);
            added += 1;
        }
    }
    (added, replaced)
}

/// Rule colours are picked for some background, not necessarily ours. Each is stepped until its
/// luminance passes a fixed threshold: on a light terminal very light colours (#00FF00,
/// #FFD700) are darkened, on a dark one very dark colours (#0000FF) are lightened. Colours that
/// already pass are left as they are. The thresholds were chosen for the bundled backgrounds;
/// contrast against the actual background isn't computed here.
pub fn readable_on(color: Rgb8, dark_background: bool) -> Rgb8 {
    let luminance = |c: Rgb8| {
        let lin = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    };
    let mut c = color;
    for _ in 0..16 {
        if dark_background {
            // On the bundled backgrounds this gives 4.5:1 on dark Themes and Default. Brighter
            // Terminal Colors (Nord, One Half Dark, Dracula) get at least 3.75:1: more lift
            // would wash out red and amber. default_colours_read_on_every_terminal_theme
            // checks the bundled rules against them.
            if luminance(c) >= 0.27 {
                break;
            }
            let lift = |v: u8| (v as f32 + (255.0 - v as f32) * 0.15).round() as u8;
            c = Rgb8 { r: lift(c.r), g: lift(c.g), b: lift(c.b) };
        } else {
            // At least 5:1 on the bundled light backgrounds, with a margin for warm paper and
            // pale blue ones, not only white.
            if luminance(c) <= 0.12 {
                break;
            }
            c = Rgb8 { r: (c.r as f32 * 0.85) as u8, g: (c.g as f32 * 0.85) as u8, b: (c.b as f32 * 0.85) as u8 };
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(text: &str, color: &str) -> Keyword {
        serde_json::from_value(serde_json::json!({
            "text": text, "enabled": true, "foreground": true, "foregroundColor": color, "isCaseSensitive": true
        }))
        .unwrap()
    }

    fn color_at(h: &Highlighter, line: &str, needle: &str) -> Option<Rgb8> {
        let styles = h.styles(line)?;
        styles[line.find(needle)?].and_then(|s| s.fg)
    }

    #[test]
    fn bundled_set_parses_and_compiles() {
        let sets = parse(DEFAULT_SET).unwrap();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].name, "Linux & RHEL");
        let h = Highlighter::from_sets(&sets, &[]);
        assert!(h.skipped.is_empty(), "{:?}", h.skipped);
        assert_eq!(h.rule_count(), sets[0].keywords.len());
        assert!(sets[0].keywords.iter().all(|rule| rule.bg().is_none()));
    }

    #[test]
    fn word_boundaries_become_ascii() {
        let cases = [
            (r"\bhealthy\b", r"(?-u:\b)healthy(?-u:\b)"),
            (r"\Bx\b{start}y\b{end-half}", r"(?-u:\B)x(?-u:\b{start})y(?-u:\b{end-half})"),
            (r"a\\b", r"a\\b"), // An escaped backslash, then `b`.
            (r"[\b]\b", r"[\b](?-u:\b)"), // Inside a class it isn't a boundary.
            (r"[]\b]\b", r"[]\b](?-u:\b)"), // `]` first in a class is literal.
            (r"[^]a]\b", r"[^]a](?-u:\b)"),
            (r"[[:alpha:]\b]\b", r"[[:alpha:]\b](?-u:\b)"),
            (r"[a[b\b]]\b", r"[a[b\b]](?-u:\b)"),
            (r"\d+\s\w\p{L}", r"\d+\s\w\p{L}"),
            (r"trailing\", r"trailing\"),
        ];
        for (from, to) in cases {
            assert_eq!(ascii_word_boundaries(from), to, "{from}");
        }
    }

    /// The rewritten bundled rules compile, use the fast engine, and colour ASCII text exactly as
    /// the rules as written do.
    #[test]
    fn ascii_word_boundaries_match_the_same_on_ascii_text() {
        let sets = parse(DEFAULT_SET).unwrap();
        let keywords: Vec<&Keyword> = sets.iter().flat_map(|s| s.keywords.iter()).collect();
        let rewritten = Highlighter::new(keywords.iter().copied());
        let unicode = Highlighter::compile(keywords.iter().map(|kw| (None, *kw)), false);
        assert!(rewritten.skipped.is_empty(), "{:?}", rewritten.skipped);
        assert_eq!(rewritten.rule_count(), unicode.rule_count());
        let lines = [
            "2026-11-14 container web is healthy, db unhealthy; WARNING: timeout",
            "healthyish unhealthy_x xhealthy ERROR: connection refused (exited 1)",
            "Active: active (running) since Fri; sshd[42]: Started OpenSSH; kernel: OOM",
            "a-healthy-b healthy.healthy (healthy) [unhealthy] health: starting",
        ];
        for line in lines {
            assert_eq!(rewritten.styles(line), unicode.styles(line), "{line}");
        }
    }

    /// Highlighting a screen of non-ASCII log rows with Unicode and with ASCII word boundaries.
    /// `KOMANDO_HIGHLIGHT_RULES` names another rules file to use instead of the bundled one.
    #[test]
    #[ignore]
    fn bench_word_boundaries_on_non_ascii_rows() {
        let text = std::env::var("KOMANDO_HIGHLIGHT_RULES")
            .map(|path| fs::read_to_string(path).unwrap())
            .unwrap_or_else(|_| DEFAULT_SET.to_string());
        let sets = parse(&text).unwrap();
        let keywords: Vec<&Keyword> = sets.iter().flat_map(|s| s.keywords.iter()).collect();
        let rows: Vec<String> = (0..42)
            .map(|i| {
                format!(
                    "2026-11-14T09:00:{i:02}Z INFO  Größe prüfen: Bestätigung für Einträge {i} ЗАПРОС обработан 获取 已完成 user=müller{i} took {i}ms"
                )
            })
            .collect();
        for (name, ascii) in [("unicode \\b", false), ("ascii \\b", true)] {
            let h = Highlighter::compile(keywords.iter().map(|kw| (None, *kw)), ascii);
            let start = std::time::Instant::now();
            let frames = 50;
            for _ in 0..frames {
                for row in &rows {
                    std::hint::black_box(h.styles(row));
                }
            }
            let per_frame = start.elapsed() / frames;
            eprintln!("{name}: {} rules, {per_frame:?} per 42-row frame", h.rule_count());
        }
    }

    #[test]
    fn first_rule_wins() {
        let h = Highlighter::new(&[kw("WARNING", "#111111"), kw("WARN", "#222222")]);
        assert_eq!(color_at(&h, "x WARNING y", "WARNING"), parse_hex("#111111"));
        assert_eq!(color_at(&h, "x WARN y", "WARN"), parse_hex("#222222"));
    }

    #[test]
    fn real_set_prefers_specific_rules() {
        let sets = parse(DEFAULT_SET).unwrap();
        let h = Highlighter::from_sets(&sets, &[]);
        assert_eq!(color_at(&h, "   Active: inactive (dead)", "inactive"), parse_hex("#A6ADB9"));
        assert_eq!(color_at(&h, "   Active: active (running)", "active"), parse_hex("#87C999"));
        assert_eq!(color_at(&h, "2026 ERROR pay timeout", "ERROR"), parse_hex("#FF4D4F"));
        assert_eq!(color_at(&h, "container is healthy", "healthy"), parse_hex("#87C999"));
        assert_eq!(color_at(&h, "container is unhealthy", "unhealthy"), parse_hex("#FF4D4F"));
    }

    #[test]
    fn rhel_diagnostics_highlight_values_and_keep_context_quiet() {
        let sets = parse(DEFAULT_SET).unwrap();
        let h = Highlighter::from_sets(&sets, &[]);
        for (line, needle, color) in [
            ("Nov 14 04:04:23 rhel-01 kernel: ACPI: INT_SRC_OVR", "kernel", "#B6A0D9"),
            ("systemd[1]: Starting OpenSSH server daemon...", "Starting", "#87BFEA"),
            ("systemd[1]: Started OpenSSH server daemon.", "Started", "#87C999"),
            ("   Active: failed (Result: exit-code)", "failed", "#FF4D4F"),
            ("   Active: active (exited)", "active (exited)", "#87C999"),
            ("   Active: inactive (dead)", "inactive (dead)", "#A6ADB9"),
            ("   Active: activating (start)", "activating", "#87BFEA"),
            ("sshd.service loaded active running OpenSSH server daemon", "active", "#87C999"),
            ("× httpd.service loaded failed failed The Apache HTTP Server", "failed", "#FF4D4F"),
            ("   Main PID: 123 (code=exited, status=0/SUCCESS)", "status=0/SUCCESS", "#87C999"),
            ("   Main PID: 123 (code=exited, status=1/FAILURE)", "status=1/FAILURE", "#FF4D4F"),
            ("type=AVC msg=audit(1:2): avc:  denied  { read } permissive=0", "avc:  denied", "#FF4D4F"),
            ("type=AVC msg=audit(1:2): avc:  denied  { read } permissive=1", "avc:  denied", "#FFAA00"),
            ("Current mode:                   enforcing", "enforcing", "#87C999"),
            ("SELINUX=permissive", "permissive", "#FFAA00"),
            ("SELinux status:                 disabled", "disabled", "#FFAA00"),
            ("Overall Status: Disabled", "Disabled", "#87BFEA"),
            ("Overall Status: Not Registered", "Not Registered", "#FFAA00"),
            ("Overall Status: Registered", "Registered", "#87C999"),
            ("This system is not registered with an entitlement server.", "not registered", "#FFAA00"),
            ("Error: GPG check FAILED", "GPG check FAILED", "#FF4D4F"),
            ("Failed to download metadata for repo 'appstream'", "Failed to download metadata", "#FF4D4F"),
            ("  Installing : httpd-2.4.62-1.el9.x86_64", "Installing", "#87BFEA"),
            ("  Removing : old-package-1.0-1.el9.x86_64", "Removing", "#FFAA00"),
            ("Complete!", "Complete!", "#87C999"),
            ("RHSA-2026:12345 Critical/Sec. kernel-core", "RHSA-2026:12345", "#FFAA00"),
            ("RHBA-2026:12345 bug fix", "RHBA-2026:12345", "#87BFEA"),
            ("CVE-2026-12345", "CVE-2026-12345", "#FFAA00"),
            ("NetworkManager[10]: state change: ip-config -> activated", "activated", "#87C999"),
            ("NetworkManager[10]: state change: activated -> failed", "failed", "#FF4D4F"),
            ("ens192 ethernet disconnected --", "disconnected", "#FFAA00"),
            ("sshd[20]: Failed password for invalid user test from 192.0.2.1", "Failed password", "#FF4D4F"),
            ("kernel: Out of memory: Killed process 123 (java)", "Out of memory", "#FF4D4F"),
            ("kernel: BUG: unable to handle page fault", "BUG:", "#FF4D4F"),
            ("kernel: XFS (dm-0): Corruption detected. Unmount and run xfs_repair", "Corruption detected", "#FF4D4F"),
            ("app: ERROR unexpected failure", "ERROR", "#FF4D4F"),
            ("app: warning request timed out", "warning", "#FFAA00"),
            ("app: errors=0 failed=0", "failed=0", "#A6ADB9"),
        ] {
            let styles = h.styles(line).unwrap_or_else(|| panic!("no highlight for {line:?}"));
            let start = line.find(needle).unwrap();
            assert!(styles[start..start + needle.len()].iter().all(|s| s.and_then(|s| s.fg) == parse_hex(color)), "{needle:?} in {line:?}");
        }
        for (line, plain) in [
            ("   Active: active (running)", "Active:"),
            ("Main PID: 123 (code=exited, status=0/SUCCESS)", "code=exited"),
            ("Nov 14 04:04:23 rhel-01 kernel: ACPI: INT_SRC_OVR", "ACPI: INT_SRC_OVR"),
            ("  Installing : httpd-2.4.62-1.el9.x86_64", "httpd-2.4.62-1.el9.x86_64"),
            ("health: starting", "health:"),
            ("RHBA-2026:12345 bug fix", "bug fix"),
            ("systemd-shutdown[1]: Remounting filesystem read-only", "Remounting filesystem read-only"),
        ] {
            let styles = h.styles(line).unwrap();
            let start = line.find(plain).unwrap();
            assert!(styles[start..start + plain.len()].iter().all(Option::is_none), "{plain:?} in {line:?}");
        }
    }

    #[test]
    fn default_rules_do_not_match_inside_unrelated_words() {
        let sets = parse(DEFAULT_SET).unwrap();
        let h = Highlighter::from_sets(&sets, &[]);
        for line in ["ERRATA", "TERROR", "WARNINGS", "information", "DEBUGGER", "isActive", "exited", "healthyish", "kernelish"] {
            assert!(h.styles(line).is_none(), "unexpected highlight in {line:?}");
        }
    }

    #[test]
    fn default_colours_read_on_every_terminal_theme() {
        let sets = parse(DEFAULT_SET).unwrap();
        let themes = crate::theme::DARK_THEMES.iter().chain(crate::theme::LIGHT_THEMES.iter())
            .map(|t| (t.name, crate::theme::TerminalPalette::from_theme(t), 4.5));
        let colors = crate::theme::DARK_TERMINAL_COLORS.iter().chain(crate::theme::LIGHT_TERMINAL_COLORS)
            .map(|c| (c.name, crate::theme::TerminalPalette::from_colors(c), if c.id == "default" { 4.5 } else { 3.75 }));
        for (name, palette, minimum) in themes.chain(colors) {
            let bg = palette.background;
            let bg = (bg.r as u32) << 16 | (bg.g as u32) << 8 | bg.b as u32;
            for rule in &sets[0].keywords {
                let c = readable_on(rule.fg().unwrap(), palette.dark);
                let fg = (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32;
                let contrast = crate::theme::contrast(fg, bg);
                assert!(contrast >= minimum, "{name}: {fg:06x} has {contrast:.2}:1 contrast");
            }
        }
    }

    #[test]
    fn only_untouched_saved_defaults_are_upgraded() {
        let dir = std::env::temp_dir().join(format!("komando-highlights-{}", uuid::Uuid::new_v4()));
        let legacy = parse(LEGACY_DEFAULT_SET).unwrap();
        let muted = parse(MUTED_DEFAULT_SET).unwrap();
        let bundled = parse(DEFAULT_SET).unwrap();
        assert_eq!(legacy[0].id, bundled[0].id, "disabled-set preferences must keep working");
        assert_eq!(load(&dir).0, bundled, "first run uses the current bundle");
        save(&dir, &legacy).unwrap();
        assert_eq!(load(&dir).0, bundled);
        save(&dir, &muted).unwrap();
        assert_eq!(load(&dir).0, bundled, "untouched muted defaults get the vivid palette");

        for original in [&legacy[0], &muted[0]] {
            let mut renamed = original.clone();
            renamed.name = "My Linux rules".into();
            let mut toggled = original.clone();
            toggled.keywords[0].enabled = false;
            let mut recoloured = original.clone();
            recoloured.keywords[0].foreground_color = Some("#abcdef".into());
            let mut reordered = original.clone();
            reordered.keywords.swap(0, 1);
            let mut imported = original.clone();
            imported.extra.insert("custom".into(), Value::Bool(true));
            for modified in [renamed, toggled, recoloured, reordered, imported, bundled[0].clone()] {
                save(&dir, &[modified.clone()]).unwrap();
                assert_eq!(load(&dir).0, vec![modified]);
            }
        }
        let mut custom = legacy[0].clone();
        custom.id = "custom".into();
        save(&dir, &[legacy[0].clone(), custom.clone()]).unwrap();
        assert_eq!(load(&dir).0, vec![bundled[0].clone(), custom]);
        save(&dir, &[]).unwrap();
        let (sets, unreadable) = load(&dir);
        assert!(sets.is_empty() && unreadable.is_none(), "deleted sets must stay deleted");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreadable_highlights_are_kept_aside_rather_than_overwritten() {
        let dir = std::env::temp_dir().join(format!("komando-highlights-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let corrupt = r#"[{"id": "mine", "name": "My rules", "keywords": [{"text": "#;
        fs::write(dir.join(FILE), corrupt).unwrap();
        let (sets, unreadable) = load(&dir);
        assert_eq!(sets, parse(DEFAULT_SET).unwrap(), "the bundled set stands in");
        let unreadable = unreadable.expect("the user is told");
        assert_eq!(unreadable.in_place, None);
        assert!(!dir.join(FILE).exists(), "the next save must not overwrite it");
        let kept: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        let name = kept[0].file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("highlights.json.broken-"), "{name}");
        assert!(unreadable.message.contains(&kept[0].display().to_string()), "{}", unreadable.message);
        assert_eq!(fs::read_to_string(&kept[0]).unwrap(), corrupt);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plain_text_is_not_a_regex_and_case_rules_apply() {
        let mut dot = kw("a.b", "#123456");
        dot.is_case_sensitive = Some(false);
        let h = Highlighter::new(&[dot]);
        assert!(h.styles("axb").is_none());
        assert_eq!(color_at(&h, "A.B", "A.B"), parse_hex("#123456"));
        let h = Highlighter::new(&[kw("Error", "#123456")]);
        assert!(h.styles("error").is_none());
    }

    #[test]
    fn match_group_colours_only_the_group() {
        let mut k = kw(r"health:\s*(starting)", "#FFA500");
        k.is_reg_exp = Some(true);
        k.is_match_group = Some(true);
        k.match_group = Some("1".into());
        let h = Highlighter::new(&[k]);
        let line = "health: starting";
        let styles = h.styles(line).unwrap();
        assert!(styles[0].is_none());
        assert!(styles[line.find("starting").unwrap()].is_some());
    }

    #[test]
    fn bad_regex_and_js_rules_are_skipped() {
        let mut bad = kw("(?<=x)y", "#fff");
        bad.is_reg_exp = Some(true);
        let mut js = kw("anything", "#fff");
        js.extra.insert("isJS".into(), Value::Bool(true));
        let h = Highlighter::new(&[bad, js, kw("ok", "#fff")]);
        assert_eq!(h.skipped.len(), 2);
        assert_eq!(h.rule_count(), 1);
    }

    #[test]
    fn export_keeps_fields_and_reimports_identically() {
        // Imported legacy/Tabby fields must still round-trip even though the new bundle
        // no longer carries these unused options.
        let sets = parse(LEGACY_DEFAULT_SET).unwrap();
        let exported = export(&sets[0]).unwrap();
        assert!(exported.contains("\"isJS\": false"));
        assert!(exported.contains("\"backgroundColor\": \"0\""));
        assert_eq!(parse(&exported).unwrap(), sets);
    }

    #[test]
    fn import_replaces_same_id_and_adds_new() {
        let mut existing = parse(DEFAULT_SET).unwrap();
        let mut copy = existing[0].clone();
        copy.name = "Renamed".into();
        let mut other = existing[0].clone();
        other.id = "other".into();
        assert_eq!(merge(&mut existing, vec![copy, other]), (1, 1));
        assert_eq!(existing.len(), 2);
        assert_eq!(existing[0].name, "Renamed");
    }

    #[test]
    fn disabled_sets_and_rules_are_ignored() {
        let sets = parse(DEFAULT_SET).unwrap();
        assert_eq!(Highlighter::from_sets(&sets, &[sets[0].id.clone()]).rule_count(), 0);
        let mut k = kw("x", "#fff");
        k.enabled = false;
        assert_eq!(Highlighter::new(&[k]).rule_count(), 0);
    }

    #[test]
    fn the_rule_at_a_spot_is_the_one_painting_it() {
        let set = |id: &str, keywords: Vec<Keyword>| HighlightSet {
            id: id.into(),
            name: id.into(),
            keywords,
            extra: Default::default(),
        };
        let mut off = kw("fail", "#f00");
        off.enabled = false;
        let sets = [set("a", vec![off, kw("failed", "#f00")]), set("b", vec![kw("fail", "#0f0"), kw("ok", "#0f0")])];
        let h = Highlighter::from_sets(&sets, &[]);
        let line = "it failed, ok";
        // "failed" from set a comes first and paints the word, so it is the one named.
        let hit = h.rule_at(line, line.find("led").unwrap()).unwrap();
        assert_eq!(hit.source, RuleSource { set: "a".into(), index: 1 });
        assert_eq!(hit.matched, "failed");
        assert_eq!(h.rule_at(line, line.find("ok").unwrap()).unwrap().source, RuleSource { set: "b".into(), index: 1 });
        assert_eq!(h.rule_at(line, 0), None, "nothing colours it");
        assert_eq!(Highlighter::new(&[kw("ok", "#fff")]).rule_at("ok", 0), None, "no set to point to");
    }

    fn draft(text: &str) -> RuleDraft {
        RuleDraft { text: text.into(), foreground: Some("#FF4D4F".into()), ..RuleDraft::default() }
    }

    #[test]
    fn a_draft_becomes_a_rule_that_colours_what_it_describes() {
        let mut d = draft(r"status=(?P<code>\d+)");
        d.regex = true;
        d.group = "code".into();
        d.bold = true;
        let rule = d.build(None).unwrap();
        assert!(rule.enabled);
        let h = Highlighter::new(&[rule]);
        let line = "exit status=137 now";
        let styles = h.styles(line).unwrap();
        assert!(styles[line.find("status").unwrap()].is_none(), "only the group is coloured");
        let s = styles[line.find("137").unwrap()].unwrap();
        assert_eq!(s.fg, parse_hex("#FF4D4F"));
        assert!(s.bold);
    }

    #[test]
    fn drafts_that_cannot_work_say_why() {
        let cases = [
            (RuleDraft { text: "  ".into(), ..draft("") }, "Enter"),
            (RuleDraft { regex: true, ..draft("(unclosed") }, "regular expression"),
            (RuleDraft { regex: true, ..draft(r"(?<=x)y") }, "regular expression"),
            (RuleDraft { regex: true, group: "2".into(), ..draft("(a)") }, "no group 2"),
            (RuleDraft { regex: true, group: "name".into(), ..draft("(a)") }, "no group name"),
            (RuleDraft { foreground: Some("red".into()), ..draft("x") }, "Text colour"),
            (RuleDraft { background: Some("#12".into()), ..draft("x") }, "Background"),
            (RuleDraft { foreground: None, ..draft("x") }, "colours nothing"),
        ];
        for (d, expected) in cases {
            let err = d.build(None).unwrap_err();
            assert!(err.contains(expected), "{d:?}: {err}");
        }
        // A group on a plain-text rule is ignored rather than refused: plain text has no groups.
        assert!(RuleDraft { group: "1".into(), ..draft("x") }.build(None).is_ok());
    }

    /// Opening a rule in the editor and saving it unchanged must not rewrite it, so exported
    /// sets stay what was imported.
    #[test]
    fn saving_an_unchanged_rule_gives_back_the_same_rule() {
        for text in [DEFAULT_SET, LEGACY_DEFAULT_SET] {
            for rule in &parse(text).unwrap()[0].keywords {
                if rule.extra.get("isJS").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                assert_eq!(&RuleDraft::from_keyword(rule).build(Some(rule)).unwrap(), rule, "{}", rule.text);
            }
        }
    }

    #[test]
    fn editing_keeps_what_the_editor_does_not_show() {
        let mut rule = kw("old", "#123456");
        rule.enabled = false;
        rule.extra.insert("custom".into(), Value::Bool(true));
        let mut d = RuleDraft::from_keyword(&rule);
        d.text = "new".into();
        d.foreground = None;
        d.background = Some("#abc".into());
        let edited = d.build(Some(&rule)).unwrap();
        assert_eq!(edited.text, "new");
        assert!(!edited.enabled, "a switched-off rule stays off");
        assert_eq!(edited.extra.get("custom"), Some(&Value::Bool(true)));
        assert_eq!(edited.fg(), None, "the removed text colour is off");
        assert_eq!(edited.bg(), parse_hex("#AABBCC"));
        assert_eq!(edited.background_color.as_deref(), Some("#AABBCC"), "stored in the long form");
    }

    #[test]
    fn a_new_set_has_its_own_id() {
        let (a, b) = (new_set("Ops"), new_set("Ops"));
        assert_eq!(a.name, "Ops");
        assert!(a.keywords.is_empty());
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn colours_are_adjusted_to_read_on_the_terminal() {
        let lime = parse_hex("#00FF00").unwrap();
        assert_eq!(readable_on(lime, true), lime);
        let adjusted = readable_on(lime, false);
        assert!(adjusted.g < lime.g);
        let blue = parse_hex("#0000FF").unwrap();
        let lifted = readable_on(blue, true);
        assert!(lifted.r > 0 && lifted.b == 0xff, "{lifted:?}");
        assert_eq!(readable_on(blue, false), blue);
        assert_eq!(parse_hex("#abc"), Some(Rgb8 { r: 0xaa, g: 0xbb, b: 0xcc }));
        assert_eq!(parse_hex("0"), None);
    }
    /// Representative terminal repaint, including the per-cell contrast work done by the
    /// renderer. Run explicitly in release mode; reports timings without flaky thresholds.
    #[test]
    #[ignore]
    fn stress_highlight_repaints() {
        let sets = parse(DEFAULT_SET).unwrap();
        let h = Highlighter::from_sets(&sets, &[]);
        let rows: Vec<String> = (0..40).map(|i| format!(
            "2026-11-12T18:22:31 INFO project-{i:03} 192.168.0.20 Active: active (running) health: starting ERROR timeout WARNING {}",
            "successful ".repeat(4)
        )).collect();
        for dark in [true, false] {
            for prepared in [false, true] {
                let start = std::time::Instant::now();
                for _ in 0..500 {
                    for row in &rows {
                        let styles = if prepared { h.styles_for_background(row, dark) } else { h.styles(row) }.unwrap();
                        for style in styles.into_iter().flatten() {
                            if let Some(c) = style.fg {
                                std::hint::black_box(if prepared || style.bg.is_some() { c } else { readable_on(c, dark) });
                            }
                        }
                    }
                }
                eprintln!("highlight repaint dark={dark} prepared={prepared}: {:?}/frame (40 rows, 500 frames)", start.elapsed() / 500);
            }
        }
    }

    #[test]
    fn prepared_styles_preserve_matches_precedence_and_backgrounds() {
        let mut background = kw("special", "#00ff00");
        background.background = Some(true);
        background.background_color = Some("#123456".into());
        let mut group = kw(r"health: (?P<state>starting)", "#0000ff");
        group.is_reg_exp = Some(true);
        group.is_match_group = Some(true);
        group.match_group = Some("state".into());
        let sets = parse(DEFAULT_SET).unwrap();
        let h = Highlighter::new(sets[0].keywords.iter().chain([&background, &group]));
        for dark in [false, true] {
            for line in ["WARNING WARN ERROR café 日本語", "special health: starting", "no matches here", ""] {
                let expected = h.styles(line).map(|styles| styles.into_iter().map(|s| s.map(|s| Style {
                    fg: s.fg.map(|c| if s.bg.is_some() { c } else { readable_on(c, dark) }),
                    ..s
                })).collect::<Vec<_>>());
                assert_eq!(h.styles_for_background(line, dark), expected, "{line}, dark={dark}");
            }
        }
    }

}
