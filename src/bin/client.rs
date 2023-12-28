static EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);

fn main() {
    let config = redpine::client::Config {
        handshake_timeout_ms: 4_000,
        connection_timeout_ms: 4_000,
    };

    let mut client = redpine::client::Client::connect_with_config(("127.0.0.1", 8888), config)
        .expect("failed to create redpine client");

    loop {
        client.send(
            vec![0x00, 0x01, 0x02].into(),
            redpine::SendMode::Unreliable(0),
        );

        while let Some(event) = client.wait_event_timeout(EVENT_TIMEOUT) {
            match event {
                redpine::client::Event::Connect => {
                    println!("client::Event::Connect");
                }
                redpine::client::Event::Disconnect => {
                    println!("client::Event::Disconnect");
                }
                redpine::client::Event::Receive(packet_bytes) => {
                    println!("client::Event::Receive {packet_bytes:02X?}");
                }
                redpine::client::Event::Timeout => {
                    println!("client::Event::Timeout");
                }
            }
        }

        // println!("no event after {}ms", EVENT_TIMEOUT.as_millis());
    }
}
