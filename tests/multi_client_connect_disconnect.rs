#[allow(dead_code)]
mod common;

use common::basic_trial;

#[test]
fn multi_client_connect_disconnect() {
    let config = basic_trial::Config {
        server_port: 10002,
        client_count: 64,
        client_connect_timeout_ms: 2000,
        client_disconnect_timeout_ms: 2000,
        client_action: basic_trial::ClientAction::HurryUpAndWait(2000),
    };

    basic_trial::run(config);
}
