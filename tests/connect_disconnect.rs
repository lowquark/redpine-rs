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
                redpine::server::Event::Connect(_peer) => {
                    println!("server::Event::Connect");

                    assert_eq!(x, 0);
                    x += 1;
                }
                redpine::server::Event::Disconnect(_peer) => {
                    println!("server::Event::Disconnect");

                    assert_eq!(x, 1);
                    x += 1;

                    break 'outer;
                }
                redpine::server::Event::Receive(_, _) => {
                    panic!("server::Event::Receive");
                }
                redpine::server::Event::Timeout(_) => {
                    panic!("server::Event::Timeout");
                }
            }
        }
    }

    assert_eq!(x, 2);
}

fn run_client() {
    let mut client = redpine::client::Client::connect(("127.0.0.1", 8888))
        .expect("failed to create redpine client");

    let mut x = 0;

    'outer: loop {
        while let Some(event) = client.wait_event_timeout(EVENT_TIMEOUT) {
            match event {
                redpine::client::Event::Connect => {
                    println!("client::Event::Connect");

                    client.disconnect();

                    assert_eq!(x, 0);
                    x += 1;
                }
                redpine::client::Event::Disconnect => {
                    println!("client::Event::Disconnect");

                    assert_eq!(x, 1);
                    x += 1;

                    break 'outer;
                }
                redpine::client::Event::Receive(_) => {
                    panic!("client::Event::Receive");
                }
                redpine::client::Event::Timeout => {
                    panic!("client::Event::Timeout");
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
