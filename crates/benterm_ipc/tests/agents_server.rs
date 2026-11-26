//! The server tells the app when a connection that waits has hung up, so
//! the app can stop waiting for it.

mod common;

use std::net::Shutdown;
use std::time::Duration;

use benterm_ipc::ServerLimits;
use common::{HELD_AGENT, RawClient, start_server, start_server_with, wait_until};
use serde_json::json;

fn wait_line(id: u64, agent: &str) -> String {
    json!({"id": id, "method": "agent.wait", "params": {"agent": agent}}).to_string()
}

#[test]
fn a_wait_reaches_the_app_with_the_connection_it_came_from() {
    let server = start_server();
    let mut one = RawClient::connect(&server.path);
    let mut other = RawClient::connect(&server.path);

    one.send(&wait_line(1, HELD_AGENT));
    server.app.wait_for_waits(1);
    other.send(&wait_line(1, HELD_AGENT));
    server.app.wait_for_waits(2);
    one.send(&wait_line(2, HELD_AGENT));
    server.app.wait_for_waits(3);

    let waits = server.app.waits.lock().unwrap();
    assert_eq!(waits[0].0, waits[2].0);
    assert_ne!(waits[0].0, waits[1].0);
}

#[test]
fn the_app_is_told_once_when_a_connection_that_waits_goes_away() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);
    client.send(&wait_line(1, HELD_AGENT));
    server.app.wait_for_waits(1);
    assert_eq!(server.app.hung_up(), []);

    drop(client);

    wait_until(|| !server.app.hung_up().is_empty());
    let waiting = server.app.waits.lock().unwrap()[0].0;
    assert_eq!(server.app.hung_up(), [waiting]);
    // The app answers the wait it gave up, and the connection ends.
    drop(server.app.waits.lock().unwrap().pop());
    wait_until(|| server.server.connection_count() == 0);
    assert_eq!(server.app.hung_up(), [waiting]);
}

#[test]
fn a_connection_that_stops_sending_has_hung_up_and_still_gets_the_answer() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);
    client.send(&wait_line(1, HELD_AGENT));
    server.app.wait_for_waits(1);

    client.stream.shutdown(Shutdown::Write).unwrap();

    wait_until(|| !server.app.hung_up().is_empty());
    // What the app does with the wait it gives up is answered like any
    // request.
    drop(server.app.waits.lock().unwrap().pop());
    let response = client.read().expect("the answer still comes");
    assert_eq!(response["id"], json!(1));
    assert_eq!(response["error"]["code"], json!("internal"));
    assert_eq!(client.read(), None);
}

#[test]
fn the_idle_timeout_does_not_interrupt_a_pending_wait() {
    let idle_timeout = Duration::from_millis(200);
    let server = start_server_with(ServerLimits {
        idle_timeout,
        ..ServerLimits::default()
    });
    let mut client = RawClient::connect(&server.path);
    client.send(&wait_line(1, HELD_AGENT));
    server.app.wait_for_waits(1);

    std::thread::sleep(idle_timeout * 3);
    assert!(server.app.hung_up().is_empty());
    drop(server.app.waits.lock().unwrap().pop());
    assert_eq!(client.read().unwrap()["id"], 1);
}

#[test]
fn a_connection_whose_waits_the_app_did_not_keep_is_not_reported() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);
    client.call(r#"{"id":1,"method":"pane.read","params":{"pane":3}}"#);
    // The app answers this wait at once and says it keeps nothing.
    client.call(&wait_line(2, "w1"));

    drop(client);

    wait_until(|| server.server.connection_count() == 0);
    assert_eq!(server.app.hung_up(), []);
}
