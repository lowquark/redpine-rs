#[allow(dead_code)]
mod common;

static SERVER_PORT: u16 = 10005;

static PACKET_COUNT: usize = 5;

fn client_task() -> Vec<(std::time::Instant, Box<[u8]>)> {
    // Connect to the server, and receive PACKET_COUNT packets. Return the packets as well as the
    // time they were each received.

    let mut client = redpine::Client::connect(("127.0.0.1", SERVER_PORT))
        .expect("failed to create redpine client");

    let event = client.wait_event();

    match event {
        redpine::ClientEvent::Connect => (),
        _ => panic!("expected connect event"),
    }

    let mut received_packets = Vec::new();

    loop {
        match client.wait_event_timeout(std::time::Duration::from_millis(200)) {
            Some(redpine::ClientEvent::Receive(msg)) => {
                received_packets.push((std::time::Instant::now(), msg));

                if received_packets.len() == PACKET_COUNT {
                    return received_packets;
                }
            }
            None => (),
            _ => panic!("expected receive"),
        }
    }
}

#[test]
fn send_while_blocking() {
    // Spin up a server and spawn a client thread in the background.

    let mut server =
        redpine::Server::bind(("127.0.0.1", SERVER_PORT)).expect("failed to create redpine server");

    let client_thread = std::thread::spawn(client_task);

    // Wait until the client connects, then send the peer to a background thread and start waiting
    // for events. Send packets periodically to the client from the background thread.

    let event = server.wait_event();

    let mut peer = match event {
        redpine::ServerEvent::Connect(peer) => peer,
        _ => panic!("expected connect event"),
    };

    let blocking_start = std::time::Instant::now();

    std::thread::spawn(move || {
        for i in 0..5 {
            std::thread::sleep(std::time::Duration::from_millis(100));

            peer.send([i, i, i].into(), redpine::SendMode::Reliable);
        }
    });

    let event = server.wait_event_timeout(std::time::Duration::from_secs(1));

    assert!(event.is_none());

    let blocking_end = std::time::Instant::now();

    // Verify that those packets were received by the client while we were blocking.

    let received_packets = client_thread.join().unwrap();

    for (idx, (time, packet)) in received_packets.into_iter().enumerate() {
        let i = idx as u8;

        assert_eq!(packet, vec![i, i, i].into_boxed_slice());

        assert!(time >= blocking_start);
        assert!(time <= blocking_end);
    }
}
