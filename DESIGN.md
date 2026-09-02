# bossfight — design

**Date:** 2026-09-03
**Status:** approved design, not yet implemented

Claude Code token budget rendered as a stickman boss fight. Live sessions are
party members; the boss's HP is the remaining 5-hour / 7-day rate-limit budget.
Burning tokens damages the boss. Hitting the cap is a party wipe.

## Motivation

`/usage` shows the rate-limit meter only on demand, inside one session. There is
no ambient, at-a-glance signal of how much budget is left across every machine
and session at once. A game screen makes budget burn continuously visible, and
makes the failure mode (getting rate limited) legible before it happens.

## Key finding: one ingest source

Everything the game renders is already in the Claude Code statusline payload,
which the harness invokes **once per session every 5 seconds**:

| Game concept        | statusline field                                    |
|---------------------|-----------------------------------------------------|
| Boss HP             | `rate_limits.five_hour.used_percentage`, `.seven_day`|
| Enrage timer        | `rate_limits.*.resets_at`                            |
| Party member        | `session_id`, `session_name`                         |
| Class               | `model.id`, `agent.name`, `effort.level`             |
| Damage dealt        | `cost.total_cost_usd` (cumulative per session)       |
| Casting animation   | `thinking.enabled`                                   |
| Zone                | `workspace.repo`, `cwd`, `git_worktree`              |

Consequences: **no JSONL tailing, no OpenTelemetry, no ccusage dependency.**

Rejected alternatives:

- **JSONL tail** (`~/.claude/projects/**/*.jsonl`, the ccusage approach) — has
  full token detail but only *estimates* the rate-limit windows, and means
  polling a 1.1 GB tree. Cannot produce the real boss HP.
- **OpenTelemetry** (`CLAUDE_CODE_ENABLE_TELEMETRY=1`) — exports
  `claude_code.token.usage`, `.subagent.spawn`, `.tool.execution` for every
  session including background ones, but does **not** export the rate-limit
  windows. Would still need a second source for boss HP.
- **`/api/oauth/usage`** — the endpoint `/usage` itself calls. Real numbers with
  no session running, but undocumented, unsupported, may break on any release,
  and requires reading the OAuth token from the macOS keychain.

Known limitation of the chosen source: the statusline only ticks while an
interactive session is on screen. Background (`--bg`) sessions do not refresh
boss HP. Accepted — any session coming to the foreground corrects it within 5 s.

## Topology

```
machine A
  N sessions ─ statusline ─udp:7778─▶ bossfight agent ─┐
                                                        │ ws + Bearer
machine B                                               ▼
  N sessions ─ statusline ─udp:7778─▶ bossfight agent ─▶ bossfight hub
                                                        │  owns world state
                                                        ├─ ws /view ─▶ browsers
                                                        └─ token ─▶ identity
```

One crate, two subcommands (`bossfight agent`, `bossfight hub`) sharing
`protocol.rs` so the wire format cannot drift. Hub on localhost is the
degenerate case, used for development.

Scope decision: **one Anthropic account, many machines, one boss.** Rate limits
are account-wide, so every agent reports the same numbers and the hub needs no
aggregation logic — latest observation wins. A future multi-account raid mode is
`HashMap<PlayerId, Boss>` where there is currently one key.

## Ingest

Added to `~/.claude/statusline.sh`:

```bash
printf '%s' "$IN" > /dev/udp/127.0.0.1/7778 2>/dev/null || :
```

Pure bash — no `nc`, no subprocess fork, non-blocking, silently no-ops when the
agent is down. This matters: a statusline hook that blocks or returns non-zero
degrades the interactive TUI for every session on the machine. UDP datagrams are
self-framing, so one JSON payload per packet needs no length prefix. The agent
binds `127.0.0.1` only.

## Protocol

Agent → hub. A whitelist, because only rendered fields are sent:

```rust
struct Observation {
    machine:      String,           // hostname, or --machine label
    session_id:   String,           // already a UUID
    session_name: Option<String>,   // from /rename
    model_id:     String,
    agent_name:   Option<String>,
    effort:       Option<String>,
    thinking:     bool,
    zone:         String,           // see Zone below
    cost_usd:     f64,              // cumulative, per session
    rate_limits:  Option<RateLimits>,
    ts:           i64,
}
```

**Zone** is the first 8 hex chars of `sha256(repo owner/name, else cwd)` by
default, so battlegrounds are distinguishable without being readable. A
`--zone-label <hash>=<name>` flag maps a hash to a display name locally.

`cwd`, `transcript_path` and raw repo owner/name are excluded — nothing draws
them, so the minimal payload and the non-leaking payload are the same struct.

Hub → browser: full `World` snapshot at 10 Hz. The client interpolates, so 5 s
ingest still renders as continuous combat.

## State

```rust
struct Boss { hp5: f32, hp7: f32, resets5: i64, resets7: i64 }  // hp = 100 - used_pct
struct Fighter {
    id: String, machine: String, name: String, class: Class,
    cost: f64, zone: String, thinking: bool,
    effort: Effort, last_seen: Instant,
}
```

Boss HP is **authoritative, never accumulated** — it is whatever the newest
observation says. The hub can restart, drop packets, or miss a machine entirely
and still be exactly correct on the next tick. Damage per tick is the HP *drop*,
split across fighters whose `cost` moved.

This is the design's load-bearing decision. It is why reconnect needs no queue,
no replay, and no at-least-once delivery.

Session key is `(machine, session_id)`.

## Fighter states

| State     | Condition                          | Render                       |
|-----------|------------------------------------|------------------------------|
| `combat`  | ticked < 15 s ago and `cost` moved | swings, damage numbers float |
| `camping` | alive, `cost` flat                 | sits at a fire               |
| `gone`    | no tick in 60 s                    | walks off screen, evicted    |

**Boss death is a party wipe.** HP reaches 0, screen goes red, `RATE LIMITED`,
stickmen lie down, respawn countdown runs to `resets_at`. Killing the boss is a
loss, not a win — this is deliberate and needs no extra machinery.

## Auth and transport

Hub runs on a Tailscale/LAN address. Plain `ws://` with a bearer token; no TLS
termination, no certificates, no renewal, because the network is already
authenticated.

- `/ingest` — requires `Authorization: Bearer <token>`. It writes world state.
- `/view` — open by default; `--view-token` gates it.

The asymmetry is a browser constraint, not laziness: **browser WebSockets cannot
set request headers**, so a gated `/view` must accept `?token=`, which leaks
tokens into access logs. Leaving a read-only view open on a private tailnet
avoids that entirely.

Config: `BOSSFIGHT_HUB` (full URL, `ws://` or `wss://`, so public hosting is a
config change rather than a rewrite), `BOSSFIGHT_TOKEN`, `BOSSFIGHT_MACHINE`.

## Failure behaviour

- Agent cannot reach hub → drop observations, reconnect with exponential
  backoff. No buffering.
- Hub restarts → world rebuilds from the next tick of each agent.
- Agent down → statusline unaffected (the `|| :` above).
- Session silent 60 s → evicted from the party.

## Frontend

One `index.html`, canvas 2D, no framework, no build step, embedded in the hub
binary. Stickmen are five lines and a circle.

Classes: four from `model_id` (Opus tank, Sonnet rogue, Haiku archer, Fable
mage); sessions with an `agent_name` render as summons. Cosmetic.

## Success criteria

1. `printf '<sample>' > /dev/udp/127.0.0.1/7778` → HP bar moves in browser < 1 s
2. Two live sessions → two named stickmen
3. Kill one session → its stickman leaves within 60 s
4. **Agent or hub down → statusline renders normally, no lag, no error**
5. Hub restarted mid-fight → HP snaps back to correct within 5 s
6. Two machines feeding one hub → one boss, party is the union
7. Bad/absent token on `/ingest` → connection refused, world unchanged

Criterion 4 is the one that matters most: this project is not allowed to degrade
the terminal it observes.

## Out of scope

History and replay, per-player rosters, multi-account raid mode, TLS, sound,
persistence across hub restarts.

## Estimated size

~250 lines agent, ~350 lines hub, ~150 lines JS, one `index.html`, 6 crates
(`tokio`, `axum`, `tokio-tungstenite`, `serde`/`serde_json`, `clap`, `anyhow`).
