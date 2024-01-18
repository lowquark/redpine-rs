use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt;
use std::net;
use std::rc::{Rc, Weak};
use std::time;

use siphasher::sip;

use super::endpoint;
use super::frame;
use super::socket;
use super::timer_wheel;
use super::ErrorKind;
use super::SendMode;

const TIMER_WHEEL_CONFIG: [timer_wheel::ArrayConfig; 3] = [
    timer_wheel::ArrayConfig {
        size: 32,
        ms_per_bin: 4,
    },
    timer_wheel::ArrayConfig {
        size: 32,
        ms_per_bin: 4 * 8,
    },
    timer_wheel::ArrayConfig {
        size: 32,
        ms_per_bin: 4 * 8 * 8,
    },
];

const FRAME_SIZE_MAX: usize = 1472;

const PEER_COUNT_MAX_DEFAULT: usize = 8;
const PEER_COUNT_MAX_MAX: usize = 65536;

const HANDSHAKE_TIMEOUT_MS: u32 = 10_000;

const CONNECTION_TIMEOUT_DEFAULT_MS: u64 = 10_000;
const CONNECTION_TIMEOUT_MIN_MS: u64 = 2_000;

#[derive(Clone)]
pub struct Config {
    pub peer_count_max: usize,
    pub connection_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            peer_count_max: PEER_COUNT_MAX_DEFAULT,
            connection_timeout_ms: CONNECTION_TIMEOUT_DEFAULT_MS,
        }
    }
}

impl Config {
    fn validate(&self) {
        assert!(
            self.peer_count_max > 0,
            "invalid server configuration: peer_count_max == 0"
        );
        assert!(
            self.peer_count_max <= PEER_COUNT_MAX_MAX,
            "invalid server configuration: peer_count_max > {}",
            PEER_COUNT_MAX_MAX
        );
        assert!(
            self.connection_timeout_ms >= CONNECTION_TIMEOUT_MIN_MS,
            "invalid server configuration: connection_timeout_ms < {}",
            CONNECTION_TIMEOUT_MIN_MS
        );
    }
}

type PeerId = u32;

struct PeerCore {
    // Peer identifier, 0 is a valid identifier
    id: PeerId,
    // Remote address
    addr: net::SocketAddr,
    // Current timer states
    rto_timer: Option<timer_wheel::TimerId>,
}

struct Peer {
    // Peer metadata
    core: PeerCore,
    // Endpoint representing this peer's connection
    endpoint: endpoint::Endpoint,
    // Permits server operations from a peer handle
    server_weak: ServerCoreWeak,
}

type PeerRc = Rc<RefCell<Peer>>;

struct PeerTable {
    // Mapping from sender address to peer
    peers: HashMap<net::SocketAddr, (PeerId, PeerRc)>,
    // Stack of unused IDs
    free_ids: Vec<PeerId>,
}

type TimerWheel = timer_wheel::TimerWheel<PeerRc>;

#[derive(Debug)]
pub enum Event {
    Connect(PeerHandle),
    Disconnect(PeerHandle),
    Receive(PeerHandle, Box<[u8]>),
    Error(PeerHandle, ErrorKind),
}

struct EndpointContext<'a> {
    server: &'a mut ServerCore,
    peer: &'a mut PeerCore,
    peer_rc: &'a PeerRc,
}

struct ServerCore {
    // Saved configuration
    config: Config,
    // Timestamps are computed relative to this instant
    time_ref: time::Instant,
    // Socket send handle
    socket_tx: socket::SocketTx,
    // Key used to compute handshake MACs
    siphash_key: [u8; 16],
    // Table of connected peers
    peer_table: PeerTable,
    // Pending timer events
    timer_wheel: TimerWheel,
    // Queue of pending events
    events: VecDeque<Event>,
    // Weak pointer to self, for peer creation
    self_weak: ServerCoreWeak,
}

type ServerCoreRc = Rc<RefCell<ServerCore>>;
type ServerCoreWeak = Weak<RefCell<ServerCore>>;

pub struct Server {
    // Interesting server data
    core: ServerCoreRc,
    // Socket receive handle
    socket_rx: socket::SocketRx,
    // Always-allocated timer expiration buffer
    timer_data_buffer: Vec<PeerRc>,
}

pub struct PeerHandle {
    peer: PeerRc,
}

impl Peer {
    fn new(
        id: PeerId,
        addr: net::SocketAddr,
        endpoint_config: endpoint::Config,
        server_weak: ServerCoreWeak,
    ) -> Self {
        Self {
            core: PeerCore {
                id,
                addr,
                rto_timer: None,
            },
            endpoint: endpoint::Endpoint::new(endpoint_config),
            server_weak,
        }
    }
}

impl PeerTable {
    pub fn new(count_max: usize) -> Self {
        debug_assert!(count_max > 0);
        debug_assert!(count_max - 1 <= PeerId::max_value() as usize);

        let peers = HashMap::new();
        let free_ids = (0..count_max)
            .map(|i| (count_max - 1 - i) as PeerId)
            .collect::<Vec<_>>();

        Self { peers, free_ids }
    }

    pub fn find(&self, addr: &net::SocketAddr) -> Option<&PeerRc> {
        self.peers.get(addr).map(|entry| &entry.1)
    }

    pub fn is_full(&self) -> bool {
        self.free_ids.len() == 0
    }

    pub fn contains(&self, addr: &net::SocketAddr) -> bool {
        self.peers.contains_key(addr)
    }

    pub fn insert(
        &mut self,
        addr: &net::SocketAddr,
        endpoint_config: endpoint::Config,
        server_weak: &ServerCoreWeak,
    ) -> PeerRc {
        // A peer may not already exist at this address
        debug_assert!(!self.peers.contains_key(addr));

        // Allocate an ID (an empty stack means we have reached the peer limit)
        let new_id = self.free_ids.pop().unwrap();

        // Create new peer object
        let peer = Peer::new(
            new_id,
            addr.clone(),
            endpoint_config,
            Weak::clone(server_weak),
        );
        let peer_rc = Rc::new(RefCell::new(peer));

        // Associate with given address
        self.peers
            .insert(addr.clone(), (new_id, Rc::clone(&peer_rc)));

        return peer_rc;
    }

    pub fn remove(&mut self, addr: &net::SocketAddr) {
        let (id, _) = self.peers.remove(addr).expect("double free?");
        self.free_ids.push(id);
    }

    pub fn count(&self) -> usize {
        self.peers.len()
    }
}

impl<'a> EndpointContext<'a> {
    fn new(server: &'a mut ServerCore, peer: &'a mut PeerCore, peer_rc: &'a PeerRc) -> Self {
        Self {
            server,
            peer,
            peer_rc,
        }
    }
}

impl<'a> endpoint::HostContext for EndpointContext<'a> {
    fn send_frame(&mut self, frame_bytes: &[u8]) {
        // println!("{:?} <- {:02X?}", self.peer.addr, frame_bytes);
        self.server.socket_tx.send(frame_bytes, &self.peer.addr);
    }

    fn set_rto_timer(&mut self, time_ms: u64) {
        let ref mut timer_id = self.peer.rto_timer;

        if let Some(id) = timer_id.take() {
            self.server.timer_wheel.unset_timer(id);
        }

        *timer_id = Some(
            self.server
                .timer_wheel
                .set_timer(time_ms, Rc::clone(self.peer_rc)),
        )
    }

    fn unset_rto_timer(&mut self) {
        let ref mut timer_id = self.peer.rto_timer;

        if let Some(id) = timer_id.take() {
            self.server.timer_wheel.unset_timer(id);
        }
    }

    fn destroy_self(&mut self) {
        self.server.peer_table.remove(&self.peer.addr);
    }

    fn on_connect(&mut self) {
        let handle = PeerHandle::new(Rc::clone(&self.peer_rc));
        let event = Event::Connect(handle);
        self.server.events.push_back(event);
    }

    fn on_disconnect(&mut self) {
        let handle = PeerHandle::new(Rc::clone(&self.peer_rc));
        let event = Event::Disconnect(handle);
        self.server.events.push_back(event);
    }

    fn on_receive(&mut self, packet_bytes: Box<[u8]>) {
        let handle = PeerHandle::new(Rc::clone(&self.peer_rc));
        let event = Event::Receive(handle, packet_bytes);
        self.server.events.push_back(event);
    }

    fn on_timeout(&mut self) {
        let handle = PeerHandle::new(Rc::clone(&self.peer_rc));
        let event = Event::Error(handle, ErrorKind::Timeout);
        self.server.events.push_back(event);
    }
}

fn compute_mac(
    key: &[u8; 16],
    sender_addr: &net::SocketAddr,
    client_nonce: u32,
    server_nonce: u32,
    server_timestamp: u32,
) -> u64 {
    use core::hash::Hasher;

    let mut hasher = sip::SipHasher13::new_with_key(key);

    match sender_addr {
        net::SocketAddr::V4(addr) => {
            hasher.write(&addr.ip().octets());
            hasher.write_u16(addr.port());
        }
        net::SocketAddr::V6(addr) => {
            hasher.write(&addr.ip().octets());
            hasher.write_u16(addr.port());
        }
    }

    hasher.write_u32(client_nonce);
    hasher.write_u32(server_nonce);
    hasher.write_u32(server_timestamp);

    return hasher.finish();
}

fn connection_params_compatible(a: &frame::ConnectionParams, b: &frame::ConnectionParams) -> bool {
    a.packet_size_in_max >= b.packet_size_out_max && b.packet_size_in_max >= a.packet_size_out_max
}

impl ServerCore {
    /// Returns the number of whole milliseconds elapsed since the server object was created.
    fn time_now_ms(&self) -> u64 {
        (time::Instant::now() - self.time_ref).as_millis() as u64
    }

    fn step_timer_wheel(&mut self, timer_data_buffer: &mut Vec<PeerRc>) -> u64 {
        let now_ms = self.time_now_ms();

        self.timer_wheel.step(now_ms, timer_data_buffer);

        return now_ms;
    }

    /// Returns a duration representing the time until the next timer event, if one exists.
    pub fn next_timer_timeout(&self) -> Option<time::Duration> {
        // WLOG, assume the time reference is zero. All units are ms, and `_ms` denotes an integer
        // timestamp as is convention in the code.
        //
        //   1: t_now_ms = floor(t_now) ≤ t_now
        //   1: t_now - t_now_ms ≥ 0
        //   2: t_wake ≥ t_now + (t_expire_ms - t_now_ms)
        //   2: t_wake ≥ (t_now - t_now_ms) + t_expire_ms
        // 2∘1: t_wake ≥ t_expire_ms
        // 2∘1: floor(t_wake) ≥ floor(t_expire_ms) = t_expire_ms
        // 2∘1: t_wake_ms ≥ t_expire_ms
        //
        // Since t_wake will round to a minimum of t_expire_ms, we will not skip a timer by waking
        // too soon.

        let now_ms = self.time_now_ms();

        return self
            .timer_wheel
            .next_expiration_time_ms()
            .map(|expire_time_ms| time::Duration::from_millis(expire_time_ms - now_ms));
    }

    /// Processes all pending timer events.
    pub fn handle_timeouts(&mut self, timer_data_buffer: &mut Vec<PeerRc>) {
        let now_ms = self.step_timer_wheel(timer_data_buffer);

        for peer_rc in timer_data_buffer.drain(..) {
            let ref mut peer = *peer_rc.borrow_mut();

            let ref mut ctx = EndpointContext::new(self, &mut peer.core, &peer_rc);

            peer.endpoint.handle_timer(now_ms, ctx);
        }
    }

    fn handle_handshake_alpha(
        &mut self,
        frame_bytes: &[u8],
        sender_addr: &net::SocketAddr,
        now_ms: u64,
    ) {
        use frame::serial::SimpleFrame;
        use frame::serial::SimpleFrameWrite;
        use frame::serial::SimplePayloadRead;

        if !frame::serial::verify_handshake_alpha_size_and_crc(frame_bytes, FRAME_SIZE_MAX) {
            return;
        }

        let payload_bytes = frame::serial::payload(frame_bytes);

        if let Some(frame) = frame::HandshakeAlphaFrame::read(payload_bytes) {
            if frame.protocol_id == frame::serial::PROTOCOL_ID {
                let server_params = frame::ConnectionParams {
                    packet_size_in_max: u32::max_value(),
                    packet_size_out_max: u32::max_value(),
                };
                let client_nonce = frame.client_nonce;
                let server_nonce = rand::random::<u32>();
                let server_timestamp = now_ms as u32;

                let ref ack_frame = frame::HandshakeAlphaAckFrame {
                    server_params,
                    client_nonce,
                    server_nonce,
                    server_timestamp,
                    server_mac: compute_mac(
                        &self.siphash_key,
                        sender_addr,
                        client_nonce,
                        server_nonce,
                        server_timestamp,
                    ),
                };

                let ref mut buffer = [0u8; frame::HandshakeAlphaAckFrame::FRAME_SIZE];
                let ack_frame_bytes = ack_frame.write(buffer);

                // println!("acking phase α...");
                self.socket_tx.send(&ack_frame_bytes, sender_addr);
            }
        }
    }

    fn handle_handshake_beta(
        &mut self,
        frame_bytes: &[u8],
        sender_addr: &net::SocketAddr,
        now_ms: u64,
    ) {
        use frame::serial::SimpleFrame;
        use frame::serial::SimpleFrameWrite;
        use frame::serial::SimplePayloadRead;

        if !frame::serial::verify_handshake_beta_size_and_crc(frame_bytes) {
            return;
        }

        let payload_bytes = frame::serial::payload(frame_bytes);

        if let Some(frame) = frame::HandshakeBetaFrame::read(payload_bytes) {
            let timestamp_now = now_ms as u32;

            let computed_mac = compute_mac(
                &self.siphash_key,
                sender_addr,
                frame.client_nonce,
                frame.server_nonce,
                frame.server_timestamp,
            );

            let mac_valid = frame.server_mac == computed_mac;
            let timestamp_valid =
                timestamp_now.wrapping_sub(frame.server_timestamp) < HANDSHAKE_TIMEOUT_MS;

            if mac_valid && timestamp_valid {
                let server_params = frame::ConnectionParams {
                    packet_size_in_max: u32::max_value(),
                    packet_size_out_max: u32::max_value(),
                };

                let params_compatible =
                    connection_params_compatible(&frame.client_params, &server_params);

                let error = if self.peer_table.contains(sender_addr) {
                    None
                } else if self.peer_table.is_full() {
                    Some(frame::HandshakeErrorKind::Capacity)
                } else if !params_compatible {
                    Some(frame::HandshakeErrorKind::Parameter)
                } else {
                    let endpoint_config = endpoint::Config {
                        timeout_time_ms: self.config.connection_timeout_ms,
                    };

                    let peer_rc =
                        self.peer_table
                            .insert(sender_addr, endpoint_config, &self.self_weak);

                    let now_ms = self.time_now_ms();

                    let ref mut peer = *peer_rc.borrow_mut();

                    let ref mut endpoint = peer.endpoint;
                    let ref mut peer_core = peer.core;

                    let ref mut ctx = EndpointContext::new(self, peer_core, &peer_rc);

                    endpoint.init(now_ms, ctx);

                    /*
                    println!(
                        "created peer {}, for sender address {:?}",
                        peer_core.id, sender_addr
                    );
                    */

                    None
                };

                // Always send an ack in case a previous ack was dropped
                let ref ack_frame = frame::HandshakeBetaAckFrame {
                    client_nonce: frame.client_nonce,
                    error,
                };

                let ref mut buffer = [0u8; frame::HandshakeBetaAckFrame::FRAME_SIZE];
                let ack_frame_bytes = ack_frame.write(buffer);

                // println!("acking phase β...");
                self.socket_tx.send(&ack_frame_bytes, sender_addr);
            }
        }
    }

    fn handle_frame_other(
        &mut self,
        frame_type: frame::FrameType,
        frame_bytes: &[u8],
        sender_addr: &net::SocketAddr,
        now_ms: u64,
    ) {
        // If a peer is associated with this address, deliver this frame to its endpoint
        if let Some(peer_rc) = self.peer_table.find(sender_addr) {
            if !frame::serial::verify_crc(frame_bytes) {
                return;
            }

            let payload_bytes = frame::serial::payload(frame_bytes);

            let peer_rc = Rc::clone(&peer_rc);

            let ref mut peer = *peer_rc.borrow_mut();

            let ref mut endpoint = peer.endpoint;
            let ref mut peer_core = peer.core;

            let ref mut ctx = EndpointContext::new(self, peer_core, &peer_rc);

            endpoint.handle_frame(frame_type, payload_bytes, now_ms, ctx);
        }
    }

    fn handle_frame(&mut self, frame_bytes: &[u8], sender_addr: &net::SocketAddr) {
        // println!("{:?} -> {:02X?}", sender_addr, frame_bytes);

        if !frame::serial::verify_minimum_size(frame_bytes) {
            return;
        }

        if let Some(frame_type) = frame::serial::read_type(frame_bytes) {
            // Initial handshakes are handled without an allocation in the peer table. Once a valid
            // open request is received, the sender's address is assumed valid (i.e. blockable) and
            // an entry in the peer table is created. Other frame types are handled by the
            // associated peer object.

            let now_ms = self.time_now_ms();

            match frame_type {
                frame::FrameType::HandshakeAlpha => {
                    self.handle_handshake_alpha(frame_bytes, sender_addr, now_ms);
                }
                frame::FrameType::HandshakeBeta => {
                    self.handle_handshake_beta(frame_bytes, sender_addr, now_ms);
                }
                _ => {
                    self.handle_frame_other(frame_type, frame_bytes, sender_addr, now_ms);
                }
            }
        }
    }

    /// Reads and processes as many frames as possible from socket_rx without blocking.
    pub fn handle_frames(&mut self, socket_rx: &mut socket::SocketRx) {
        while let Ok(Some((frame_bytes, sender_addr))) = socket_rx.try_read_frame() {
            // Process this frame
            self.handle_frame(frame_bytes, &sender_addr);
        }
    }

    /// Reads and processes as many frames as possible from socket_rx, waiting up to `wait_timeout`
    /// for the first.
    pub fn handle_frames_wait(
        &mut self,
        socket_rx: &mut socket::SocketRx,
        wait_timeout: Option<time::Duration>,
    ) {
        if let Ok(Some((frame_bytes, sender_addr))) = socket_rx.wait_for_frame(wait_timeout) {
            // Process this frame
            self.handle_frame(frame_bytes, &sender_addr);
            // Process any further frames without blocking
            self.handle_frames(socket_rx);
        }
    }
}

impl Server {
    pub fn bind<A>(bind_addr: A) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        Self::bind_with_config(bind_addr, Default::default())
    }

    pub fn bind_with_config<A>(bind_addr: A, config: Config) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        config.validate();

        let time_ref = time::Instant::now();

        let (socket_tx, socket_rx) = socket::new(bind_addr, FRAME_SIZE_MAX)?;

        // XXX TODO: CSPRNG!
        let siphash_key = (0..16)
            .map(|_| rand::random::<u8>())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        let peer_table = PeerTable::new(config.peer_count_max);
        let timer_wheel = TimerWheel::new(&TIMER_WHEEL_CONFIG, 0);

        // In order to do interesting things with Peer objects, PeerHandles require Peers to keep a
        // (weak) pointer to the ServerCore object. The most convenient way to accomplish this is
        // to give ServerCore a weak pointer to itself that can be cloned upon Peer construction.
        // The alternative is to pass pointers to the ServerCore down the frame handling call
        // stack, which is cumbersome.
        let core = Rc::new_cyclic(|self_weak| {
            RefCell::new(ServerCore {
                config,
                time_ref,
                socket_tx,
                siphash_key,
                peer_table,
                timer_wheel,
                events: VecDeque::new(),
                self_weak: Weak::clone(self_weak),
            })
        });

        Ok(Self {
            core,
            socket_rx,
            timer_data_buffer: Vec::new(),
        })
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts in an attempt to produce an event.
    ///
    /// Returns `None` if no events are available, or if an error was encountered while reading
    /// from the internal socket.
    pub fn poll_event(&mut self) -> Option<Event> {
        let ref mut core = *self.core.borrow_mut();

        if core.events.is_empty() {
            core.handle_frames(&mut self.socket_rx);

            core.handle_timeouts(&mut self.timer_data_buffer);
        }

        return core.events.pop_front();
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts until an event can be returned.
    pub fn wait_event(&mut self) -> Event {
        let ref mut core = *self.core.borrow_mut();

        loop {
            let wait_timeout = core.next_timer_timeout();

            core.handle_frames_wait(&mut self.socket_rx, wait_timeout);

            core.handle_timeouts(&mut self.timer_data_buffer);

            if let Some(event) = core.events.pop_front() {
                return event;
            }
        }
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts until an event can be returned. Waits for a
    /// maximum duration of `timeout`.
    ///
    /// Returns `None` if no events were available within `timeout`.
    pub fn wait_event_timeout(&mut self, timeout: time::Duration) -> Option<Event> {
        let ref mut core = *self.core.borrow_mut();

        if core.events.is_empty() {
            let mut remaining_timeout = timeout;
            let mut wait_begin = time::Instant::now();

            loop {
                let wait_timeout = if let Some(timer_timeout) = core.next_timer_timeout() {
                    remaining_timeout.min(timer_timeout)
                } else {
                    remaining_timeout
                };

                core.handle_frames_wait(&mut self.socket_rx, Some(wait_timeout));

                core.handle_timeouts(&mut self.timer_data_buffer);

                if !core.events.is_empty() {
                    // Found what we're looking for
                    break;
                }

                let now = time::Instant::now();
                let elapsed_time = now - wait_begin;

                if elapsed_time >= remaining_timeout {
                    // No time left
                    break;
                }

                remaining_timeout -= elapsed_time;
                wait_begin = now;
            }
        }

        return core.events.pop_front();
    }

    pub fn local_addr(&self) -> net::SocketAddr {
        self.socket_rx.local_addr()
    }

    pub fn peer_count(&self) -> usize {
        let ref core = *self.core.borrow();

        core.peer_table.count().into()
    }
}

impl PeerHandle {
    fn new(peer: PeerRc) -> Self {
        Self { peer }
    }

    pub fn send(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        let ref mut peer = *self.peer.borrow_mut();

        if let Some(server_rc) = Weak::upgrade(&peer.server_weak) {
            let ref mut server = *server_rc.borrow_mut();

            let now_ms = server.time_now_ms();

            let ref mut ctx = EndpointContext::new(server, &mut peer.core, &self.peer);

            peer.endpoint.send(packet_bytes, mode, now_ms, ctx);
        }
    }

    pub fn flush(&mut self) {
        let ref mut peer = *self.peer.borrow_mut();

        if let Some(server_rc) = Weak::upgrade(&peer.server_weak) {
            let ref mut server = *server_rc.borrow_mut();

            let now_ms = server.time_now_ms();

            let ref mut ctx = EndpointContext::new(server, &mut peer.core, &self.peer);

            peer.endpoint.flush(now_ms, ctx);
        }
    }

    pub fn disconnect(&mut self) {
        let ref mut peer = *self.peer.borrow_mut();

        if let Some(server_rc) = Weak::upgrade(&peer.server_weak) {
            let ref mut server = *server_rc.borrow_mut();

            let now_ms = server.time_now_ms();

            let ref mut ctx = EndpointContext::new(server, &mut peer.core, &self.peer);

            peer.endpoint.disconnect(now_ms, ctx);
        }
    }

    pub fn id(&self) -> PeerId {
        self.peer.borrow().core.id
    }
}

impl std::fmt::Debug for PeerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeerHandle")
            .field("id", &self.id())
            .finish()
    }
}
