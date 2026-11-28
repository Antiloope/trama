// SPDX-License-Identifier: GPL-3.0-or-later

//! The API over HTTP and a WebSocket (axum, on a tokio runtime of one
//! thread). Handlers hold no state of their own: each request becomes a
//! message for the project, and each WebSocket client gets the project's
//! changes through a queue of its own.
//!
//! It listens on 127.0.0.1 and ::1 only. Any page open in a browser on the
//! same computer can send requests to localhost, so a request whose `Host`
//! is not this server, or whose `Origin` is another site, is refused.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Json, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use crossbeam_channel::Sender;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

use super::project::ToProject;
use super::{StartError, log};
use crate::api::{self, CHANGES_PATH, CONTROL_PATH, ControlRequest, ErrorReply, STATUS_PATH};

/// The changes a WebSocket client may have waiting before it is let go.
const WATCHER_QUEUE: usize = 4096;

/// How long connections get to close once the server stops.
const CLOSING_TIME: Duration = Duration::from_secs(1);

/// Listens on localhost at `port`, or any free port for 0. Returns the
/// listeners and the port they share.
pub(super) fn bind(port: u16) -> Result<(Vec<TcpListener>, u16), StartError> {
    let failed = |error| StartError::Io {
        doing: "listening on localhost",
        error,
    };
    // A free port on 127.0.0.1 may be taken on ::1; then another is tried.
    let mut attempts = if port == 0 { 8 } else { 1 };
    loop {
        attempts -= 1;
        let ipv4 = match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(ipv4) => ipv4,
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                return Err(StartError::PortTaken(port));
            }
            Err(error) => return Err(failed(error)),
        };
        let bound = ipv4.local_addr().map_err(failed)?.port();
        let mut listeners = vec![ipv4];
        // Browsers may try ::1 first for localhost. A system without IPv6
        // cannot listen there, and is reached on 127.0.0.1 alone.
        match TcpListener::bind((Ipv6Addr::LOCALHOST, bound)) {
            Ok(ipv6) => listeners.push(ipv6),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                if attempts == 0 {
                    return Err(StartError::PortTaken(bound));
                }
                continue;
            }
            Err(_) => {}
        }
        for listener in &listeners {
            listener.set_nonblocking(true).map_err(failed)?;
        }
        return Ok((listeners, bound));
    }
}

/// Serves the API on its own thread until `stopping` turns true.
pub(super) fn start(
    listeners: Vec<TcpListener>,
    port: u16,
    to_project: Sender<ToProject>,
    stopping: watch::Receiver<bool>,
) -> io::Result<JoinHandle<()>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    thread::Builder::new().name("api".into()).spawn(move || {
        let api = Api { to_project, port };
        if let Err(error) = runtime.block_on(serve(listeners, api, stopping)) {
            log("api", error);
        }
    })
}

async fn serve(
    listeners: Vec<TcpListener>,
    api: Api,
    stopping: watch::Receiver<bool>,
) -> io::Result<()> {
    let app = Router::new()
        .route("/", get(page))
        .route(STATUS_PATH, get(status))
        .route(CONTROL_PATH, post(control))
        .route(CHANGES_PATH, get(changes))
        .layer(middleware::from_fn_with_state(
            api.clone(),
            only_this_server,
        ))
        .with_state(api);
    let mut servers = JoinSet::new();
    for listener in listeners {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let (app, stopped) = (app.clone(), stopped(stopping.clone()));
        servers.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stopped)
                .await
        });
    }
    tokio::select! {
        _ = async { while servers.join_next().await.is_some() {} } => {}
        _ = async {
            stopped(stopping).await;
            tokio::time::sleep(CLOSING_TIME).await;
        } => {}
    }
    Ok(())
}

async fn stopped(mut stopping: watch::Receiver<bool>) {
    let _ = stopping.wait_for(|stopping| *stopping).await;
}

#[derive(Clone)]
struct Api {
    to_project: Sender<ToProject>,
    port: u16,
}

impl Api {
    /// The project's answer, or `None` once it has stopped.
    async fn ask<T>(&self, message: impl FnOnce(oneshot::Sender<T>) -> ToProject) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        self.to_project.send(message(reply)).ok()?;
        answer.await.ok()
    }

    /// Whether a request was sent to this server, and not by a page of
    /// another site.
    fn sent_here(&self, headers: &HeaderMap) -> bool {
        let header = |name| headers.get(name).map(|value| value.to_str());
        let host = matches!(header(header::HOST), Some(Ok(host)) if self.is_this_server(host));
        let origin = match header(header::ORIGIN) {
            // Only browsers send an origin; other clients are programs on
            // this computer.
            None => true,
            Some(Ok(origin)) => origin
                .strip_prefix("http://")
                .is_some_and(|host| self.is_this_server(host)),
            Some(Err(_)) => false,
        };
        host && origin
    }

    /// Whether `host`, as a `Host` header or an origin without its scheme,
    /// names this server.
    fn is_this_server(&self, host: &str) -> bool {
        let Some((name, port)) = host.rsplit_once(':') else {
            return false;
        };
        port.parse() == Ok(self.port) && matches!(name, "localhost" | "127.0.0.1" | "[::1]")
    }
}

fn error(status: StatusCode, error: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorReply {
            error: error.into(),
        }),
    )
        .into_response()
}

fn stopping() -> Response {
    error(StatusCode::SERVICE_UNAVAILABLE, "Trama is stopping")
}

async fn only_this_server(State(api): State<Api>, request: Request, next: Next) -> Response {
    if api.sent_here(request.headers()) {
        next.run(request).await
    } else {
        error(
            StatusCode::FORBIDDEN,
            "Trama answers on localhost only, and only to its own pages",
        )
    }
}

async fn page(State(api): State<Api>) -> Response {
    match api.ask(ToProject::Status).await {
        Some(status) => api::describe(&status, api.port).into_response(),
        None => stopping(),
    }
}

async fn status(State(api): State<Api>) -> Response {
    match api.ask(ToProject::Status).await {
        Some(status) => Json(status).into_response(),
        None => stopping(),
    }
}

async fn control(
    State(api): State<Api>,
    request: Result<Json<ControlRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(request) => request,
        Err(rejection) => return error(rejection.status(), rejection.body_text()),
    };
    match api.ask(|reply| ToProject::Control(request, reply)).await {
        Some(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Some(Err(reason)) => error(StatusCode::BAD_REQUEST, reason),
        None => stopping(),
    }
}

async fn changes(State(api): State<Api>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| watch_changes(api, socket))
}

async fn watch_changes(api: Api, mut socket: WebSocket) {
    let (watcher, mut events) = mpsc::channel(WATCHER_QUEUE);
    if api.to_project.send(ToProject::Watch(watcher)).is_err() {
        return;
    }
    log("client", "watching");
    loop {
        tokio::select! {
            event = events.recv() => {
                // The project lets a client go when it falls behind, and
                // every client when Trama stops.
                let Some(event) = event else {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                };
                let Ok(text) = serde_json::to_string(&event) else {
                    break;
                };
                if socket.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            message = socket.recv() => match message {
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                // Clients have nothing to say here; requests go over HTTP.
                Some(Ok(_)) => {}
            },
        }
    }
    log("client", "gone");
}
