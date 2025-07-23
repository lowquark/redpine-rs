#[test]
fn client_send() {
    let server_port = 10004;

    // Create client object in main thread to verify it implements Send
    let mut client = redpine::Client::connect(("127.0.0.1", server_port))
        .expect("failed to create redpine client");

    let thread = std::thread::spawn(move || {
        client.poll_event();
    });

    thread.join().unwrap();
}

#[test]
fn server_send() {
    let server_port = 10004;

    // Create server object in main thread to verify it implements Send
    let mut server = redpine::Server::bind(("127.0.0.1", server_port))
        .expect("failed to create redpine server");

    let thread = std::thread::spawn(move || {
        server.poll_event();
    });

    thread.join().unwrap();
}
