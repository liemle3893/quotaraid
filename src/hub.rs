//! The hub. Owns the world; every other component is a client of it.
//!
//! Scope decision from DESIGN.md: ONE Anthropic account, many machines, one
//! boss. Rate limits are account-wide, so every agent reports the same numbers
//! and there is no aggregation logic anywhere — latest observation wins. A
//! future multi-account raid mode is a `HashMap<PlayerId, World>` where there
//! is currently one key.

use crate::protocol::{Observation, World};
use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

const TICK_MS: u64 = 100; // 10 Hz to browsers; the client interpolates

#[derive(Clone)]
struct App {
    world: Arc<Mutex<World>>,
    tx: broadcast::Sender<String>,
    token: String,
    view_token: bool,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn bearer_ok(token: &str, headers: &HeaderMap) -> bool {
    if token.is_empty() {
        return true;
    }
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v == token)
        .unwrap_or(false)
}

pub async fn run(listen: &str, token: &str, view_token: bool) -> Result<()> {
    let (tx, _) = broadcast::channel(16);
    let app = App {
        world: Arc::new(Mutex::new(World::default())),
        tx: tx.clone(),
        token: token.to_string(),
        view_token,
    };

    // Evict and publish on one clock, so a fighter cannot linger in a snapshot
    // after it has been evicted from the world.
    {
        let app = app.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_millis(TICK_MS));
            loop {
                iv.tick().await;
                let snapshot = {
                    let mut w = app.world.lock().unwrap();
                    w.evict(now_ms());
                    serde_json::to_string(&*w).unwrap_or_default()
                };
                let _ = app.tx.send(snapshot);
            }
        });
    }

    let router = Router::new()
        .route("/", get(index))
        .route("/ingest", get(ingest))
        .route("/view", get(view))
        .route("/panel", get(panel))
        .with_state(app);

    let l = tokio::net::TcpListener::bind(listen).await?;
    println!("[hub] listening on {listen}");
    println!("[hub]   /        browser view");
    println!("[hub]   /panel   ESP32 line format");
    println!(
        "[hub]   /ingest  agents{}",
        if token.is_empty() {
            " (NO TOKEN SET — open)"
        } else {
            ""
        }
    );
    axum::serve(l, router).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

/// The ESP32's endpoint. Plain text, one header line plus one row per fighter.
/// Not JSON and not a WebSocket: the device would take on a parser, a WS
/// library, reconnect logic and framing to gain push latency it cannot use —
/// the upstream statusline only ticks every 5 seconds.
async fn panel(State(app): State<App>) -> impl IntoResponse {
    let w = app.world.lock().unwrap();
    (
        [("content-type", "text/plain; charset=us-ascii")],
        w.panel_line(now_ms()),
    )
}

async fn ingest(
    ws: WebSocketUpgrade,
    State(app): State<App>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !bearer_ok(&app.token, &headers) {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    ws.on_upgrade(move |sock| ingest_socket(sock, app))
        .into_response()
}

async fn ingest_socket(mut sock: WebSocket, app: App) {
    while let Some(Ok(msg)) = sock.next().await {
        let Message::Text(txt) = msg else { continue };
        let Ok(obs) = serde_json::from_str::<Observation>(&txt) else {
            continue;
        };
        app.world.lock().unwrap().apply(obs, now_ms());
    }
}

async fn view(
    ws: WebSocketUpgrade,
    State(app): State<App>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    // Browsers cannot set headers on a WebSocket, so a gated view has to take
    // the token in the query string — where it lands in access logs. Off by
    // default for exactly that reason; see DESIGN.md.
    if app.view_token && q.get("token").map(String::as_str) != Some(app.token.as_str()) {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    ws.on_upgrade(move |sock| view_socket(sock, app))
        .into_response()
}

async fn view_socket(sock: WebSocket, app: App) {
    let (mut out, mut inp) = sock.split();
    let mut rx = app.tx.subscribe();
    loop {
        tokio::select! {
            r = rx.recv() => match r {
                Ok(snap) => { if out.send(Message::Text(snap)).await.is_err() { break; } }
                // A slow browser falls behind the broadcast ring. Skipping to
                // the newest snapshot is right: an old world is worthless.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            m = inp.next() => if m.is_none() { break },
        }
    }
}
