//! The `/api/oauth/usage` endpoint — the only source that reports BOTH windows.
//!
//! The statusline is not sufficient. Measured on a live tailnet: 92 consecutive
//! payloads, 92 carrying `seven_day`, **zero** carrying `five_hour`, while this
//! endpoint reported `five_hour` at 2% at the same moment. So an absent window
//! does NOT mean zero usage — Claude Code simply omits it — and inferring zero
//! from absence would have been wrong.
//!
//! UNDOCUMENTED and unsupported. It is what `/usage` itself calls, and it may
//! change or vanish in any Claude Code release. Opt-in for that reason, and
//! because it reads the OAuth token.
//!
//! The token is read locally and sent only to api.anthropic.com, which is where
//! it already goes. It is never logged, and never leaves the machine in any
//! Observation.

use crate::protocol::{RateLimits, Window};
use anyhow::{anyhow, Result};

const URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// macOS keeps the credential in the keychain; Linux in a 0600 JSON file.
fn access_token() -> Result<String> {
    if let Ok(home) = std::env::var("HOME") {
        let p = std::path::Path::new(&home)
            .join(".claude")
            .join(".credentials.json");
        if let Ok(txt) = std::fs::read_to_string(&p) {
            if let Some(t) = token_from_json(&txt) {
                return Ok(t);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .output()?;
        if out.status.success() {
            let txt = String::from_utf8_lossy(&out.stdout);
            if let Some(t) = token_from_json(&txt) {
                return Ok(t);
            }
        }
    }
    Err(anyhow!("no Claude Code OAuth token found"))
}

fn token_from_json(txt: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(txt.trim()).ok()?;
    v.pointer("/claudeAiOauth/accessToken")
        .or_else(|| v.pointer("/accessToken"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

/// `2026-09-03T07:00:00.324645+00:00` -> unix seconds.
///
/// Hand-rolled rather than pulling in a date crate: the format is fixed, and
/// this is Howard Hinnant's days_from_civil, which is well-trodden. Only UTC
/// offsets are produced by this API.
fn iso_to_epoch(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (hh, mm, ss) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

fn window(v: &serde_json::Value, key: &str) -> Option<Window> {
    let w = v.get(key)?;
    let used = w.get("utilization")?.as_f64()? as f32;
    let resets_at = iso_to_epoch(w.get("resets_at")?.as_str()?)?;
    Some(Window {
        used_percentage: used,
        resets_at,
    })
}

/// Blocking: call from `spawn_blocking`.
pub fn fetch() -> Result<RateLimits> {
    let token = access_token()?;
    let body: serde_json::Value = ureq::get(URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .call()?
        .into_json()?;
    let rl = RateLimits {
        five_hour: window(&body, "five_hour"),
        seven_day: window(&body, "seven_day"),
    };
    if rl.five_hour.is_none() && rl.seven_day.is_none() {
        return Err(anyhow!("usage response carried neither window"));
    }
    Ok(rl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_parses_against_known_epochs() {
        assert_eq!(iso_to_epoch("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(
            iso_to_epoch("2000-01-01T00:00:00.000000+00:00"),
            Some(946_684_800)
        );
        // A leap day, which naive month arithmetic gets wrong.
        assert_eq!(
            iso_to_epoch("2024-02-29T12:00:00.000000+00:00"),
            Some(1_709_208_000)
        );
        assert_eq!(
            iso_to_epoch("2026-09-03T07:00:00.324645+00:00"),
            Some(1_788_418_800)
        );
        assert_eq!(iso_to_epoch("nonsense"), None);
    }

    #[test]
    fn windows_come_out_of_a_real_response_shape() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"five_hour":{"utilization":2.0,"resets_at":"2026-09-03T07:00:00.324645+00:00"},
                "seven_day":{"utilization":40.0,"resets_at":"2026-09-06T12:00:00.324668+00:00"}}"#,
        )
        .unwrap();
        let f = window(&v, "five_hour").unwrap();
        assert_eq!(f.used_percentage, 2.0);
        assert_eq!(f.resets_at, 1_788_418_800);
        // The weekly epoch the live panel has been showing all session.
        assert_eq!(window(&v, "seven_day").unwrap().resets_at, 1_788_696_000);
        assert_eq!(window(&v, "seven_day").unwrap().used_percentage, 40.0);
        assert!(window(&v, "nope").is_none());
    }

    #[test]
    fn token_is_read_from_either_shape() {
        assert_eq!(
            token_from_json(r#"{"claudeAiOauth":{"accessToken":"abc"}}"#).as_deref(),
            Some("abc")
        );
        assert_eq!(
            token_from_json(r#"{"accessToken":"xyz"}"#).as_deref(),
            Some("xyz")
        );
        assert_eq!(token_from_json("not json"), None);
    }
}
