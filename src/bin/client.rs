static EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);

fn main() {
    let config = redpine::ClientConfig {
        handshake_timeout_ms: 4_000,
        connection_timeout_ms: 4_000,
    };

    let mut client = redpine::Client::connect_with_config(("127.0.0.1", 8888), config)
        .expect("failed to create redpine client");

    loop {
        client.send(
            vec![0x00, 0x01, 0x02].into(),
            redpine::SendMode::Unreliable(0),
        );

        while let Some(event) = client.wait_event_timeout(EVENT_TIMEOUT) {
            match event {
                redpine::ClientEvent::Connect => {
                    println!("ClientEvent::Connect");
                }
                redpine::ClientEvent::Disconnect => {
                    println!("ClientEvent::Disconnect");
                }
                redpine::ClientEvent::Receive(packet_bytes) => {
                    println!("ClientEvent::Receive {packet_bytes:02X?}");
                }
                redpine::ClientEvent::Error(kind) => {
                    match kind {
                        redpine::ErrorKind::Timeout => {
                            println!("ClientEvent::Error(redpine::ErrorKind::Timeout)");
                        }
                        redpine::ErrorKind::Capacity => {
                            println!("ClientEvent::Error(redpine::ErrorKind::Capacity)");
                        }
                        redpine::ErrorKind::Parameter => {
                            println!("ClientEvent::Error(redpine::ErrorKind::Parameter)");
                        }
                    }
                }
            }
        }

        // println!("no event after {}ms", EVENT_TIMEOUT.as_millis());
    }
}
