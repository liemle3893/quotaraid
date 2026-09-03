//! The collector. Runs on every machine that has Claude Code sessions.
//!
//! Ingest is loopback UDP because the producer is a statusline hook, and a
//! statusline hook that blocks or errors degrades the terminal for every
//! session on the machine. One line of bash:
//!
//!   printf '%s' "${IN//$'\n'/ }" > /dev/udp/127.0.0.1/7778 2>/dev/null || :
//!
//! No `nc`, no subprocess fork, non-blocking, silently a no-op when this agent
//! is not running. UDP datagrams are self-framing, so one JSON payload per
//! packet needs no length prefix.
//!
//! The `${IN//...}` newline strip is not cosmetic. Bash's /dev/udp redirection
//! writes ONE DATAGRAM PER LINE, so a payload containing a newline arrives as
//! several fragments, each of which is invalid JSON and is dropped here in
//! silence. Claude Code sends compact single-line JSON today; this makes the
//! hook immune to that ever changing. Measured 2026-09-03 — multi-line
//! payloads vanished entirely until the split was found.

use crate::protocol::Observation;
use anyhow::Result;
use futures_util::SinkExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

/// Small on purpose. When the hub is unreachable the channel fills and
/// `try_send` drops — which is correct, not a compromise: boss HP is
/// authoritative, so the next 5s tick restates the truth. A queue here would
/// replay stale percentages over fresh ones.
const QUEUE: usize = 8;

pub async fn run(
    hub: &str,
    token: &str,
    machine: &str,
    listen: &str,
    oauth_usage: bool,
) -> Result<()> {
    let sock = UdpSocket::bind(listen).await?;
    println!("[agent] {machine}: udp {listen} -> {hub}");

    let (tx, rx) = mpsc::channel::<String>(QUEUE);
    tokio::spawn(forward(hub.to_string(), token.to_string(), rx));

    // Detached sessions never render a statusline, so poll transcripts too.
    {
        let tx = tx.clone();
        let machine = machine.to_string();
        tokio::spawn(async move {
            loop {
                for obs in crate::transcripts::scan(&machine) {
                    if let Ok(j) = serde_json::to_string(&obs) {
                        let _ = tx.try_send(j);
                    }
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }

    if oauth_usage {
        let tx = tx.clone();
        let machine = machine.to_string();
        tokio::spawn(async move {
            loop {
                match tokio::task::spawn_blocking(crate::usage::fetch).await {
                    Ok(Ok(rl)) => {
                        let ts = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        // A quota reading, not a fighter: it carries no session
                        // of its own, and the hub keys fighters by session id.
                        let obs = Observation {
                            machine: machine.clone(),
                            session_id: "oauth-usage".into(),
                            session_name: None,
                            model_id: String::new(),
                            agent_name: None,
                            effort: None,
                            thinking: false,
                            zone: String::new(),
                            cost_usd: 0.0,
                            activity: 0.0,
                            source: crate::protocol::Source::Usage,
                            busy: Some(false),
                            rate_limits: Some(rl),
                            ts,
                        };
                        if let Ok(j) = serde_json::to_string(&obs) {
                            let _ = tx.try_send(j);
                        }
                    }
                    Ok(Err(e)) => eprintln!("[agent] usage endpoint: {e}"),
                    Err(e) => eprintln!("[agent] usage task: {e}"),
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    let mut buf = vec![0u8; 64 * 1024];
    let mut dropped: u64 = 0;
    loop {
        let (n, _from) = sock.recv_from(&mut buf).await?;
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) else {
            continue;
        };
        let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        let Some(obs) = Observation::from_statusline(&v, machine, ts) else {
            continue;
        };
        if let Ok(json) = serde_json::to_string(&obs) {
            if tx.try_send(json).is_err() {
                dropped += 1;
                if dropped % 50 == 1 {
                    eprintln!("[agent] hub unreachable, dropped {dropped} (this is fine)");
                }
            }
        }
    }
}

async fn forward(hub: String, token: String, mut rx: mpsc::Receiver<String>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        match connect(&hub, &token).await {
            Ok(mut ws) => {
                println!("[agent] connected to {hub}");
                backoff = Duration::from_secs(1);
                while let Some(msg) = rx.recv().await {
                    if ws.send(Message::Text(msg)).await.is_err() {
                        eprintln!("[agent] hub went away, reconnecting");
                        break;
                    }
                }
            }
            Err(e) => {
                eprintln!("[agent] connect failed ({e}), retry in {:?}", backoff);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

async fn connect(
    hub: &str,
    token: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let mut req = hub.into_client_request()?;
    if !token.is_empty() {
        req.headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse()?);
    }
    let (ws, _) = tokio_tungstenite::connect_async(req).await?;
    Ok(ws)
}
