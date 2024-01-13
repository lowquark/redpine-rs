use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time;

#[derive(Debug)]
struct DataPacket {
    id: u16,
    unrel: bool,
    data: Box<[u8]>,
}

#[derive(Debug)]
struct SummaryPacket {
    rel_md5: [u8; 16],
    unrel_md5: [u8; 16],
    rel_log: Box<[bool]>,
    unrel_log: Box<[bool]>,
}

#[derive(Debug)]
enum Packet {
    Data(DataPacket),
    SummaryReq,
    Summary(SummaryPacket),
}

impl Packet {
    fn to_bytes(&self) -> Box<[u8]> {
        let mut v = Vec::new();

        match self {
            Self::Data(data_packet) => {
                v.push(if data_packet.unrel { 0x01 } else { 0x00 });
                v.extend_from_slice(&data_packet.id.to_le_bytes());
                v.extend_from_slice(&data_packet.data);
            }
            Self::SummaryReq => {
                v.push(0x02);
            }
            Self::Summary(summary_packet) => {
                v.push(0x03);

                v.extend_from_slice(&summary_packet.rel_md5);
                v.extend_from_slice(&summary_packet.unrel_md5);

                let len_u16: u16 = summary_packet.rel_log.len().try_into().unwrap();
                v.extend_from_slice(&len_u16.to_le_bytes());
                for &x in summary_packet.rel_log.iter() {
                    v.push(x as u8);
                }

                let len_u16: u16 = summary_packet.unrel_log.len().try_into().unwrap();
                v.extend_from_slice(&len_u16.to_le_bytes());
                for &x in summary_packet.unrel_log.iter() {
                    v.push(x as u8);
                }
            }
        }

        return v.into_boxed_slice();
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        let mut read_idx = 0;

        if bytes[0] == 0x00 || bytes[0] == 0x01 {
            let unrel = bytes[0] == 0x01;
            read_idx += 1;

            let id = u16::from_le_bytes(bytes[read_idx..read_idx + 2].try_into().unwrap());
            read_idx += 2;

            let data = bytes[read_idx..].into();

            Self::Data(DataPacket { unrel, id, data })
        } else if bytes[0] == 0x02 {
            Self::SummaryReq
        } else if bytes[0] == 0x03 {
            read_idx += 1;

            let rel_md5: [u8; 16] = bytes[read_idx..read_idx + 16].try_into().unwrap();
            read_idx += 16;

            let unrel_md5: [u8; 16] = bytes[read_idx..read_idx + 16].try_into().unwrap();
            read_idx += 16;

            let log_len =
                u16::from_le_bytes(bytes[read_idx..read_idx + 2].try_into().unwrap()) as usize;
            read_idx += 2;

            let mut rel_log = Vec::new();
            for &x in bytes[read_idx..read_idx + log_len].iter() {
                rel_log.push(x != 0);
                read_idx += 1;
            }

            let log_len =
                u16::from_le_bytes(bytes[read_idx..read_idx + 2].try_into().unwrap()) as usize;
            read_idx += 2;

            let mut unrel_log = Vec::new();
            for &x in bytes[read_idx..read_idx + log_len].iter() {
                unrel_log.push(x != 0);
                read_idx += 1;
            }

            Self::Summary(SummaryPacket {
                rel_md5,
                unrel_md5,
                rel_log: rel_log.into_boxed_slice(),
                unrel_log: unrel_log.into_boxed_slice(),
            })
        } else {
            panic!("invalid packet")
        }
    }
}

fn run_server(port: u16, config: redpine::ServerConfig, rx: mpsc::Receiver<()>) {
    // Always bind to core zero
    let core_ids = core_affinity::get_core_ids().expect("failed to query core IDs");
    let core_id = core_ids[0];

    let mut server = redpine::Server::bind_with_config(("127.0.0.1", port), config)
        .expect("failed to create redpine server");

    let success = core_affinity::set_for_current(core_id);
    assert_eq!(success, true);

    struct StreamData {
        md5_ctx: Option<md5::Context>,
        next_id: u16,
        log: Vec<bool>,
    }

    struct PeerData {
        rel_stream: StreamData,
        unrel_stream: StreamData,
    }

    let mut peer_data = std::collections::HashMap::new();

    loop {
        let event_timeout = time::Duration::from_millis(1000);

        while let Some(event) = server.wait_event_timeout(event_timeout) {
            match event {
                redpine::ServerEvent::Connect(peer) => {
                    peer_data.insert(
                        peer.id(),
                        PeerData {
                            rel_stream: StreamData {
                                md5_ctx: Some(md5::Context::new()),
                                next_id: 0,
                                log: Vec::new(),
                            },
                            unrel_stream: StreamData {
                                md5_ctx: Some(md5::Context::new()),
                                next_id: 0,
                                log: Vec::new(),
                            },
                        },
                    );
                }
                redpine::ServerEvent::Disconnect(_) => {}
                redpine::ServerEvent::Receive(mut peer, bytes) => {
                    let ref mut peer_data = peer_data.get_mut(&peer.id()).unwrap();

                    let packet = Packet::from_bytes(&bytes);

                    match packet {
                        Packet::Data(packet) => {
                            let stream_data = if packet.unrel {
                                &mut peer_data.unrel_stream
                            } else {
                                &mut peer_data.rel_stream
                            };
                            let ref mut md5_ctx = stream_data.md5_ctx.as_mut().unwrap();
                            let ref mut log = stream_data.log;

                            md5_ctx.consume(&packet.id.to_le_bytes());
                            md5_ctx.consume(&packet.data);

                            if packet.unrel && packet.id < stream_data.next_id {
                                panic!("unreliable packet received out of order");
                            }
                            if !packet.unrel && packet.id != stream_data.next_id {
                                panic!("reliable packet received out of order");
                            }
                            stream_data.next_id = packet.id + 1;

                            let log_idx = packet.id as usize;
                            let required_size = log_idx + 1;
                            log.resize(required_size, false);
                            if log[log_idx] == false {
                                log[log_idx] = true;
                            } else {
                                panic!("duplicate packet received");
                            }
                        }
                        Packet::SummaryReq => {
                            let packet = Packet::Summary(SummaryPacket {
                                rel_md5: peer_data
                                    .rel_stream
                                    .md5_ctx
                                    .take()
                                    .unwrap()
                                    .compute()
                                    .into(),
                                unrel_md5: peer_data
                                    .unrel_stream
                                    .md5_ctx
                                    .take()
                                    .unwrap()
                                    .compute()
                                    .into(),
                                rel_log: peer_data.rel_stream.log.clone().into(),
                                unrel_log: peer_data.unrel_stream.log.clone().into(),
                            });

                            peer.send(packet.to_bytes(), redpine::SendMode::Reliable);
                        }
                        _ => panic!(),
                    }
                }
                redpine::ServerEvent::Error(_, _) => {
                    panic!("ServerEvent::Error");
                }
            }
        }

        match rx.try_recv() {
            Ok(_) | Err(TryRecvError::Disconnected) => {
                break;
            }
            Err(TryRecvError::Empty) => {}
        }
    }
}

fn run_client(id: usize, server_port: u16, config: Config) {
    // Stay off the server's core (zero)
    let core_ids = core_affinity::get_core_ids().expect("failed to query core IDs");
    if core_ids.len() > 1 {
        let core_idx = 1 + (id % (core_ids.len() - 1));
        let core_id = core_ids[core_idx];

        let success = core_affinity::set_for_current(core_id);
        assert_eq!(success, true);
    } else {
        // ¯\_(ツ)_/¯
    }

    let connect_timeout = time::Duration::from_millis(config.client_connect_timeout_ms);
    let disconnect_timeout = time::Duration::from_millis(config.client_disconnect_timeout_ms);

    let mut client = redpine::Client::connect(("127.0.0.1", server_port))
        .expect("failed to create redpine client");

    let mut connected = false;

    while let Some(event) = client.wait_event_timeout(connect_timeout) {
        match event {
            redpine::ClientEvent::Connect => {
                println!("client {} connected", id);
                connected = true;
                break;
            }
            other => {
                panic!("unexpected {:?}", other);
            }
        }
    }

    if !connected {
        panic!("timed out waiting for connect");
    }

    let mut rel_md5_ctx = md5::Context::new();
    let mut unrel_md5_ctx = md5::Context::new();
    let mut rel_send_log = Vec::new();
    let mut unrel_send_log = Vec::new();

    match config.client_action {
        ClientAction::HurryUpAndWait(delay_ms) => {
            thread::sleep(time::Duration::from_millis(delay_ms));
        }
        ClientAction::Transfer(config) => {
            let pre_delay = time::Duration::from_millis(config.pre_delay_ms);
            let post_delay = time::Duration::from_millis(config.post_delay_ms);
            let spacing_delay = time::Duration::from_millis(config.spacing_delay_ms);

            if let Some(event) = client.wait_event_timeout(pre_delay) {
                panic!("unexpected {:?}", event);
            }

            println!("client {} starting transfer", id);

            for _ in 0..config.packet_count {
                let max_size = config.packet_distribution
                    [rand::random::<usize>() % config.packet_distribution.len()];
                let size = rand::random::<usize>() % max_size;
                let data = (0..size)
                    .map(|_| rand::random::<u8>())
                    .collect::<Vec<_>>()
                    .into_boxed_slice();

                let unrel = (rand::random::<usize>() % 2) == 1;

                let (packet_id, mode) = if unrel {
                    (unrel_send_log.len(), redpine::SendMode::Unreliable(500))
                } else {
                    (rel_send_log.len(), redpine::SendMode::Reliable)
                };

                let packet_id_u16: u16 = packet_id.try_into().unwrap();
                let packet = Packet::Data(DataPacket {
                    unrel,
                    id: packet_id_u16,
                    data: data.clone(),
                });

                client.send(packet.to_bytes(), mode.clone());

                if unrel {
                    unrel_send_log.push(data);
                } else {
                    rel_send_log.push(data);
                }

                if let Some(event) = client.wait_event_timeout(spacing_delay) {
                    panic!("unexpected {:?}", event);
                }
            }

            println!("client {} finished transfer", id);

            if let Some(event) = client.wait_event_timeout(post_delay) {
                panic!("unexpected {:?}", event);
            }

            let packet = Packet::SummaryReq;

            println!("client {} requesting summary", id);

            client.send(packet.to_bytes(), redpine::SendMode::Reliable);

            let summary_timeout = time::Duration::from_millis(config.summary_timeout_ms);

            if let Some(event) = client.wait_event_timeout(summary_timeout) {
                match event {
                    redpine::ClientEvent::Receive(bytes) => {
                        let packet = Packet::from_bytes(&bytes);

                        match packet {
                            Packet::Summary(packet) => {
                                let mut received_packet_count = 0;

                                for (idx, ref data) in rel_send_log.iter().enumerate() {
                                    if idx < packet.rel_log.len() {
                                        let was_received = packet.rel_log[idx];

                                        if was_received {
                                            let id: u16 = idx.try_into().unwrap();

                                            rel_md5_ctx.consume(&id.to_le_bytes());
                                            rel_md5_ctx.consume(&data);

                                            received_packet_count += 1;
                                        } else {
                                            panic!("reliable packet {} was not received", idx);
                                        }
                                    }
                                }

                                for (idx, ref data) in unrel_send_log.iter().enumerate() {
                                    if idx < packet.unrel_log.len() {
                                        let was_received = packet.unrel_log[idx];

                                        if was_received {
                                            let id: u16 = idx.try_into().unwrap();

                                            unrel_md5_ctx.consume(&id.to_le_bytes());
                                            unrel_md5_ctx.consume(&data);

                                            received_packet_count += 1;
                                        }
                                    }
                                }

                                if received_packet_count < config.packet_count {
                                    println!(
                                        "warning: client {} sent {} packets, but only {} were received",
                                        id, config.packet_count, received_packet_count
                                    );
                                }

                                // Expect that within each stream, the packets which were reported
                                // received had the expected bytes and were received in the correct
                                // order
                                assert_eq!(rel_md5_ctx.compute(), md5::Digest(packet.rel_md5));
                                assert_eq!(unrel_md5_ctx.compute(), md5::Digest(packet.unrel_md5));
                            }
                            other => panic!("unexpected {:?}", other),
                        }
                    }
                    other => panic!("unexpected {:?}", other),
                }
            }
        }
    }

    client.disconnect();

    while let Some(event) = client.wait_event_timeout(disconnect_timeout) {
        match event {
            redpine::ClientEvent::Disconnect => {
                println!("client {} disconnected", id);
                connected = false;
                break;
            }
            other => {
                panic!("unexpected {:?}", other);
            }
        }
    }

    if connected {
        panic!("timed out waiting for disconnect");
    }
}

#[derive(Clone)]
pub struct TransferConfig {
    pub packet_count: usize,
    pub packet_distribution: Vec<usize>,
    pub pre_delay_ms: u64,
    pub post_delay_ms: u64,
    pub spacing_delay_ms: u64,
    pub summary_timeout_ms: u64,
}

#[derive(Clone)]
pub enum ClientAction {
    HurryUpAndWait(u64),
    Transfer(TransferConfig),
}

#[derive(Clone)]
pub struct Config {
    pub server_port: u16,
    pub client_count: usize,
    pub client_connect_timeout_ms: u64,
    pub client_disconnect_timeout_ms: u64,
    pub client_action: ClientAction,
}

pub fn run(config: Config) {
    let server_port = config.server_port;

    let server_config = redpine::ServerConfig {
        peer_count_max: config.client_count,
        ..Default::default()
    };

    let (server_tx, server_rx) = mpsc::channel();

    let server_thread = thread::spawn(move || run_server(server_port, server_config, server_rx));
    let mut client_threads = Vec::new();

    let mut client_id = 0;
    for _ in 0..config.client_count {
        let config = config.clone();

        client_threads.push(thread::spawn(move || {
            run_client(client_id, server_port, config)
        }));

        client_id += 1;
    }

    for client_thread in client_threads.into_iter() {
        client_thread.join().unwrap();
    }

    let _ = server_tx.send(());

    server_thread.join().unwrap();
}
