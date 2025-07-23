#[allow(dead_code)]
mod common;

use common::basic_trial;

#[test]
fn single_client_connect_disconnect() {
    let config = basic_trial::Config {
        server_port: 10000,
        client_count: 1,
        client_connect_timeout_ms: 1000,
        client_disconnect_timeout_ms: 1000,
        client_action: basic_trial::ClientAction::HurryUpAndWait(500),
    };

    basic_trial::run(config);
}
