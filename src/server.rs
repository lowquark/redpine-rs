use std::any;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt;
use std::net;
use std::sync::{Arc, Mutex, RwLock};
use std::time;

use siphasher::sip;

use super::endpoint;
use super::frame;
use super::socket;
use super::timer_queue;
use super::ErrorKind;
use super::SendMode;

const FRAME_SIZE_MAX: usize = 1472;

const PEER_COUNT_MAX_DEFAULT: usize = 8;
const PEER_COUNT_MAX_MAX: usize = 65536;

const HANDSHAKE_TIMEOUT_MS: u32 = 10_000;

const CONNECTION_TIMEOUT_DEFAULT_MS: u64 = 10_000;
const CONNECTION_TIMEOUT_MIN_MS: u64 = 2_000;

const TIMER_TEST_LIMIT: usize = 24;
const TIMER_TEST_INTERVAL: time::Duration = time::Duration::from_millis(250);

/// Configuration for a [`Server`] object.
#[derive(Clone)]
pub struct Config {
    /// Maximum number of clients which may be connected at any given time.
    ///
    /// Minimum value: 1 \
    /// Maximum value: 65,536 \
    /// Default value: 8
    pub peer_count_max: usize,

    /// Timeout to use when once a connection has been established, in milliseconds.
    ///
    /// Minimum value: 2,000 \
    /// Default value: 10,000
    pub connection_timeout_ms: u64,

    /// Configuration for unreliable / reliable channel prioritization.
    pub channel_balance: endpoint::ChannelBalanceConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            peer_count_max: PEER_COUNT_MAX_DEFAULT,
            connection_timeout_ms: CONNECTION_TIMEOUT_DEFAULT_MS,
            channel_balance: Default::default(),
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

        self.channel_balance.validate();
    }
}

struct Epoch {
    time_base: time::Instant,
}

impl Epoch {
    fn new() -> Self {
        Self {
            time_base: time::Instant::now(),
        }
    }

    fn time_now_ms(&self) -> u64 {
        self.time_base.elapsed().as_millis() as u64
    }
}

struct PeerCore {
    // Remote address
    addr: net::SocketAddr,
    // Independent user data associated with this peer
    data: Option<Arc<dyn any::Any + Send + Sync>>,
    // Server epoch
    epoch: Arc<Epoch>,
    // Permits sending frames from a peer handle
    socket_tx: Arc<socket::SocketTx>,
    // Next RTO timer expiration time
    rto_timer: Option<u64>,
    // Permits signaling events from a peer handle
    events: Arc<Mutex<VecDeque<Event>>>,
}

struct Peer {
    // Peer metadata
    core: PeerCore,
    // Endpoint representing this peer's connection
    endpoint: endpoint::Endpoint,
}

type PeerRef = Arc<RwLock<Peer>>;

impl timer_queue::Timer for RwLock<Peer> {
    fn test(&self, time_now: u64) -> bool {
        if let Ok(peer) = &mut self.write() {
            if let Some(t) = &peer.core.rto_timer {
                if *t < time_now {
                    peer.core.rto_timer = None;
                    return true;
                }
            }
        }
        false
    }
}

type TimerQueue = timer_queue::TimerQueue<RwLock<Peer>>;

/// Represents a server event.
#[derive(Debug)]
pub enum Event {
    /// Produced when a new client has connected.
    Connect(PeerHandle),
    /// Produced when a connection terminates gracefully.
    Disconnect(PeerHandle),
    /// Produced when a packet has been received.
    Receive(PeerHandle, Box<[u8]>),
    /// Produced in response to a fatal connection error.
    Error(PeerHandle, ErrorKind),
}

struct EndpointContext<'a> {
    peer: &'a mut PeerCore,
    peer_ref: &'a PeerRef,
}

struct ServerCore {
    // Saved configuration
    config: Config,
    // Source of integer timestamps
    epoch: Arc<Epoch>,
    // Socket send handle
    socket_tx: Arc<socket::SocketTx>,
    // Key used to compute handshake MACs
    siphash_key: [u8; 16],
    // Table of connected peers
    peers: HashMap<net::SocketAddr, PeerRef>,
    // Pending timer events
    timer_queue: TimerQueue,
    // Queue of pending events
    events: Arc<Mutex<VecDeque<Event>>>,
}

/// A Redpine server.
pub struct Server {
    // Interesting server data
    core: ServerCore,
    // Socket receive handle
    socket_rx: socket::SocketRx,
    // Always-allocated timer expiration buffer
    timer_data_buffer: Vec<PeerRef>,
}

/// Represents a connected client.
#[derive(Clone)]
pub struct PeerHandle {
    peer: PeerRef,
}

impl Peer {
    fn new(
        addr: net::SocketAddr,
        endpoint_config: endpoint::Config,
        epoch: Arc<Epoch>,
        socket_tx: Arc<socket::SocketTx>,
        events: Arc<Mutex<VecDeque<Event>>>,
    ) -> Self {
        Self {
            core: PeerCore {
                addr,
                data: None,
                epoch,
                socket_tx,
                events,
                rto_timer: None,
            },
            endpoint: endpoint::Endpoint::new(endpoint_config),
        }
    }
}

impl<'a> EndpointContext<'a> {
    fn new(peer: &'a mut PeerCore, peer_ref: &'a PeerRef) -> Self {
        Self { peer, peer_ref }
    }
}

impl<'a> endpoint::HostContext for EndpointContext<'a> {
    fn send_frame(&mut self, frame_bytes: &[u8]) {
        // println!("{:?} <- {:02X?}", self.peer.addr, frame_bytes);
        self.peer.socket_tx.send(frame_bytes, &self.peer.addr);
    }

    fn set_rto_timer(&mut self, time_ms: u64) {
        self.peer.rto_timer = Some(time_ms);
    }

    fn unset_rto_timer(&mut self) {
        self.peer.rto_timer = None;
    }

    fn on_connect(&mut self) {
        let handle = PeerHandle::new(Arc::clone(&self.peer_ref));
        let event = Event::Connect(handle);
        self.peer.events.lock().unwrap().push_back(event);
    }

    fn on_disconnect(&mut self) {
        let handle = PeerHandle::new(Arc::clone(&self.peer_ref));
        let event = Event::Disconnect(handle);
        self.peer.events.lock().unwrap().push_back(event);
    }

    fn on_receive(&mut self, packet_bytes: Box<[u8]>) {
        let handle = PeerHandle::new(Arc::clone(&self.peer_ref));
        let event = Event::Receive(handle, packet_bytes);
        self.peer.events.lock().unwrap().push_back(event);
    }

    fn on_timeout(&mut self) {
        let handle = PeerHandle::new(Arc::clone(&self.peer_ref));
        let event = Event::Error(handle, ErrorKind::Timeout);
        self.peer.events.lock().unwrap().push_back(event);
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
    fn step_timer_wheel(&mut self, timer_data_buffer: &mut Vec<PeerRef>) -> u64 {
        let now_ms = self.epoch.time_now_ms();

        self.timer_queue.test(TIMER_TEST_LIMIT, now_ms, |peer| {
            timer_data_buffer.push(Arc::clone(peer))
        });

        return now_ms;
    }

    /// Returns a duration representing the time until the next timer event, if one exists.
    pub fn next_timer_timeout(&self) -> Option<time::Duration> {
        if self.timer_queue.len() > 0 {
            Some(TIMER_TEST_INTERVAL)
        } else {
            None
        }
    }

    /// Processes all pending timer events.
    pub fn handle_timeouts(&mut self, timer_data_buffer: &mut Vec<PeerRef>) {
        let now_ms = self.step_timer_wheel(timer_data_buffer);

        for peer_ref in timer_data_buffer.drain(..) {
            let ref mut peer = *peer_ref.write().unwrap();

            let ref mut ctx = EndpointContext::new(&mut peer.core, &peer_ref);

            match peer.endpoint.handle_rto_timer(now_ms, ctx) {
                endpoint::TimeoutAction::Continue => (),
                endpoint::TimeoutAction::Terminate => {
                    self.peers.remove(&peer.core.addr);
                }
            }
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

                let error = if self.peers.contains_key(sender_addr) {
                    None
                } else if self.peers.len() >= self.config.peer_count_max {
                    Some(frame::HandshakeErrorKind::Capacity)
                } else if !params_compatible {
                    Some(frame::HandshakeErrorKind::Parameter)
                } else {
                    let endpoint_config = endpoint::Config {
                        timeout_time_ms: self.config.connection_timeout_ms,
                        prio_config: self.config.channel_balance.clone().into(),
                    };

                    // Create new peer object
                    let peer = Peer::new(
                        sender_addr.clone(),
                        endpoint_config,
                        Arc::clone(&self.epoch),
                        Arc::clone(&self.socket_tx),
                        Arc::clone(&self.events),
                    );

                    let peer_ref = Arc::new(RwLock::new(peer));

                    // Associate with given address
                    self.peers
                        .insert(sender_addr.clone(), Arc::clone(&peer_ref));

                    // Add to the timer queue
                    self.timer_queue.add_timer(Arc::downgrade(&peer_ref));

                    // Initialize endpoint

                    let now_ms = self.epoch.time_now_ms();

                    let ref mut peer = *peer_ref.write().unwrap();

                    let ref mut endpoint = peer.endpoint;
                    let ref mut peer_core = peer.core;

                    let ref mut ctx = EndpointContext::new(peer_core, &peer_ref);

                    endpoint.init(now_ms, ctx);

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
        if let Some(peer_ref) = self.peers.get(sender_addr) {
            if !frame::serial::verify_crc(frame_bytes) {
                return;
            }

            let payload_bytes = frame::serial::payload(frame_bytes);

            let peer_ref = Arc::clone(&peer_ref);

            let ref mut peer = *peer_ref.write().unwrap();

            let ref mut endpoint = peer.endpoint;
            let ref mut peer_core = peer.core;

            let ref mut ctx = EndpointContext::new(peer_core, &peer_ref);

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

            let now_ms = self.epoch.time_now_ms();

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
    /// Equivalent to calling [`Server::bind_with_config`] with default configuration.
    pub fn bind<A>(bind_addr: A) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        Self::bind_with_config(bind_addr, Default::default())
    }

    /// Binds a UDP socket at the provided address, and returns a new server object. Errors
    /// encountered during socket initialization are forwarded to the caller.
    pub fn bind_with_config<A>(bind_addr: A, config: Config) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        config.validate();

        let epoch = Epoch::new();

        let (socket_tx, socket_rx) = socket::new(bind_addr, FRAME_SIZE_MAX)?;

        // This samples the thread-local RNG, which is a CSPRNG (see docs for rand::rngs::StdRng)
        let siphash_key = (0..16)
            .map(|_| rand::random::<u8>())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();

        // In order to do interesting things with Peer objects, PeerHandles require Peers to keep a
        // (weak) pointer to the ServerCore object. The most convenient way to accomplish this is
        // to give ServerCore a weak pointer to itself that can be cloned upon Peer construction.
        // The alternative is to pass pointers to the ServerCore down the frame handling call
        // stack, which is cumbersome.
        let core = ServerCore {
            config,
            epoch: Arc::new(epoch),
            socket_tx: Arc::new(socket_tx),
            siphash_key,
            peers: Default::default(),
            timer_queue: Default::default(),
            events: Default::default(),
        };

        Ok(Self {
            core,
            socket_rx,
            timer_data_buffer: Vec::new(),
        })
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts in an attempt to produce an event.
    ///
    /// Returns `None` if no events are available.
    pub fn poll_event(&mut self) -> Option<Event> {
        let ref mut core = self.core;

        if core.events.lock().unwrap().is_empty() {
            core.handle_frames(&mut self.socket_rx);

            core.handle_timeouts(&mut self.timer_data_buffer);
        }

        return core.events.lock().unwrap().pop_front();
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts until an event can be returned.
    pub fn wait_event(&mut self) -> Event {
        let ref mut core = self.core;

        loop {
            let wait_timeout = core.next_timer_timeout();

            core.handle_frames_wait(&mut self.socket_rx, wait_timeout);

            core.handle_timeouts(&mut self.timer_data_buffer);

            if let Some(event) = core.events.lock().unwrap().pop_front() {
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
        let ref mut core = self.core;

        if core.events.lock().unwrap().is_empty() {
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

                if !core.events.lock().unwrap().is_empty() {
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

        return core.events.lock().unwrap().pop_front();
    }

    /// Returns the local address of the internal UDP socket.
    pub fn local_addr(&self) -> net::SocketAddr {
        self.socket_rx.local_addr()
    }

    /// Returns the number of peers in the peer table.
    ///
    /// *Note*: Peers may exist in the peer table for some time after they have disconnected.
    pub fn peer_count(&self) -> usize {
        self.core.peers.len()
    }
}

impl PeerHandle {
    fn new(peer: PeerRef) -> Self {
        Self { peer }
    }

    /// Returns the user data associated with this peer.
    pub fn data(&self) -> Option<Arc<dyn any::Any + Send + Sync>> {
        self.peer.read().unwrap().core.data.clone()
    }

    /// Associates user data with this peer.
    pub fn set_data(&self, data: Option<Arc<dyn any::Any + Send + Sync>>) {
        self.peer.write().unwrap().core.data = data;
    }

    /// Equivalent to calling [`PeerHandle::enqueue`] followed by [`PeerHandle::flush`].
    ///
    /// *Note*: When sending many packets, it is more efficient to call `enqueue` multiple times
    /// followed by a final call to `flush`.
    pub fn send(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        let ref mut peer = *self.peer.write().unwrap();

        let now_ms = peer.core.epoch.time_now_ms();

        let ref mut ctx = EndpointContext::new(&mut peer.core, &self.peer);

        peer.endpoint.enqueue(packet_bytes, mode, now_ms);
        peer.endpoint.flush(now_ms, ctx);
    }

    /// Enqueues a packet to be sent to this peer.
    pub fn enqueue(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        let ref mut peer = *self.peer.write().unwrap();

        let now_ms = peer.core.epoch.time_now_ms();

        peer.endpoint.enqueue(packet_bytes, mode, now_ms);
    }

    /// Sends as much data as possible for this peer on the underlying socket.
    pub fn flush(&mut self) {
        let ref mut peer = *self.peer.write().unwrap();

        let now_ms = peer.core.epoch.time_now_ms();

        let ref mut ctx = EndpointContext::new(&mut peer.core, &self.peer);

        peer.endpoint.flush(now_ms, ctx);
    }

    /// Disconnects this peer gracefully. No more packets will be sent or received once this
    /// function has been called.
    pub fn disconnect(&mut self) {
        let ref mut peer = *self.peer.write().unwrap();

        let now_ms = peer.core.epoch.time_now_ms();

        let ref mut ctx = EndpointContext::new(&mut peer.core, &self.peer);

        peer.endpoint.disconnect(now_ms, ctx);
    }
}

impl std::cmp::PartialEq for PeerHandle {
    fn eq(&self, other: &PeerHandle) -> bool {
        Arc::ptr_eq(&self.peer, &other.peer)
    }
}

impl std::cmp::Eq for PeerHandle {}

impl std::fmt::Debug for PeerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeerHandle").finish()
    }
}
