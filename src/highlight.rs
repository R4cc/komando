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

use crate::model::write_atomic;

pub const FILE: &str = "highlights.json";
const DEFAULT_SET: &str = include_str!("../assets/highlights/default.json");

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
}

impl Highlighter {
    pub fn new<'a>(keywords: impl IntoIterator<Item = &'a Keyword>) -> Self {
        let mut rules = Vec::new();
        let mut patterns = Vec::new();
        let mut skipped = Vec::new();
        for kw in keywords {
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
            let pattern = if kw.is_regex() { kw.text.clone() } else { regex::escape(&kw.text) };
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
            rules.push(Rule { regex, group, style });
        }
        let set = (!patterns.is_empty()).then(|| RegexSetBuilder::new(&patterns).build().ok()).flatten();
        Self { set, rules, skipped }
    }

    pub fn from_sets(sets: &[HighlightSet], disabled_sets: &[String]) -> Self {
        Self::new(sets.iter().filter(|s| !disabled_sets.contains(&s.id)).flat_map(|s| s.keywords.iter()))
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// One entry per byte of `line`: the style of the first rule that covers it.
    pub fn styles(&self, line: &str) -> Option<Vec<Option<Style>>> {
        let set = self.set.as_ref()?;
        let matched = set.matches(line);
        if !matched.matched_any() {
            return None;
        }
        let mut out: Vec<Option<Style>> = vec![None; line.len()];
        for index in matched.iter() {
            let rule = &self.rules[index];
            let mut paint = |start: usize, end: usize| {
                for slot in &mut out[start..end] {
                    if slot.is_none() {
                        *slot = Some(rule.style);
                    }
                }
            };
            match &rule.group {
                Group::Whole => {
                    for m in rule.regex.find_iter(line) {
                        paint(m.start(), m.end());
                    }
                }
                Group::Index(i) => {
                    for caps in rule.regex.captures_iter(line) {
                        if let Some(m) = caps.get(*i) {
                            paint(m.start(), m.end());
                        }
                    }
                }
                Group::Name(name) => {
                    for caps in rule.regex.captures_iter(line) {
                        if let Some(m) = caps.name(name) {
                            paint(m.start(), m.end());
                        }
                    }
                }
            }
        }
        Some(out)
    }
}

fn first_line(s: &str) -> String {
    s.lines().last().unwrap_or(s).trim().to_string()
}

/// Parse an imported file: one set object, or an array of sets.
pub fn parse(text: &str) -> Result<Vec<HighlightSet>> {
    let value: Value = serde_json::from_str(text).context("not valid JSON")?;
    let sets = match value {
        Value::Array(items) => items
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<HighlightSet>, _>>()
            .context("not a list of highlight sets")?,
        Value::Object(_) => vec![serde_json::from_value(value).context("not a highlight set")?],
        _ => bail!("expected a highlight set"),
    };
    if sets.is_empty() {
        bail!("the file contains no highlight sets");
    }
    Ok(sets)
}

pub fn load(dir: &Path) -> Vec<HighlightSet> {
    match fs::read_to_string(dir.join(FILE)) {
        Ok(text) => parse(&text).unwrap_or_else(|err| {
            log::error!("{FILE}: {err:#}");
            vec![]
        }),
        // First run: start with the bundled set.
        Err(_) => parse(DEFAULT_SET).unwrap_or_default(),
    }
}

pub fn save(dir: &Path, sets: &[HighlightSet]) -> Result<()> {
    fs::create_dir_all(dir)?;
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

/// Rule colours are picked for some background, not necessarily ours. Each is adjusted until
/// it reads at 4.5:1 or better against the terminal: on a light terminal very light colours
/// (#00FF00, #FFD700) are darkened, on a dark one very dark colours (#0000FF) are lightened.
/// Colours that already read well are left as they are.
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
            // Against near-black (L ≈ 0.004): (L + 0.05) / 0.054 >= 4.5  =>  L >= 0.19
            if luminance(c) >= 0.19 {
                break;
            }
            let lift = |v: u8| (v as f32 + (255.0 - v as f32) * 0.15).round() as u8;
            c = Rgb8 { r: lift(c.r), g: lift(c.g), b: lift(c.b) };
        } else {
            // Against near-white (L ≈ 0.97): 1.02 / (L + 0.05) >= 4.5  =>  L <= 0.18
            if luminance(c) <= 0.18 {
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
        assert_eq!(sets[0].name, "ADJUSTED");
        assert_eq!(sets[0].keywords.len(), 69);
        let h = Highlighter::from_sets(&sets, &[]);
        assert!(h.skipped.is_empty(), "{:?}", h.skipped);
        assert_eq!(h.rule_count(), 69);
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
        assert_eq!(color_at(&h, "   Active: inactive (dead)", "Active"), parse_hex("#FF4500"));
        assert_eq!(color_at(&h, "   Active: active (running)", "Active"), parse_hex("#00FF00"));
        assert_eq!(color_at(&h, "2026 ERROR pay timeout", "ERROR"), parse_hex("#FF0000"));
        assert_eq!(color_at(&h, "container is healthy", "healthy"), parse_hex("#00FF00"));
        assert_eq!(color_at(&h, "container is unhealthy", "unhealthy"), parse_hex("#FF4500"));
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
        let sets = parse(DEFAULT_SET).unwrap();
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
}
