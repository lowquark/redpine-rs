static EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);

fn main() {
    let config = redpine::ServerConfig {
        peer_count_max: 1,
        connection_timeout_ms: 4_000,
    };

    let mut server = redpine::Server::bind_with_config(("127.0.0.1", 8888), config)
        .expect("failed to create redpine server");

    loop {
        while let Some(event) = server.wait_event_timeout(EVENT_TIMEOUT) {
            match event {
                redpine::ServerEvent::Connect(_peer) => {
                    println!("ServerEvent::Connect");
                }
                redpine::ServerEvent::Disconnect(_peer) => {
                    println!("ServerEvent::Disconnect");
                }
                redpine::ServerEvent::Receive(mut peer, packet_bytes) => {
                    println!("ServerEvent::Receive {:02X?}", packet_bytes);

                    peer.send(packet_bytes, redpine::SendMode::Unreliable(0));
                    peer.flush();
                }
                redpine::ServerEvent::Error(_peer, kind) => {
                    match kind {
                        redpine::ErrorKind::Timeout => {
                            println!("ServerEvent::Error(redpine::ErrorKind::Timeout)");
                        }
                        redpine::ErrorKind::Capacity => {
                            println!("ServerEvent::Error(redpine::ErrorKind::Capacity)");
                        }
                        redpine::ErrorKind::Parameter => {
                            println!("ServerEvent::Error(redpine::ErrorKind::Parameter)");
                        }
                    }
                }
            }
        }

        // println!("no event after {}ms", EVENT_TIMEOUT.as_millis());
    }
}
