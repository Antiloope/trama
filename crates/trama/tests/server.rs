// SPDX-License-Identifier: GPL-3.0-or-later

//! The server as a black box: started inside the test with no sound card,
//! on a free port and a temporary user data folder, and driven through its
//! API the way the command line and the web page drive it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use tempfile::TempDir;
use trama::api::{
    Action, CHANGES_PATH, Cell, Change, ControlRequest, Event, Project, STATUS_PATH, Status,
};
use trama::client::Client;
use trama::server::{Server, ServerConfig, StartError};
use trama::user_data::{Running, UserData};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const TIMEOUT: Duration = Duration::from_secs(5);

struct TestServer {
    server: Server,
    data: UserData,
    _folder: TempDir,
}

fn start() -> TestServer {
    let folder = tempfile::tempdir().unwrap();
    let data = UserData::at(folder.path().join("trama"));
    let server = Server::start(ServerConfig {
        port: 0,
        user_data: data.clone(),
        sound_card: false,
    })
    .unwrap();
    TestServer {
        server,
        data,
        _folder: folder,
    }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn watch(port: u16) -> Socket {
    let (socket, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}{CHANGES_PATH}")).unwrap();
    if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    }
    socket
}

fn next_event(socket: &mut Socket) -> Event {
    loop {
        if let Message::Text(text) = socket.read().unwrap() {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

fn snapshot(socket: &mut Socket) -> Status {
    match next_event(socket) {
        Event::Snapshot(status) => status,
        other => panic!("expected a snapshot first, got {other:?}"),
    }
}

fn press(module: &str, input: &str) -> ControlRequest {
    ControlRequest {
        module: module.into(),
        input: input.into(),
        cell: None,
        action: Action::Press { velocity: 1.0 },
    }
}

fn transport(playing: bool, bpm: f64) -> Change {
    Change::Transport {
        clock: "clock".into(),
        playing,
        bpm,
    }
}

/// A raw HTTP request, so the test chooses every header.
fn raw_get(port: u16, path: &str, headers: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\n{headers}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn the_scratch_project_starts_with_its_main_clock_stopped() {
    let running = start();
    let status = Client::at(running.server.port()).status().unwrap();
    assert_eq!(status.project, Project::Scratch);
    assert_eq!(status.sound_card, None);
    assert_eq!(status.modules, vec!["clock".to_string()]);
    assert_eq!(status.state, vec![transport(false, 120.0)]);
    assert_eq!(status.warnings, vec!["no sound card, so nothing plays"]);
    assert!(running.data.scratch_project().is_dir());
}

#[test]
fn two_clients_see_the_same_change() {
    let running = start();
    let port = running.server.port();
    let mut first = watch(port);
    let mut second = watch(port);
    for socket in [&mut first, &mut second] {
        assert_eq!(snapshot(socket).state, vec![transport(false, 120.0)]);
    }

    Client::at(port).control(&press("clock", "play")).unwrap();
    for socket in [&mut first, &mut second] {
        assert_eq!(next_event(socket), Event::Change(transport(true, 120.0)));
    }

    let tempo = ControlRequest {
        action: Action::Move { value: 96.0 },
        ..press("clock", "tempo")
    };
    Client::at(port).control(&tempo).unwrap();
    for socket in [&mut first, &mut second] {
        assert_eq!(next_event(socket), Event::Change(transport(true, 96.0)));
    }

    // A client that arrives later starts from where the others are.
    let mut third = watch(port);
    assert_eq!(snapshot(&mut third).state, vec![transport(true, 96.0)]);
}

#[test]
fn stopping_the_server_stops_the_transport_and_lets_clients_go() {
    let running = start();
    let port = running.server.port();
    let mut socket = watch(port);
    snapshot(&mut socket);
    Client::at(port).control(&press("clock", "play")).unwrap();
    assert_eq!(
        next_event(&mut socket),
        Event::Change(transport(true, 120.0))
    );

    running.server.stop();
    assert_eq!(
        next_event(&mut socket),
        Event::Change(transport(false, 120.0))
    );
    loop {
        match socket.read() {
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    assert!(Client::find(&running.data).is_err());
}

#[test]
fn controls_a_module_does_not_have_are_refused_with_a_reason() {
    let running = start();
    let client = Client::at(running.server.port());
    let refusal = |request: ControlRequest| client.control(&request).unwrap_err().to_string();

    assert_eq!(
        refusal(press("looper", "play")),
        "there is no module looper"
    );
    assert_eq!(
        refusal(press("Clock", "play")),
        "\"Clock\" is not a module name"
    );
    assert_eq!(
        refusal(press("clock", "volume")),
        "module clock has no control input \"volume\""
    );
    assert_eq!(
        refusal(ControlRequest {
            cell: Some(Cell { row: 0, column: 0 }),
            ..press("clock", "play")
        }),
        "input \"play\" of clock is one control: it takes no cell"
    );
    assert_eq!(
        refusal(ControlRequest {
            action: Action::Press { velocity: 2.0 },
            ..press("clock", "play")
        }),
        "a velocity goes from 0.0 to 1.0"
    );
    // Nothing reached the clock.
    assert_eq!(
        client.status().unwrap().state,
        vec![transport(false, 120.0)]
    );
}

#[test]
fn commands_find_the_server_through_the_user_data_folder() {
    let running = start();
    assert_eq!(
        running.data.running_server().unwrap(),
        Some(Running::At {
            port: running.server.port()
        })
    );
    let client = Client::find(&running.data).unwrap();
    assert_eq!(client.port(), running.server.port());

    let data = running.data.clone();
    running.server.stop();
    let error = Client::find(&data).err().unwrap().to_string();
    assert!(error.contains("start one with `trama`"), "{error}");
}

#[test]
fn a_second_server_says_where_the_first_one_is() {
    let running = start();
    let second = Server::start(ServerConfig {
        port: 0,
        user_data: running.data.clone(),
        sound_card: false,
    });
    let port = running.server.port();
    match second {
        Err(StartError::AlreadyRunning(Running::At { port: at })) => assert_eq!(at, port),
        Err(other) => panic!("expected AlreadyRunning, got {other}"),
        Ok(_) => panic!("a second server started"),
    }

    let data = running.data.clone();
    running.server.stop();
    let again = Server::start(ServerConfig {
        port: 0,
        user_data: data,
        sound_card: false,
    });
    assert!(again.is_ok());
}

#[test]
fn a_port_in_use_is_reported() {
    let running = start();
    let folder = tempfile::tempdir().unwrap();
    let other = Server::start(ServerConfig {
        port: running.server.port(),
        user_data: UserData::at(folder.path().join("trama")),
        sound_card: false,
    });
    assert!(matches!(other, Err(StartError::PortTaken(port)) if port == running.server.port()));
}

#[test]
fn the_page_shows_the_status_as_text() {
    let running = start();
    let port = running.server.port();
    let response = raw_get(port, "/", &format!("Host: localhost:{port}\r\n"));
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("text/plain"), "{response}");
    assert!(response.contains("the scratch project"), "{response}");
    assert!(response.contains("stopped, 120 BPM"), "{response}");
}

#[test]
fn requests_from_pages_of_other_sites_are_refused() {
    let running = start();
    let port = running.server.port();
    let ok = |response: String| response.starts_with("HTTP/1.1 200");
    let refused = |response: String| response.starts_with("HTTP/1.1 403");

    assert!(ok(raw_get(
        port,
        STATUS_PATH,
        &format!("Host: localhost:{port}\r\n")
    )));
    assert!(ok(raw_get(
        port,
        STATUS_PATH,
        &format!("Host: 127.0.0.1:{port}\r\nOrigin: http://localhost:{port}\r\n")
    )));
    // A name that resolves to this computer, as some attacks arrange.
    assert!(refused(raw_get(
        port,
        STATUS_PATH,
        &format!("Host: example.com:{port}\r\n")
    )));
    assert!(refused(raw_get(port, STATUS_PATH, "")));
    assert!(refused(raw_get(
        port,
        STATUS_PATH,
        &format!("Host: localhost:{port}\r\nOrigin: https://example.com\r\n")
    )));
    assert!(refused(raw_get(
        port,
        STATUS_PATH,
        &format!("Host: localhost:{port}\r\nOrigin: null\r\n")
    )));

    // The changes too: a page of another site cannot watch them.
    let mut request = format!("ws://127.0.0.1:{port}{CHANGES_PATH}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Origin", "https://example.com".parse().unwrap());
    assert!(tungstenite::connect(request).is_err());
}

/// A form on another site can post plain text without the browser asking
/// the server first, but not JSON.
#[test]
fn a_control_must_be_json() {
    let running = start();
    let port = running.server.port();
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    let body = r#"{"module":"clock","input":"play","action":{"press":{"velocity":1.0}}}"#;
    write!(
        stream,
        "POST /api/control HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Type: text/plain\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 415"), "{response}");
    let client = Client::at(port);
    assert_eq!(
        client.status().unwrap().state,
        vec![transport(false, 120.0)]
    );
}
