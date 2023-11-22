use std::thread;

static EVENT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1000);

fn run_server() {
    println!("server thread");

    let mut ufl_server =
        ufl::Server::bind(("127.0.0.1", 8888)).expect("failed to create ufl server");

    let mut x = 0;

    'outer: loop {
        while let Some(event) = ufl_server.wait_event_timeout(EVENT_TIMEOUT) {
            match event {
                ufl::server::Event::Connect(_peer) => {
                    println!("server::Event::Connect");

                    assert_eq!(x, 0);
                    x += 1;
                }
                ufl::server::Event::Disconnect(_peer) => {
                    println!("server::Event::Disconnect");

                    assert_eq!(x, 1);
                    x += 1;

                    break 'outer;
                }
                ufl::server::Event::Receive(_, _) => {
                    panic!("server::Event::Receive");
                }
                ufl::server::Event::Timeout(_) => {
                    panic!("server::Event::Timeout");
                }
            }
        }
    }

    assert_eq!(x, 2);
}

fn run_client() {
    let mut ufl_client =
        ufl::client::Client::connect(("127.0.0.1", 8888)).expect("failed to create ufl client");

    let mut x = 0;

    'outer: loop {
        while let Some(event) = ufl_client.wait_event_timeout(EVENT_TIMEOUT) {
            match event {
                ufl::client::Event::Connect => {
                    println!("client::Event::Connect");

                    ufl_client.disconnect();

                    assert_eq!(x, 0);
                    x += 1;
                }
                ufl::client::Event::Disconnect => {
                    println!("client::Event::Disconnect");

                    assert_eq!(x, 1);
                    x += 1;

                    break 'outer;
                }
                ufl::client::Event::Receive(_) => {
                    panic!("client::Event::Receive");
                }
                ufl::client::Event::Timeout => {
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
