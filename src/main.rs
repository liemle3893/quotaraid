//! quotaraid — Claude Code usage rendered as a boss fight.
//!
//! One crate, two subcommands, sharing `protocol` so the wire format cannot
//! drift between them:
//!
//!   agent  — listens for statusline payloads on loopback UDP, whitelists them,
//!            forwards to a hub over WebSocket.
//!   hub    — owns the world; serves browsers (/view), the ESP32 (/panel) and
//!            agents (/ingest).
//!
//! See DESIGN.md. The load-bearing decision is that boss HP is authoritative
//! rather than accumulated, which is why nothing here buffers or replays.

mod agent;
mod hub;
mod protocol;
mod transcripts;
mod usage;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "quotaraid", version, about = "Claude usage as a boss fight")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Collect this machine's statusline payloads and forward them to a hub.
    Agent {
        /// Full hub URL. ws:// on a tailnet; wss:// works unchanged if the hub
        /// ever moves to the public internet, so hosting stays a config change.
        #[arg(
            long,
            env = "QUOTARAID_HUB",
            default_value = "ws://127.0.0.1:7777/ingest"
        )]
        hub: String,
        #[arg(long, env = "QUOTARAID_TOKEN", default_value = "")]
        token: String,
        /// Defaults to the hostname.
        #[arg(long, env = "QUOTARAID_MACHINE")]
        machine: Option<String>,
        /// Loopback only — this accepts unauthenticated datagrams by design.
        #[arg(long, default_value = "127.0.0.1:7778")]
        listen: String,
        /// Also poll Claude Code's own usage endpoint for quota.
        ///
        /// Off by default: the endpoint is UNDOCUMENTED and this reads your
        /// OAuth token. Worth it because the statusline does not reliably carry
        /// the 5-hour window — measured at 0 of 92 payloads while the endpoint
        /// reported it at 2%.
        #[arg(long)]
        oauth_usage: bool,
    },
    /// Own the world state and serve it.
    Hub {
        #[arg(long, default_value = "0.0.0.0:7777")]
        listen: String,
        /// Required on /ingest, which writes world state.
        #[arg(long, env = "QUOTARAID_TOKEN", default_value = "")]
        token: String,
        /// Also gate /view. Off by default: browsers cannot set request headers
        /// on a WebSocket, so gating it means a token in the query string and
        /// therefore in access logs. A read-only view on a private tailnet is
        /// not worth that trade.
        #[arg(long)]
        view_token: bool,
    },
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().split('.').next().unwrap_or("host").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "host".into())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Agent {
            hub,
            token,
            machine,
            listen,
            oauth_usage,
        } => {
            agent::run(
                &hub,
                &token,
                &machine.unwrap_or_else(hostname),
                &listen,
                oauth_usage,
            )
            .await
        }
        Cmd::Hub {
            listen,
            token,
            view_token,
        } => hub::run(&listen, &token, view_token).await,
    }
}
