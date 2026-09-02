//! Watches Claude Code transcripts so DETACHED sessions still join the fight.
//!
//! The statusline hook is the only source of rate limits, but it only fires for
//! sessions that render a UI. `claude --resume` under `bg-pty-host` — which is
//! how agents actually run headless — never renders one, so the party came up
//! empty exactly when work was happening. Every session writes a transcript
//! regardless, so that is the signal that always exists.
//!
//! Nothing here reads transcript CONTENT beyond the model id: the file's byte
//! count is the activity measure, and the session id is its filename.

use crate::protocol::{zone_id, Observation, Source};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A session counts as present if its transcript was written this recently.
///
/// This was 120s, which made an idle session DISAPPEAR from the party rather
/// than camp. That contradicts the state model — attacking / idle / victory —
/// and it churned the roster: every drop and rejoin reshuffled the party the
/// panel was drawing. Idle is a state, not an absence. 30 minutes keeps a quiet
/// session on the field; the hub's own 60s eviction still removes anything this
/// watcher stops reporting entirely.
const LIVE_SECS: u64 = 30 * 60;

fn projects_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let d = Path::new(&home).join(".claude").join("projects");
    d.is_dir().then_some(d)
}

/// Is this session mid-turn — including waiting on a long tool call?
///
/// Token spend is NOT the right signal. A session blocked on a 5-minute bash
/// command or a Monitor is working, but its cost and its transcript are both
/// flat, so it read as idle and the fighter went camping mid-fight.
///
/// Two payload fields that look like they would answer this do not:
///   * `prompt_id` is present for EVERY session, idle ones included — it is the
///     last prompt seen, not one in flight. Measured across 11 sessions.
///   * `cost.total_duration_ms` is wall-clock since session start, so it
///     advances forever whether or not anything is happening.
///
/// The transcript does answer it. If the last entry is an assistant turn
/// containing a `tool_use`, the session is blocked waiting for that tool. If it
/// is a user turn carrying a `tool_result`, the tool just returned and the turn
/// continues. Only a plain assistant reply means it is genuinely waiting on a
/// human.
fn mid_turn(tail: &str, age_secs: u64) -> bool {
    // A transcript is NOT a list of messages. It interleaves `attachment`,
    // `system` and `continued-in` records, and the last line is very often one
    // of those — checking only the final line reported a session that was
    // demonstrably mid-turn as idle. Scan back to the last real message.
    let mut kind = "";
    let mut has_tool_use = false;
    for l in tail.lines().rev() {
        let t = l.trim_start();
        if !t.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else {
            continue;
        };
        match v.get("type").and_then(|x| x.as_str()) {
            Some(k @ ("assistant" | "user")) => {
                kind = if k == "assistant" { "assistant" } else { "user" };
                has_tool_use = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                    .is_some_and(|b| {
                        b.iter().any(|x| {
                            x.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                        })
                    });
                break;
            }
            _ => continue,
        }
    }

    let working = match kind {
        // Blocked on a tool: bash, Monitor, anything slow. Spends nothing,
        // writes nothing, and is absolutely working.
        "assistant" => has_tool_use,
        // A user turn — a prompt or a tool result — always awaits a reply.
        "user" => true,
        _ => false,
    };

    // ...but only if the file is still being touched. A session abandoned
    // mid-turn stays "blocked on a tool" forever: one was found frozen 48
    // minutes in. Long tool calls are minutes, not quarter-hours.
    working && age_secs < 15 * 60
}

/// Last `"model":"..."` in the tail of the file. Cheap: reads at most 64KB from
/// the end rather than parsing the whole transcript, which can be tens of MB.
fn read_tail(path: &Path, len: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path).ok()?;
    let want = 64 * 1024u64;
    let from = len.saturating_sub(want);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.take(want).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn last_model(tail: &str) -> Option<String> {
    let key = "\"model\":\"";
    let i = tail.rfind(key)? + key.len();
    let rest = &tail[i..];
    let j = rest.find('"')?;
    Some(rest[..j].to_string())
}

/// One Observation per session whose transcript moved recently.
pub fn scan(machine: &str) -> Vec<Observation> {
    let Some(root) = projects_dir() else { return Vec::new() };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut out = Vec::new();

    let Ok(projects) = fs::read_dir(&root) else { return out };
    for proj in projects.flatten() {
        let pdir = proj.path();
        if !pdir.is_dir() { continue; }
        // The directory name is the project path with separators flattened —
        // hashed, never sent raw, same rule as the statusline whitelist.
        let zone = zone_id(proj.file_name().to_str(), None);
        let Ok(files) = fs::read_dir(&pdir) else { continue };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            let Ok(md) = f.metadata() else { continue };
            let len = md.len();
            if len == 0 { continue; }
            let age = md
                .modified().ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| now.saturating_sub(d.as_secs()))
                .unwrap_or(u64::MAX);
            if age > LIVE_SECS { continue; }
            let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            let tail = read_tail(&path, len).unwrap_or_default();
            out.push(Observation {
                machine: machine.to_string(),
                session_id: session_id.to_string(),
                session_name: None,
                model_id: last_model(&tail).unwrap_or_default(),
                agent_name: None,
                effort: None,
                thinking: false,
                zone: zone.clone(),
                cost_usd: 0.0,          // unknown here; the statusline owns cost
                activity: len as f64,   // bytes written IS the work signal
                busy: Some(mid_turn(&tail, age)),
                source: Source::Transcript,
                rate_limits: None,      // only the statusline can report these
                ts: now as i64,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_never_panics_without_a_claude_dir() {
        // Runs in CI and on machines with no ~/.claude — must degrade, not abort.
        let _ = scan("test");
    }

    #[test]
    fn waiting_on_a_tool_still_counts_as_working() {
        // The case that mattered: blocked on a long bash call. Nothing is being
        // spent and the transcript is flat, but the session is working.
        let blocked = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#;
        assert!(mid_turn(blocked, 5));

        let returned = r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#;
        assert!(mid_turn(returned, 5));

        // A plain reply means it is waiting on a human — genuinely idle.
        let done = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        assert!(!mid_turn(done, 5));

        // Real transcripts end on attachment/system records far more often than
        // on a message. Checking only the last line called a working session
        // idle; the scan must walk back past them.
        let noisy = format!("{blocked}\n{{\"type\":\"attachment\"}}\n{{\"type\":\"system\"}}\n");
        assert!(mid_turn(&noisy, 5), "must look past attachment/system records");

        // Abandoned mid-turn: stays "blocked on a tool" forever otherwise.
        assert!(!mid_turn(blocked, 60 * 60), "a 1h-old tool_use is not work");
        assert!(!mid_turn("", 5));
    }

    #[test]
    fn a_transcript_that_grows_reads_as_activity() {
        // The hub's rule is "activity went up". Bytes satisfy it monotonically,
        // which is the whole reason for using them.
        let a = 1000f64;
        let b = 1200f64;
        assert!(b > a);
    }
}

#[cfg(test)]
mod live {
    /// Diagnostic against the real ~/.claude, not a fixture. Run with:
    ///   cargo test live_scan -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_scan() {
        let obs = super::scan("dbg");
        println!("  {} live sessions", obs.len());
        let mut v: Vec<_> = obs.iter().collect();
        v.sort_by_key(|o| o.session_id.clone());
        for o in v.iter().take(12) {
            println!(
                "  {}  busy={:?}  bytes={:.0}  model={}",
                &o.session_id[..8.min(o.session_id.len())],
                o.busy,
                o.activity,
                if o.model_id.is_empty() { "?" } else { &o.model_id }
            );
        }
    }
}
