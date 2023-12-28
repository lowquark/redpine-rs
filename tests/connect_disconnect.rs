use std::thread;

static EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);

fn run_server() {
    println!("server thread");

    let mut server =
        redpine::Server::bind(("127.0.0.1", 8888)).expect("failed to create redpine server");

    let mut x = 0;

    'outer: loop {
        while let Some(event) = server.wait_event_timeout(EVENT_TIMEOUT) {
            match event {
                redpine::ServerEvent::Connect(_peer) => {
                    println!("ServerEvent::Connect");

                    assert_eq!(x, 0);
                    x += 1;
                }
                redpine::ServerEvent::Disconnect(_peer) => {
                    println!("ServerEvent::Disconnect");

                    assert_eq!(x, 1);
                    x += 1;

                    break 'outer;
                }
                redpine::ServerEvent::Receive(_, _) => {
                    panic!("ServerEvent::Receive");
                }
                redpine::ServerEvent::Timeout(_) => {
                    panic!("ServerEvent::Timeout");
                }
            }
        }
    }

    assert_eq!(x, 2);
}

fn run_client() {
    let mut client =
        redpine::Client::connect(("127.0.0.1", 8888)).expect("failed to create redpine client");

    let mut x = 0;

    'outer: loop {
        while let Some(event) = client.wait_event_timeout(EVENT_TIMEOUT) {
            match event {
                redpine::ClientEvent::Connect => {
                    println!("ClientEvent::Connect");

                    client.disconnect();

                    assert_eq!(x, 0);
                    x += 1;
                }
                redpine::ClientEvent::Disconnect => {
                    println!("ClientEvent::Disconnect");

                    assert_eq!(x, 1);
                    x += 1;

                    break 'outer;
                }
                redpine::ClientEvent::Receive(_) => {
                    panic!("ClientEvent::Receive");
                }
                redpine::ClientEvent::Timeout => {
                    panic!("ClientEvent::Timeout");
                }
            }
        }
    }

    assert_eq!(x, 2);
}

#[test]
fn connect_disconnect() {
    let server_thread = thread::spawn(run_server);
    let client_thread = thread::spawn(run_client);

    server_thread.join().unwrap();
    client_thread.join().unwrap();
}
