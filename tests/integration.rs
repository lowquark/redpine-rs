mod client_server_trial;

#[cfg(test)]
mod integration {
    use super::client_server_trial::*;

    // Run in parallel at your own risk

    #[test]
    #[ignore]
    fn single_client_connect_disconnect() {
        let config = Config {
            server_port: 10000,
            client_count: 1,
            client_connect_timeout_ms: 1000,
            client_disconnect_timeout_ms: 1000,
            client_action: ClientAction::HurryUpAndWait(500),
        };

        run(config);
    }

    #[test]
    #[ignore]
    fn single_client_transfer() {
        let config = Config {
            server_port: 10001,
            client_count: 1,
            client_connect_timeout_ms: 1000,
            client_disconnect_timeout_ms: 1000,
            client_action: ClientAction::Transfer(TransferConfig {
                packet_count: 1000,
                packet_distribution: vec![10, 100, 100, 100, 100, 1000, 1000, 1000, 1000, 10000],
                pre_delay_ms: 500,
                post_delay_ms: 500,
                spacing_delay_ms: 1,
                summary_timeout_ms: 6000,
            }),
        };

        run(config);
    }

    #[test]
    #[ignore]
    fn multi_client_connect_disconnect() {
        let config = Config {
            server_port: 10002,
            client_count: 64,
            client_connect_timeout_ms: 2000,
            client_disconnect_timeout_ms: 2000,
            client_action: ClientAction::HurryUpAndWait(2000),
        };

        run(config);
    }

    #[test]
    #[ignore]
    fn multi_client_transfer() {
        let config = Config {
            server_port: 10003,
            client_count: 64,
            client_connect_timeout_ms: 2000,
            client_disconnect_timeout_ms: 2000,
            client_action: ClientAction::Transfer(TransferConfig {
                packet_count: 1000,
                packet_distribution: vec![10, 100, 100, 100, 100, 1000, 1000, 1000, 1000, 10000],
                pre_delay_ms: 2000,
                post_delay_ms: 2000,
                spacing_delay_ms: 1,
                summary_timeout_ms: 6000,
            }),
        };

        run(config);
    }
}
