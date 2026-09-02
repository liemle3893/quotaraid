//! The wire format, shared by `agent` and `hub` so it cannot drift.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Where an Observation came from. The two sources measure activity in
/// different units — cost in dollars vs transcript bytes — and BOTH describe the
/// same session, so they must not share a counter. They did once: bytes
/// (~500k) always exceeded cost (~1.5), so every transcript update looked like
/// work and every statusline update looked idle. Rate limits, which only the
/// statusline carries, were then never trusted and the boss had no HP at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Statusline,
    Transcript,
}

impl Default for Source {
    fn default() -> Self {
        Source::Statusline
    }
}

/// One rate-limit window as Claude Code reports it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub used_percentage: f32,
    pub resets_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimits {
    #[serde(default)]
    pub five_hour: Option<Window>,
    #[serde(default)]
    pub seven_day: Option<Window>,
}

/// Agent -> hub. A WHITELIST: only fields something actually draws.
///
/// `cwd`, `transcript_path` and the raw repo owner/name are deliberately absent.
/// Nothing renders them, so the minimal payload and the non-leaking payload are
/// the same struct — the safe choice costs nothing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub machine: String,
    pub session_id: String,
    pub session_name: Option<String>,
    pub model_id: String,
    pub agent_name: Option<String>,
    pub effort: Option<String>,
    pub thinking: bool,
    pub zone: String,
    pub cost_usd: f64,
    /// Any monotonically increasing measure of work. The statusline sets it to
    /// cost; the transcript watcher sets it to bytes written. The hub only ever
    /// asks "did this go up", so the unit does not matter — but the SOURCE does:
    /// detached sessions (`claude --resume` under bg-pty-host) never render a
    /// statusline, so cost alone leaves the party empty exactly when agents are
    /// working headless.
    #[serde(default)]
    pub activity: f64,
    #[serde(default)]
    pub source: Source,
    pub rate_limits: Option<RateLimits>,
    pub ts: i64,
}

fn s(v: &serde_json::Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for k in path {
        cur = cur.get(k)?;
    }
    cur.as_str().map(|x| x.to_string())
}

/// First 8 hex of sha256 — battlegrounds stay distinguishable without being
/// readable. sha2 rather than std's DefaultHasher because DefaultHasher's
/// output is not stable across Rust releases, and two machines must agree.
pub fn zone_id(repo: Option<&str>, cwd: Option<&str>) -> String {
    let src = repo.or(cwd).unwrap_or("unknown");
    let d = Sha256::digest(src.as_bytes());
    d.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

pub fn class_of(model_id: &str) -> &'static str {
    let m = model_id.to_ascii_lowercase();
    for (needle, class) in [
        ("opus", "opus"),
        ("sonnet", "sonnet"),
        ("haiku", "haiku"),
        ("fable", "fable"),
    ] {
        if m.contains(needle) {
            return class;
        }
    }
    "other"
}

/// The panel's font covers 0x20-0x7E only, and the line format is
/// space-separated. A `session_name` is whatever the user typed into `/rename`,
/// so it is sanitised HERE — the device cannot defend itself and would render
/// silent blanks (BOARD.md 3b).
pub fn sanitize(input: &str, max: usize) -> String {
    let mut out = String::new();
    for c in input.chars() {
        if out.chars().count() >= max {
            break;
        }
        let c = c.to_ascii_uppercase();
        match c {
            ' ' | '\t' => out.push('_'),
            c if (0x21u8..=0x7E).contains(&(c as u32 as u8)) && c.is_ascii() => out.push(c),
            _ => {}
        }
    }
    if out.is_empty() {
        out.push('?');
    }
    out
}

impl Observation {
    /// Build from a raw Claude Code statusline payload. `None` when the payload
    /// carries no session — nothing else is worth reporting.
    pub fn from_statusline(v: &serde_json::Value, machine: &str, ts: i64) -> Option<Self> {
        let session_id = s(v, &["session_id"])?;
        let repo = match (
            s(v, &["workspace", "repo", "owner"]),
            s(v, &["workspace", "repo", "name"]),
        ) {
            (Some(o), Some(n)) => Some(format!("{o}/{n}")),
            _ => None,
        };
        let cwd = s(v, &["cwd"]).or_else(|| s(v, &["workspace", "current_dir"]));
        let cost = v
            .get("cost")
            .and_then(|c| c.get("total_cost_usd"))
            .and_then(|n| n.as_f64())
            .unwrap_or(0.0);
        Some(Observation {
            machine: machine.to_string(),
            session_id,
            session_name: s(v, &["session_name"]),
            model_id: s(v, &["model", "id"]).unwrap_or_default(),
            agent_name: s(v, &["agent", "name"]),
            effort: s(v, &["effort", "level"]),
            thinking: v
                .get("thinking")
                .and_then(|t| t.get("enabled"))
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
            zone: zone_id(repo.as_deref(), cwd.as_deref()),
            cost_usd: cost,
            activity: cost,
            source: Source::Statusline,
            rate_limits: v
                .get("rate_limits")
                .and_then(|r| serde_json::from_value(r.clone()).ok()),
            ts,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Fighter {
    pub machine: String,
    pub session_id: String,
    pub name: String,
    pub class: String,
    pub zone: String,
    pub thinking: bool,
    pub effort: Option<String>,
    pub cost: f64,
    #[serde(skip)]
    pub act_sl: f64,
    #[serde(skip)]
    pub act_tr: f64,
    #[serde(skip)]
    pub last_seen_ms: u64,
    #[serde(skip)]
    pub last_move_ms: u64,
}

pub const FIGHTER_TTL_MS: u64 = 60_000;
pub const COMBAT_MS: u64 = 15_000;

impl Fighter {
    pub fn attacking(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.last_move_ms) < COMBAT_MS
    }
}

/// Rate limits go stale. Only the statusline can report them, and it only fires
/// for sessions that render a UI — so with everything detached the last known
/// numbers can be hours old. Serving them anyway made the panel display a
/// confident, wrong boss HP for an entire session. Past this age they are
/// reported as unknown instead.
pub const BOSS_TTL_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Default, Serialize)]
pub struct World {
    pub boss: RateLimits,
    #[serde(skip)]
    pub boss_seen_ms: Option<u64>,
    #[serde(serialize_with = "fighters_as_vec")]
    pub fighters: HashMap<String, Fighter>,
}

fn fighters_as_vec<S: serde::Serializer>(
    m: &HashMap<String, Fighter>,
    s: S,
) -> Result<S::Ok, S::Error> {
    let mut v: Vec<&Fighter> = m.values().collect();
    v.sort_by(|a, b| (&a.machine, &a.session_id).cmp(&(&b.machine, &b.session_id)));
    serde::Serialize::serialize(&v, s)
}

/// Combine an incoming window with what we already had.
///
/// Every session reports whatever IT last saw in an API response header, so
/// idle sessions carry stale percentages. Measured on one tailnet: 11 sessions
/// reporting 8 different values (7% to 45%) for the SAME window. "Newest
/// observation wins" is really "whichever session ticked last", so the boss HP
/// swung wildly with nothing changing.
///
/// Within a window, usage only accrues — so the HIGHEST value seen is the best
/// estimate, and it is monotonic. A larger `resets_at` means the window rolled,
/// and the new one starts from its own value rather than inheriting the old
/// maximum.
fn merge_window(cur: Option<Window>, new: Option<Window>) -> Option<Window> {
    match (cur, new) {
        (_, None) => cur,
        (None, Some(n)) => Some(n),
        (Some(c), Some(n)) => Some(if n.resets_at > c.resets_at {
            n                                   // window rolled — start fresh
        } else if n.resets_at < c.resets_at {
            c                                   // a straggler from the old window
        } else if n.used_percentage > c.used_percentage {
            n                                   // same window, more usage seen
        } else {
            c
        }),
    }
}

impl World {
    pub fn apply(&mut self, o: Observation, now_ms: u64) {
        let key = format!("{}/{}", o.machine, o.session_id);

        // Did THIS session just do work? A statusline carries whatever that
        // session last saw in an API response header, and an idle session
        // re-sends that same stale snapshot every 5 seconds forever. Believing
        // it means an hour-old percentage overwrites a fresh one — measured on
        // one tailnet as 11 sessions reporting 8 different values (7%-45%) for
        // the same window, with the boss HP swinging on whichever ticked last.
        //
        // Only a session whose activity just moved has headers worth trusting.
        // Movement is compared against the counter for THIS source only.
        let moved = self.fighters.get(&key).is_some_and(|f| {
            let prev = match o.source {
                Source::Statusline => f.act_sl,
                Source::Transcript => f.act_tr,
            };
            o.activity > prev + 1e-9
        });
        // Rate limits are only current if the STATUSLINE that carried them
        // belongs to a session that just did work.
        let worked = moved && o.source == Source::Statusline;

        // Boss HP is AUTHORITATIVE, never accumulated — but only from a source
        // that is actually current. merge_window then guards the rest: within a
        // window usage only accrues, so the max holds; a rolled window starts
        // fresh.
        if worked {
            if let Some(rl) = o.rate_limits.clone() {
                if rl.five_hour.is_some() || rl.seven_day.is_some() {
                    self.boss.five_hour = merge_window(self.boss.five_hour, rl.five_hour);
                    self.boss.seven_day = merge_window(self.boss.seven_day, rl.seven_day);
                    self.boss_seen_ms = Some(now_ms);
                }
            }
        }
        let name = sanitize(
            o.session_name
                .as_deref()
                .unwrap_or_else(|| &o.session_id[..o.session_id.len().min(6)]),
            14,
        );
        match self.fighters.get_mut(&key) {
            Some(f) => {
                // A rising activity counter is the evidence of work. Cost comes
                // from the statusline; transcript bytes come from sessions that
                // never render one.
                if moved {
                    f.last_move_ms = now_ms;
                }
                match o.source {
                    Source::Statusline => f.act_sl = o.activity,
                    Source::Transcript => f.act_tr = o.activity,
                }
                if o.cost_usd > 0.0 { f.cost = o.cost_usd; }
                f.name = name;
                f.class = class_of(&o.model_id).to_string();
                f.zone = o.zone;
                f.thinking = o.thinking;
                f.effort = o.effort;
                f.last_seen_ms = now_ms;
            }
            None => {
                self.fighters.insert(
                    key,
                    Fighter {
                        machine: o.machine,
                        session_id: o.session_id,
                        name,
                        class: class_of(&o.model_id).to_string(),
                        zone: o.zone,
                        thinking: o.thinking,
                        effort: o.effort,
                        cost: o.cost_usd,
                        act_sl: if o.source == Source::Statusline { o.activity } else { 0.0 },
                        act_tr: if o.source == Source::Transcript { o.activity } else { 0.0 },
                        last_seen_ms: now_ms,
                        last_move_ms: now_ms,
                    },
                );
            }
        }
    }

    pub fn evict(&mut self, now_ms: u64) {
        self.fighters
            .retain(|_, f| now_ms.saturating_sub(f.last_seen_ms) < FIGHTER_TTL_MS);
    }

    /// One header line plus one row per fighter. A line format, not JSON, so the
    /// ESP32 needs no parser — the same reason `puck/bridge.py` used one.
    ///
    ///   <hp5> <hp7> <secs5> <secs7> <week> <n>
    ///   <class> <atk|camp> <name>
    ///
    /// HP is `100 - used_percentage`; `-1` means the window is absent.
    ///
    /// Windows are sent as SECONDS REMAINING, not as the unix epoch the API
    /// reports. The panel has no real-time clock and no NTP, so an epoch would
    /// be unusable there; the hub already knows `now`, so it does the
    /// subtraction. `week` stays an epoch on purpose — it is an identity, not a
    /// duration, and the firmware rotates the beast when it changes.
    pub fn panel_line(&self, now_ms: u64) -> String {
        let now_s = (now_ms / 1000) as i64;
        // -1 means "unknown", which the panel renders as "--". A stale number
        // shown confidently is worse than an admitted gap.
        let fresh = self
            .boss_seen_ms
            .is_some_and(|t| now_ms.saturating_sub(t) < BOSS_TTL_MS);
        let hp = |w: Option<Window>| {
            if !fresh { return -1.0; }
            w.map(|w| 100.0 - w.used_percentage).unwrap_or(-1.0)
        };
        let at = |w: Option<Window>| {
            if !fresh { return -1; }
            w.map(|w| (w.resets_at - now_s).max(0)).unwrap_or(-1)
        };
        let week = if fresh { self.boss.seven_day.map(|w| w.resets_at).unwrap_or(-1) } else { -1 };
        let mut v: Vec<&Fighter> = self.fighters.values().collect();
        v.sort_by(|a, b| (&a.machine, &a.session_id).cmp(&(&b.machine, &b.session_id)));
        let mut out = format!(
            "{:.1} {:.1} {} {} {} {}\n",
            hp(self.boss.five_hour),
            hp(self.boss.seven_day),
            at(self.boss.five_hour),
            at(self.boss.seven_day),
            week,
            v.len()
        );
        for f in v {
            out.push_str(&format!(
                "{} {} {}\n",
                f.class,
                if f.attacking(now_ms) { "atk" } else { "camp" },
                f.name
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply as a session that just did work — the only kind whose rate limits
    /// are trusted. First sighting establishes the fighter; the second, with
    /// higher activity, is the one that proves the headers are current.
    fn apply_busy(w: &mut World, o: &Observation, t: u64) {
        let mut a = o.clone();
        a.activity = 1.0;
        a.source = Source::Statusline;
        w.apply(a, t);
        let mut b = o.clone();
        b.activity = 2.0;
        b.source = Source::Statusline;
        w.apply(b, t);
    }

    fn statusline() -> serde_json::Value {
        serde_json::json!({
            "session_id": "abcdef12-3456-7890-abcd-ef1234567890",
            "session_name": "night owl  <build>",
            "cwd": "/Users/someone/secret-project",
            "transcript_path": "/Users/someone/.claude/projects/x/y.jsonl",
            "model": { "id": "claude-opus-5", "display_name": "Opus 5" },
            "workspace": { "repo": { "owner": "acme", "name": "private-thing" } },
            "thinking": { "enabled": true },
            "effort": { "level": "high" },
            "cost": { "total_cost_usd": 1.25 },
            "rate_limits": {
                "five_hour": { "used_percentage": 41.0, "resets_at": 1757000000 },
                "seven_day": { "used_percentage": 63.0, "resets_at": 1757400000 }
            }
        })
    }

    #[test]
    fn parses_statusline_and_leaks_no_paths() {
        let o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        assert_eq!(o.class_check(), "opus");
        assert_eq!(o.cost_usd, 1.25);
        assert!(o.thinking);
        let json = serde_json::to_string(&o).unwrap();
        // The whole point of the whitelist: these must not reach the hub.
        assert!(!json.contains("secret-project"), "cwd leaked: {json}");
        assert!(!json.contains("private-thing"), "repo name leaked: {json}");
        assert!(!json.contains(".jsonl"), "transcript path leaked: {json}");
        assert_eq!(o.zone.len(), 8);
    }

    #[test]
    fn zone_is_stable_and_differs_per_repo() {
        assert_eq!(zone_id(Some("a/b"), None), zone_id(Some("a/b"), None));
        assert_ne!(zone_id(Some("a/b"), None), zone_id(Some("a/c"), None));
    }

    #[test]
    fn boss_hp_is_authoritative_across_windows_not_accumulated() {
        // Still never accumulated — but "authoritative" is per WINDOW. Inside
        // one window the max wins (idle sessions report stale percentages);
        // when the window rolls, the new value replaces outright even though it
        // is lower. This test used to assert that any later value wins, which
        // let a session idle for an hour drag the boss backwards.
        let mut w = World::default();
        let mut o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        apply_busy(&mut w, &o, 1000);
        assert_eq!(w.boss.five_hour.unwrap().used_percentage, 41.0);

        // Same window, lower reading: ignored.
        o.rate_limits = Some(RateLimits {
            five_hour: Some(Window { used_percentage: 12.0, resets_at: 1757000000 }),
            seven_day: None,
        });
        apply_busy(&mut w, &o, 2000);
        assert_eq!(w.boss.five_hour.unwrap().used_percentage, 41.0);

        // Window rolled: take it, even going backwards.
        o.rate_limits = Some(RateLimits {
            five_hour: Some(Window { used_percentage: 12.0, resets_at: 1757018000 }),
            seven_day: None,
        });
        apply_busy(&mut w, &o, 3000);
        assert_eq!(w.boss.five_hour.unwrap().used_percentage, 12.0);
    }

    #[test]
    fn transcript_bytes_do_not_masquerade_as_statusline_activity() {
        // Both sources describe the same session in different units. Sharing one
        // counter meant bytes always beat cost, so statusline updates never
        // registered as work and rate limits were never trusted.
        let mut w = World::default();
        let mut sl = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        sl.activity = 1.25;
        w.apply(sl.clone(), 1000);

        let mut tr = sl.clone();
        tr.source = Source::Transcript;
        tr.activity = 500_000.0;          // a large, unrelated unit
        tr.rate_limits = None;
        w.apply(tr, 1100);

        sl.activity = 1.50;               // cost moved: this IS work
        w.apply(sl, 1200);
        assert_eq!(
            w.boss.seven_day.unwrap().used_percentage, 63.0,
            "a real statusline update must still count after a transcript update"
        );
    }

    #[test]
    fn an_idle_session_cannot_report_rate_limits_at_all() {
        // The user's observation: standby sessions keep sending, and what they
        // send is stale. Re-sending an identical payload must change nothing.
        let mut w = World::default();
        let o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        w.apply(o.clone(), 1000);        // first sighting: no prior activity
        assert!(w.boss.seven_day.is_none(), "a first sighting proves no work");
        w.apply(o.clone(), 2000);        // identical -> still idle
        assert!(w.boss.seven_day.is_none(), "an idle re-send must not count");

        let mut busy = o.clone();
        busy.activity += 1.0;            // this session actually did something
        w.apply(busy, 3000);
        assert_eq!(w.boss.seven_day.unwrap().used_percentage, 63.0);
    }

    #[test]
    fn stale_sessions_cannot_drag_the_boss_backwards() {
        // 11 real sessions on one tailnet reported 8 different percentages for
        // the same window. Usage only accrues, so the max is the estimate.
        let mut w = World::default();
        let mut o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        for (i, pct) in [45.0f32, 7.0, 38.0, 20.0].iter().enumerate() {
            o.session_id = format!("s{i}");
            o.rate_limits = Some(RateLimits {
                five_hour: None,
                seven_day: Some(Window { used_percentage: *pct, resets_at: 1788696000 }),
            });
            o.activity = 1.0;
            w.apply(o.clone(), 1000 + i as u64);   // first sighting
            o.activity = 2.0;
            w.apply(o.clone(), 1100 + i as u64);   // now it has worked
        }
        assert_eq!(w.boss.seven_day.unwrap().used_percentage, 45.0,
                   "must hold the highest seen, not the last to arrive");
    }

    #[test]
    fn a_rolled_window_starts_fresh_instead_of_inheriting_the_max() {
        let mut w = World::default();
        let mut o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        o.rate_limits = Some(RateLimits {
            five_hour: None,
            seven_day: Some(Window { used_percentage: 92.0, resets_at: 100 }),
        });
        apply_busy(&mut w, &o, 1000);
        o.rate_limits = Some(RateLimits {
            five_hour: None,
            seven_day: Some(Window { used_percentage: 3.0, resets_at: 200 }),
        });
        apply_busy(&mut w, &o, 2000);
        assert_eq!(w.boss.seven_day.unwrap().used_percentage, 3.0);
    }

    #[test]
    fn an_observation_without_windows_does_not_clear_the_boss() {
        let mut w = World::default();
        let mut o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        apply_busy(&mut w, &o, 1000);
        o.rate_limits = None; // API-key session, or before the first response
        o.session_id = "other".into();
        apply_busy(&mut w, &o, 1100);
        assert_eq!(w.boss.five_hour.unwrap().used_percentage, 41.0);
    }

    #[test]
    fn cost_movement_is_what_makes_a_fighter_attack() {
        let mut w = World::default();
        let mut o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        w.apply(o.clone(), 0);
        let key = w.fighters.keys().next().unwrap().clone();
        // Cost flat for 20s -> camping.
        w.apply(o.clone(), 20_000);
        assert!(!w.fighters[&key].attacking(20_000));
        // Cost moves -> back in combat.
        o.cost_usd = 2.0;
        o.activity = 2.0;
        w.apply(o, 21_000);
        assert!(w.fighters[&key].attacking(21_000));
    }

    #[test]
    fn silent_fighters_are_evicted() {
        let mut w = World::default();
        let o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        w.apply(o, 0);
        w.evict(FIGHTER_TTL_MS - 1);
        assert_eq!(w.fighters.len(), 1);
        w.evict(FIGHTER_TTL_MS + 1);
        assert!(w.fighters.is_empty());
    }

    #[test]
    fn two_machines_one_boss_party_is_the_union() {
        let mut w = World::default();
        let a = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        let b = Observation::from_statusline(&statusline(), "desk", 1).unwrap();
        apply_busy(&mut w, &a, 100);
        apply_busy(&mut w, &b, 100);
        assert_eq!(w.fighters.len(), 2, "same session_id on two machines is two fighters");
        assert_eq!(w.boss.five_hour.unwrap().used_percentage, 41.0);
    }

    #[test]
    fn panel_line_is_ascii_and_space_safe() {
        let mut w = World::default();
        let o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        apply_busy(&mut w, &o, 0);
        let line = w.panel_line(0);
        let mut it = line.lines();
        let head: Vec<&str> = it.next().unwrap().split(' ').collect();
        assert_eq!(head.len(), 6);
        assert_eq!(head[0], "59.0"); // 100 - 41
        assert_eq!(head[1], "37.0"); // 100 - 63
        assert_eq!(head[4], "1757400000", "week is an identity, so it stays an epoch");
        assert_eq!(head[5], "1");
        let row: Vec<&str> = it.next().unwrap().split(' ').collect();
        assert_eq!(row.len(), 3, "a name with a space would break the row: {row:?}");
        assert_eq!(row[0], "opus");
        assert_eq!(row[1], "atk");
        // The device font is 0x20-0x7E; anything else renders as a silent blank.
        assert!(
            line.chars().all(|c| c == '\n' || (' '..='~').contains(&c)),
            "non-renderable byte reached the panel: {line:?}"
        );
    }

    #[test]
    fn windows_are_sent_as_seconds_remaining_not_epochs() {
        // The panel has no clock. An epoch would be unusable there.
        let mut w = World::default();
        let o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        let now_ms = 1_756_999_000_000u64; // 1000s before five_hour.resets_at
        apply_busy(&mut w, &o, now_ms);
        let head = w.panel_line(now_ms).lines().next().unwrap().to_string();
        let f: Vec<&str> = head.split(' ').collect();
        assert_eq!(f[2], "1000", "secs5 should count down: {head}");
        assert_eq!(f[4], "1757400000", "week stays an epoch: {head}");
    }

    #[test]
    fn a_reset_in_the_past_clamps_to_zero_not_negative() {
        let mut w = World::default();
        let o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        apply_busy(&mut w, &o, 9_000_000_000_000);
        let head = w.panel_line(9_000_000_000_000).lines().next().unwrap().to_string();
        assert_eq!(head.split(' ').nth(2).unwrap(), "0", "{head}");
    }

    #[test]
    fn stale_rate_limits_report_unknown_not_a_confident_wrong_number() {
        let mut w = World::default();
        let o = Observation::from_statusline(&statusline(), "mbp", 1).unwrap();
        apply_busy(&mut w, &o, 1000);
        assert!(w.panel_line(1000).starts_with("59.0"), "fresh should report");
        let head = w.panel_line(1000 + BOSS_TTL_MS + 1).lines().next().unwrap().to_string();
        assert!(head.starts_with("-1.0 -1.0 -1 -1 -1"), "stale must be unknown: {head}");
    }

    #[test]
    fn absent_windows_report_minus_one_not_zero() {
        // 0 would mean "budget spent" — the exact opposite of "unknown".
        let w = World::default();
        let head = w.panel_line(0).lines().next().unwrap().to_string();
        assert!(head.starts_with("-1.0 -1.0 -1 -1 -1 0"), "{head}");
    }

    #[test]
    fn sanitize_strips_what_the_panel_cannot_draw() {
        assert_eq!(sanitize("hello world", 32), "HELLO_WORLD");
        // The dropped char leaves its neighbouring spaces behind, so runs of
        // "_" are expected. Harmless: the invariant that matters is no spaces
        // and nothing outside 0x21-0x7E.
        assert_eq!(sanitize("caf\u{e9} \u{2014} x", 32), "CAF__X");
        assert_eq!(sanitize("", 32), "?");
        assert_eq!(sanitize("abcdefghij", 4), "ABCD");
    }

    impl Observation {
        fn class_check(&self) -> &'static str {
            class_of(&self.model_id)
        }
    }
}
