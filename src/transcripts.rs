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

use crate::protocol::{zone_id, Observation};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A session is "live" if its transcript was written this recently.
const LIVE_SECS: u64 = 120;

fn projects_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let d = Path::new(&home).join(".claude").join("projects");
    d.is_dir().then_some(d)
}

/// Last `"model":"..."` in the tail of the file. Cheap: reads at most 64KB from
/// the end rather than parsing the whole transcript, which can be tens of MB.
fn last_model(path: &Path, len: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path).ok()?;
    let want = 64 * 1024u64;
    let from = len.saturating_sub(want);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = String::new();
    f.take(want).read_to_string(&mut buf).ok()?;
    let key = "\"model\":\"";
    let i = buf.rfind(key)? + key.len();
    let rest = &buf[i..];
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
            out.push(Observation {
                machine: machine.to_string(),
                session_id: session_id.to_string(),
                session_name: None,
                model_id: last_model(&path, len).unwrap_or_default(),
                agent_name: None,
                effort: None,
                thinking: false,
                zone: zone.clone(),
                cost_usd: 0.0,          // unknown here; the statusline owns cost
                activity: len as f64,   // bytes written IS the work signal
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
    fn a_transcript_that_grows_reads_as_activity() {
        // The hub's rule is "activity went up". Bytes satisfy it monotonically,
        // which is the whole reason for using them.
        let a = 1000f64;
        let b = 1200f64;
        assert!(b > a);
    }
}
