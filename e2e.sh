#!/usr/bin/env bash
# End-to-end against DESIGN.md's success criteria. bash, not zsh: /dev/udp.
set -u
cd ~/Documents/git/Personal/quotaraid
B=./target/release/quotaraid
TOK=s3cret
HUBP=7799; UDPA=7798; UDPB=7797
# The transcript watcher scans $HOME/.claude, so on a real machine every live
# session joined the fixture and the fighter counts were whatever happened to be
# running. Give the agents an empty HOME so this suite tests only what it sends.
ISO=$(mktemp -d)
trap 'rm -rf "$ISO"' EXIT
pass=0; fail=0
ok(){ echo "  PASS  $1"; pass=$((pass+1)); }
no(){ echo "  FAIL  $1"; fail=$((fail+1)); }

$B hub --listen 127.0.0.1:$HUBP --token $TOK >/tmp/bf_hub.log 2>&1 & HUB=$!
for i in $(seq 1 100); do curl -sf "http://127.0.0.1:$HUBP/panel" >/dev/null 2>&1 && break; done

HOME="$ISO" $B agent --hub ws://127.0.0.1:$HUBP/ingest --token $TOK --machine mbp  --listen 127.0.0.1:$UDPA >/tmp/bf_a.log 2>&1 & AG1=$!
HOME="$ISO" $B agent --hub ws://127.0.0.1:$HUBP/ingest --token $TOK --machine desk --listen 127.0.0.1:$UDPB >/tmp/bf_b.log 2>&1 & AG2=$!
for i in $(seq 1 200); do grep -q connected /tmp/bf_a.log && grep -q connected /tmp/bf_b.log && break; done

mk(){ # $1 session_id  $2 pct  $3 cost  $4 name
# SINGLE LINE on purpose: bash's /dev/udp sends one datagram per line, which is
# what the real statusline hook does too (it strips newlines before writing).
printf '%s' '{"session_id":"'"$1"'","session_name":"'"$4"'","cwd":"/Users/x/secret-proj","transcript_path":"/Users/x/.claude/projects/p/t.jsonl","model":{"id":"claude-opus-5"},"workspace":{"repo":{"owner":"acme","name":"topsecret"}},"thinking":{"enabled":false},"cost":{"total_cost_usd":'"$3"'},"rate_limits":{"five_hour":{"used_percentage":'"$2"',"resets_at":1757000000},"seven_day":{"used_percentage":63.0,"resets_at":1757400000}}}'
}
# Sends TWICE with a rising cost. The hub only believes rate limits from a
# session that just did work — a first sighting proves nothing, and an idle
# re-send must change nothing. One datagram therefore registers a fighter but
# no quota, which is correct behaviour and used to fail this script.
send(){
  mk "$2" "$3" "$4" "$5" > /dev/udp/127.0.0.1/$1
  mk "$2" "$3" "$(awk -v c="$4" 'BEGIN{printf "%.2f", c+0.5}')" "$5" > /dev/udp/127.0.0.1/$1
}

# --- 1: a datagram moves boss HP
send $UDPA sess-aaaa 41.0 1.0 "night owl"
for i in $(seq 1 100); do curl -s "http://127.0.0.1:$HUBP/panel" | head -1 | grep -q "^59.0" && break; done
H=$(curl -s "http://127.0.0.1:$HUBP/panel" | head -1)
# hp5 hp7 secs5 secs7 week n — secs are countdowns, so only hp and n are fixed.
read -r h5 h7 _s5 _s7 _wk n <<<"$H"
[[ "$h5" == 59.0 && "$h7" == 37.0 && "$n" == 1 ]] \
  && ok "udp -> boss hp 59.0/37.0, 1 fighter" || no "header was: $H"

# --- 2 + 6: second machine -> union of fighters, still ONE boss
send $UDPB sess-bbbb 41.0 2.0 "desk run"
for i in $(seq 1 100); do [[ "$(curl -s http://127.0.0.1:$HUBP/panel | head -1 | awk '{print $6}')" == 2 ]] && break; done
N=$(curl -s "http://127.0.0.1:$HUBP/panel" | head -1 | awk '{print $6}')
[[ "$N" == 2 ]] && ok "two machines -> party of 2, one boss" || no "expected 2 fighters, got $N"

# --- privacy: nothing identifying may reach the hub
P=$(curl -s "http://127.0.0.1:$HUBP/panel")
if grep -qE 'secret-proj|topsecret|jsonl' <<<"$P"; then no "PRIVATE DATA LEAKED: $P"; else ok "no cwd/repo/transcript in panel output"; fi
grep -qE 'NIGHT_OWL' <<<"$P" && ok "session name sanitised (space -> _)" || no "name not sanitised: $P"

# --- 7: bad token on /ingest is refused
C=$(curl -s -o /dev/null -w '%{http_code}' -H 'Authorization: Bearer wrong' \
      -H 'Connection: Upgrade' -H 'Upgrade: websocket' -H 'Sec-WebSocket-Version: 13' \
      -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' "http://127.0.0.1:$HUBP/ingest")
[[ "$C" == 401 ]] && ok "/ingest rejects a bad token (401)" || no "/ingest returned $C, expected 401"

# --- 5: hub restart -> world rebuilds from the next tick
kill $HUB 2>/dev/null; wait $HUB 2>/dev/null
$B hub --listen 127.0.0.1:$HUBP --token $TOK >>/tmp/bf_hub.log 2>&1 & HUB=$!
for i in $(seq 1 200); do curl -sf "http://127.0.0.1:$HUBP/panel" >/dev/null 2>&1 && break; done
# The statusline ticks every 5s, so the guarantee is convergence on a LATER
# tick, not delivery of the first one. The agent only learns the socket died
# when it tries to write, so that write is lost and the reconnect follows it —
# by design: DESIGN.md chose dropping over a queue, because a queue would
# replay a stale percentage over a fresh one.
for t in $(seq 1 20); do
  send $UDPA sess-aaaa 77.0 3.0 "night owl" >/dev/null
  for i in $(seq 1 40); do curl -s "http://127.0.0.1:$HUBP/panel" | head -1 | grep -q '^23.0' && break 2; done
done
H2=$(curl -s "http://127.0.0.1:$HUBP/panel" | head -1)
[[ "$H2" == 23.0* ]] && ok "hub restart -> converges on a later tick ($H2)" || no "after restart: $H2"

echo; echo "  $pass passed, $fail failed"
kill $HUB $AG1 $AG2 2>/dev/null
exit $fail
