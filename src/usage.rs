//! The usage ping: once a day, an anonymous event to Aptabase, so the developers can estimate
//! how many people use Komando.
//!
//! It carries Komando's version and the operating system's name and nothing else: no hosts,
//! addresses, commands, user names or a lasting id. Each ping gets a fresh random session id;
//! Aptabase counts daily users from a hash it salts anew every day, so pings can't be linked
//! across days. On by default, also for installs from before it existed (`Settings::usage_ping`);
//! switched off in the welcome tour or Settings → About, nothing is sent.
//!
//! The day of the last ping (UTC) is kept in `usage-ping.txt` in the data folder, so a restart
//! doesn't send another one the same day. A ping that fails is tried again later that day.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, anyhow};
use chrono::{NaiveDate, SecondsFormat, Utc};
use rand::RngExt;

/// The Aptabase app key, from `KOMANDO_APTABASE_APP_KEY` when Komando is built (the release
/// workflow sets it from a repository secret). Its region (`A-EU-…`, `A-US-…`) picks the
/// server. Builds without it, such as local and fork builds, send nothing.
const APP_KEY: &str = match option_env!("KOMANDO_APTABASE_APP_KEY") {
    Some(key) => key,
    None => "",
};

/// The one event a ping sends.
const EVENT: &str = "daily_ping";

/// Where the day of the last successful ping is kept, in the data folder.
const STATE_FILE: &str = "usage-ping.txt";

/// The ingest endpoint for an app key, from its region.
fn endpoint(app_key: &str) -> Option<&'static str> {
    match app_key.split('-').nth(1)? {
        "EU" => Some("https://eu.aptabase.com/api/v0/events"),
        "US" => Some("https://us.aptabase.com/api/v0/events"),
        _ => None,
    }
}

/// The day this process last sent a ping, whether or not `STATE_FILE` could be written.
static SENT_ON: std::sync::Mutex<Option<NaiveDate>> = std::sync::Mutex::new(None);

/// A ping is due when none was sent today.
fn due(last: Option<NaiveDate>, today: NaiveDate) -> bool {
    last.is_none_or(|last| last < today)
}

fn last_ping(data_dir: &Path) -> Option<NaiveDate> {
    let text = fs::read_to_string(data_dir.join(STATE_FILE)).ok()?;
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()
}

fn os_name() -> &'static str {
    if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    }
}

/// The event as Aptabase's ingest API takes it: a list of events.
fn body(now: chrono::DateTime<Utc>, session_id: u64) -> String {
    serde_json::json!([{
        "timestamp": now.to_rfc3339_opts(SecondsFormat::Millis, true),
        "sessionId": session_id.to_string(),
        "eventName": EVENT,
        "systemProps": {
            "isDebug": cfg!(debug_assertions),
            "osName": os_name(),
            "osVersion": "",
            "locale": "",
            "appVersion": env!("CARGO_PKG_VERSION"),
            "sdkVersion": concat!("komando@", env!("CARGO_PKG_VERSION")),
        },
        "props": {},
    }])
    .to_string()
}

/// Send today's ping unless one was sent already. Blocking: call it off the UI thread.
/// `Ok(true)` when a ping went out.
pub fn ping_if_due(data_dir: &Path) -> Result<bool> {
    // Tests never reach the network.
    if cfg!(test) {
        return Ok(false);
    }
    let Some(url) = endpoint(APP_KEY) else { return Ok(false) };
    // One ping at a time, so two can't both find today's still due.
    static SENDING: AtomicBool = AtomicBool::new(false);
    if SENDING.swap(true, Ordering::AcqRel) {
        return Ok(false);
    }
    let result = send_if_due(data_dir, url);
    SENDING.store(false, Ordering::Release);
    result
}

fn send_if_due(data_dir: &Path, url: &str) -> Result<bool> {
    let now = Utc::now();
    let today = now.date_naive();
    let sent_here = *SENT_ON.lock().unwrap_or_else(|e| e.into_inner());
    if !due(last_ping(data_dir).max(sent_here), today) {
        return Ok(false);
    }
    // As Aptabase's own SDKs make them: the time in seconds and a random part.
    let session_id = now.timestamp() as u64 * 100_000_000 + rand::rng().random_range(0..100_000_000);
    crate::update::post_json(url, &[("App-Key", APP_KEY)], &body(now, session_id)).map_err(|err| match err {
        ureq::Error::Status(code, _) => anyhow!("Aptabase answered with error {code}"),
        ureq::Error::Transport(err) => anyhow!("Couldn't reach Aptabase: {err}"),
    })?;
    // Remembered here too: when the file can't be written (a read-only or full disk), the
    // hourly look would otherwise send another ping every hour.
    *SENT_ON.lock().unwrap_or_else(|e| e.into_inner()) = Some(today);
    crate::model::write_atomic(&data_dir.join(STATE_FILE), today.format("%Y-%m-%d").to_string().as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn one_ping_a_day() {
        let today = day("2026-11-18");
        assert!(due(None, today));
        assert!(due(Some(day("2026-11-17")), today));
        assert!(!due(Some(today), today));
        // A clock set back doesn't send a second one.
        assert!(!due(Some(day("2026-11-19")), today));
    }

    #[test]
    fn the_app_key_picks_the_region() {
        assert_eq!(endpoint("A-EU-1234567890"), Some("https://eu.aptabase.com/api/v0/events"));
        assert_eq!(endpoint("A-US-1234567890"), Some("https://us.aptabase.com/api/v0/events"));
        assert_eq!(endpoint(""), None);
        assert_eq!(endpoint("A-SH-1234567890"), None);
    }

    #[test]
    fn the_last_ping_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("komando-usage-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(last_ping(&dir), None);
        fs::write(dir.join(STATE_FILE), "2026-11-18\n").unwrap();
        assert_eq!(last_ping(&dir), Some(day("2026-11-18")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_event_carries_only_version_and_os() {
        let now = Utc::now();
        let json: serde_json::Value = serde_json::from_str(&body(now, 42)).unwrap();
        let event = &json[0];
        assert_eq!(event["eventName"], EVENT);
        assert_eq!(event["sessionId"], "42");
        assert_eq!(event["props"], serde_json::json!({}));
        assert_eq!(event["systemProps"]["appVersion"], env!("CARGO_PKG_VERSION"));
    }
}
