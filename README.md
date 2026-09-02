# quotaraid

Your Claude Code usage quota, rendered as a boss fight.

The boss is your **unspent** budget. Agents doing work attack it; killing it
means the quota got used. Idle agents camp. There is no losing state — you paid
for the quota either way, so the unused part is the thing in your way.

Runs as a hub plus one agent per machine. Any browser can watch, and so can a
320x240 ESP32 panel on your desk.

```
machine A
  N sessions ─ statusline ─udp:7778─▶ quotaraid agent ─┐
                                                        │ ws + Bearer
machine B                                               ▼
  N sessions ─ statusline ─udp:7778─▶ quotaraid agent ─▶ quotaraid hub
                                                        ├─ /       browser
                                                        ├─ /panel  embedded
                                                        └─ /ingest agents
```

## Install

Download a binary for your platform from
[Releases](https://github.com/liemle3893/quotaraid/releases). Linux builds are
static musl, so they run on any distro with no glibc to match.

```sh
# Linux x86_64 (also: linux-aarch64, macos-aarch64, macos-x86_64)
curl -fsSL -o quotaraid \
  https://github.com/liemle3893/quotaraid/releases/latest/download/quotaraid-linux-x86_64
chmod +x quotaraid && sudo mv quotaraid /usr/local/bin/
```

Or build it: `cargo install --git https://github.com/liemle3893/quotaraid`

## Run

```sh
export QUOTARAID_TOKEN=$(openssl rand -hex 16)

# once, on whichever machine shows the fight
quotaraid hub --listen 0.0.0.0:7777

# on every machine that runs Claude Code
quotaraid agent --hub ws://<hub-host>:7777/ingest
```

Then add one line to `~/.claude/statusline.sh`, anywhere after `IN=$(cat)`:

```bash
printf '%s' "${IN//$'\n'/ }" > /dev/udp/127.0.0.1/7778 2>/dev/null || :
```

No `nc`, no fork, non-blocking, and a silent no-op when the agent is not
running — a statusline hook that blocks or errors degrades every terminal on
the machine, so this one cannot do either.

**The newline strip is load-bearing.** Bash's `/dev/udp` sends **one datagram
per line**, so a payload containing a newline arrives as fragments that are each
invalid JSON and each dropped without a word.

Don't have a `statusline.sh`? You still get the party — the agent watches
transcripts too. You just won't get quota numbers, which only a statusline can
report.

## Endpoints

| path | for | auth |
|---|---|---|
| `/` | browser | open |
| `/view` | browser (WebSocket) | open, `--view-token` to gate |
| `/panel` | embedded devices, plain text | open |
| `/ingest` | agents | `Authorization: Bearer` |

`/view` is open by default because browsers cannot set headers on a WebSocket,
so gating it means a token in the query string and therefore in access logs.

## What it reads, and what it never sends

The agent whitelists: machine, session id and name, model, effort, zone hash,
cost, and rate limits. It never sends `cwd`, `transcript_path`, or raw repo
owner/name — nothing renders them, so the minimal payload and the non-leaking
payload are the same struct. The `zone` is a truncated SHA-256 so battlegrounds
stay distinguishable without being readable.

## Notes from production

`DESIGN.md` records what the Claude Code statusline actually provides, which is
not what it looks like:

- **Detached sessions never render a statusline**, so the party is empty exactly
  when agents are working headless. Transcripts are the signal that always
  exists.
- **Idle sessions re-send stale rate limits forever.** Measured: 11 sessions
  reporting 8 different values for one window. Only a session that just did work
  is believed, and within a window the maximum wins.
- **"Working" is not "spending."** A session blocked on a long tool call spends
  nothing. `prompt_id` and `cost.total_duration_ms` both look like they answer
  this and neither does.

## Test

```sh
cargo test    # unit: parsing, privacy whitelist, HP rules, eviction, sanitising
./e2e.sh      # end-to-end: real processes, real UDP, real WebSockets
```

## Licence

MIT
