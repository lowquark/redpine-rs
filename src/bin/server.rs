static EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);

fn main() {
    let config = ufl::server::Config {
        peer_count_max: 1,
        connection_timeout_ms: 4_000,
    };

    let mut ufl_server = ufl::Server::bind_with_config(("127.0.0.1", 8888), config)
        .expect("failed to create ufl server");

    loop {
        while let Some(event) = ufl_server.wait_event_timeout(EVENT_TIMEOUT) {
            match event {
                ufl::server::Event::Connect(_peer) => {
                    println!("server::Event::Connect");
                }
                ufl::server::Event::Disconnect(_peer) => {
                    println!("server::Event::Disconnect");
                }
                ufl::server::Event::Receive(mut peer, packet_bytes) => {
                    println!("server::Event::Receive {:02X?}", packet_bytes);

                    peer.send(packet_bytes, ufl::SendMode::Unreliable(0));
                    peer.flush();
                }
                ufl::server::Event::Timeout(_peer) => {
                    println!("server::Event::Timeout");
                }
            }
        }

        // println!("no event after {}ms", EVENT_TIMEOUT.as_millis());
    }
}
