mod ai;
mod analysis;
mod game;
mod history;
mod maps;
mod room;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use history::{History, Record};
use room::{AppState, ClientMsg, Mode, Shared};
use serde::Deserialize;
use serde_json::json;
use std::future::IntoFuture;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

const INDEX: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLE: &str = include_str!("../static/style.css");
const FAVICON: &str = include_str!("../static/favicon.svg");

#[tokio::main]
async fn main() {
    let data = std::env::var_os("DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from);
    let st: Shared = Arc::new(AppState::new(History::open(data)));

    let sweeper = st.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            room::sweep(&sweeper);
        }
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/g/{id}", get(index))
        .route("/history", get(index))
        .route("/r/{id}", get(index))
        .route("/app.js", get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }))
        .route("/style.css", get(|| async { asset("text/css; charset=utf-8", STYLE) }))
        .route("/favicon.svg", get(|| async { asset("image/svg+xml", FAVICON) }))
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/games", post(create_game))
        .route("/api/history", get(list_history))
        .route("/api/history/{id}", get(get_record))
        .route("/api/history/{id}/analysis", get(get_analysis))
        .route("/ws/{id}", get(websocket))
        .with_state(st);

    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3000);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await.expect("cannot bind port");
    println!("bacteria listening on http://0.0.0.0:{port}");

    tokio::select! {
        res = axum::serve(listener, app).into_future() => res.expect("server error"),
        _ = shutdown_signal() => println!("shutting down"),
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

async fn index() -> Response {
    asset("text/html; charset=utf-8", INDEX)
}

#[derive(Deserialize)]
struct CreateGame {
    mode: String,
    difficulty: Option<ai::Difficulty>,
}

async fn create_game(State(st): State<Shared>, Json(req): Json<CreateGame>) -> Response {
    let mode = match (req.mode.as_str(), req.difficulty) {
        ("online", _) => Mode::Online,
        ("local", _) => Mode::Local,
        ("solo", Some(level)) => Mode::Solo(level),
        _ => return (StatusCode::BAD_REQUEST, "unknown mode").into_response(),
    };
    match room::create(&st, mode) {
        Some((id, token)) => Json(json!({ "id": id, "token": token })).into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "too many games, try again later").into_response(),
    }
}

#[derive(Deserialize)]
struct HistoryParams {
    /// Cursor from a previous page.
    before: Option<usize>,
    limit: Option<usize>,
    /// Comma-separated game ids, instead of a page.
    ids: Option<String>,
}

async fn list_history(State(st): State<Shared>, Query(q): Query<HistoryParams>) -> Response {
    let (games, next) = match &q.ids {
        Some(ids) => (st.history.some(&ids.split(',').take(200).collect::<Vec<_>>()), None),
        None => st.history.page(q.before, q.limit.unwrap_or(20).clamp(1, 50)),
    };
    let games: Vec<&Record> = games.iter().map(|r| &**r).collect();
    Json(json!({ "games": games, "next": next })).into_response()
}

async fn get_record(State(st): State<Shared>, Path(id): Path<String>) -> Response {
    match st.history.get(&id) {
        Some(r) => Json(&*r).into_response(),
        None => (StatusCode::NOT_FOUND, "no such game").into_response(),
    }
}

async fn get_analysis(State(st): State<Shared>, Path(id): Path<String>) -> Response {
    let Some(record) = st.history.get(&id) else {
        return (StatusCode::NOT_FOUND, "no such game").into_response();
    };
    match st.analyses.get(record).await {
        Some(json) => ([(header::CONTENT_TYPE, "application/json")], json.as_str().to_owned()).into_response(),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "analysis failed").into_response(),
    }
}

#[derive(Deserialize)]
struct WsParams {
    token: Option<String>,
}

async fn websocket(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    Query(params): Query<WsParams>,
    State(st): State<Shared>,
) -> Response {
    ws.max_message_size(4 * 1024).on_upgrade(move |socket| client(socket, st, id, params.token))
}

async fn client(socket: WebSocket, st: Shared, id: String, token: Option<String>) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let conn = st.next_conn.fetch_add(1, Ordering::Relaxed);

    let writer = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                return;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });

    if !room::join(&st, &id, conn, tx, token) {
        let _ = writer.await; // flushes the "not found" error, then closes
        return;
    }

    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => {
                if let Ok(msg) = serde_json::from_str::<ClientMsg>(text.as_str()) {
                    room::on_message(&st, &id, conn, msg);
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    room::leave(&st, &id, conn);
    writer.abort();
}
