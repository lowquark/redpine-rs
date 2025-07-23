#[allow(dead_code)]
mod common;

use common::basic_trial;

#[test]
fn single_client_transfer() {
    let config = basic_trial::Config {
        server_port: 10001,
        client_count: 1,
        client_connect_timeout_ms: 1000,
        client_disconnect_timeout_ms: 1000,
        client_action: basic_trial::ClientAction::Transfer(basic_trial::TransferConfig {
            packet_count: 1000,
            packet_distribution: vec![10, 100, 100, 100, 100, 1000, 1000, 1000, 1000, 10000],
            pre_delay_ms: 500,
            post_delay_ms: 500,
            spacing_delay_ms: 1,
            summary_timeout_ms: 6000,
        }),
    };

    basic_trial::run(config);
}
