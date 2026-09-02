# bossfight

Claude Code usage rendered as a boss fight. The boss is your **unspent**
budget — agents working are attacking it, and killing it means the quota got
used. See `DESIGN.md`.

```
machine A
  N sessions ─ statusline ─udp:7778─▶ bossfight agent ─┐
                                                        │ ws + Bearer
machine B                                               ▼
  N sessions ─ statusline ─udp:7778─▶ bossfight agent ─▶ bossfight hub
                                                        ├─ /       browser
                                                        ├─ /panel  ESP32
                                                        └─ /ingest agents
```

## Run

```sh
cargo build --release

# on the machine that shows the fight
BOSSFIGHT_TOKEN=$(openssl rand -hex 16) \
  ./target/release/bossfight hub --listen 0.0.0.0:7777

# on every machine with Claude Code sessions
BOSSFIGHT_TOKEN=<same> \
  ./target/release/bossfight agent --hub ws://<hub-host>:7777/ingest
```

Then add one line to `~/.claude/statusline.sh`, anywhere after `IN=$(cat)`:

```bash
printf '%s' "${IN//$'\n'/ }" > /dev/udp/127.0.0.1/7778 2>/dev/null || :
```

No `nc`, no fork, non-blocking, and a silent no-op when the agent is not
running — a statusline hook that blocks or errors degrades every terminal on
the machine, so this one cannot do either.

**The newline strip matters.** Bash's `/dev/udp` sends one datagram per line,
so a payload with a newline arrives as fragments that are each invalid JSON and
are each dropped without a word.

## Endpoints

| path      | for      | auth                          |
|-----------|----------|-------------------------------|
| `/`       | browser  | open                          |
| `/view`   | browser  | open (`--view-token` to gate) |
| `/panel`  | ESP32    | open                          |
| `/ingest` | agents   | `Authorization: Bearer`       |

`/view` is open by default because browsers cannot set headers on a WebSocket,
so gating it means a token in the query string and therefore in access logs.

## Test

```sh
cargo test    # 10 unit tests: parsing, privacy whitelist, authoritative HP,
              # eviction, panel format, sanitisation
./e2e.sh      # 6 end-to-end: real processes, real UDP, real WebSockets
```
