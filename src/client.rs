use std::collections::VecDeque;
use std::net;
use std::sync::{Arc, RwLock};
use std::time;

use super::endpoint;
use super::frame;
use super::socket;
use super::ErrorKind;
use super::SendMode;

const FRAME_SIZE_MAX: usize = 1472;

const HANDSHAKE_TIMEOUT_DEFAULT_MS: u64 = 10_000;
const HANDSHAKE_TIMEOUT_MIN_MS: u64 = 2_000;

const HANDSHAKE_RESEND_TIMEOUT_MS: u64 = 2_000;

const CONNECTION_TIMEOUT_DEFAULT_MS: u64 = 10_000;
const CONNECTION_TIMEOUT_MIN_MS: u64 = 2_000;

/// Configuration for a [`Client`] object.
#[derive(Clone)]
pub struct Config {
    /// Timeout to use when initiating a connection, in milliseconds.
    ///
    /// Minimum value: 2,000 \
    /// Default value: 10,000
    pub handshake_timeout_ms: u64,

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
            handshake_timeout_ms: HANDSHAKE_TIMEOUT_DEFAULT_MS,
            connection_timeout_ms: CONNECTION_TIMEOUT_DEFAULT_MS,
            channel_balance: Default::default(),
        }
    }
}

impl Config {
    fn validate(&self) {
        assert!(
            self.handshake_timeout_ms >= HANDSHAKE_TIMEOUT_MIN_MS,
            "invalid client configuration: handshake_timeout_ms < {}",
            HANDSHAKE_TIMEOUT_MIN_MS
        );
        assert!(
            self.connection_timeout_ms >= CONNECTION_TIMEOUT_MIN_MS,
            "invalid client configuration: connection_timeout_ms < {}",
            CONNECTION_TIMEOUT_MIN_MS
        );

        self.channel_balance.validate();
    }
}

type EndpointRef = Arc<RwLock<endpoint::Endpoint>>;

enum HandshakePhase {
    Alpha,
    Beta,
}

struct HandshakeState {
    phase: HandshakePhase,
    timeout_time_ms: u64,
    timeout_ms: u64,
    packet_buffer: Vec<(Box<[u8]>, SendMode)>,
    frame: Box<[u8]>,
    local_nonce: u32,
}

struct ActiveState {
    endpoint_ref: EndpointRef,
}

enum State {
    Handshake(HandshakeState),
    Active(ActiveState),
    Quiescent,
}

struct Timer {
    timeout_ms: Option<u64>,
}

/// Represents a client event.
#[derive(Debug)]
pub enum Event {
    /// Produced when a connection to the server has been established.
    Connect,
    /// Produced when the connection terminates gracefully.
    Disconnect,
    /// Produced when a packet has been received.
    Receive(Box<[u8]>),
    /// Produced in response to a fatal connection error.
    Error(ErrorKind),
}

struct EndpointContext<'a> {
    client: &'a mut ClientCore,
}

struct ClientCore {
    // Saved configuration
    config: Config,
    // Timestamps are computed relative to this instant
    time_ref: time::Instant,
    // Socket send handle
    socket_tx: socket::ConnectedSocketTx,
    // Current mode of operation
    state: State,
    // Pending RTO timer event
    rto_timer: Timer,
    // Queue of events
    events: VecDeque<Event>,
}

/// A Redpine client connection.
pub struct Client {
    // Interesting client data
    core: ClientCore,
    // Socket receive handle
    socket_rx: socket::ConnectedSocketRx,
}

impl<'a> EndpointContext<'a> {
    fn new(client: &'a mut ClientCore) -> Self {
        Self { client }
    }
}

impl<'a> endpoint::HostContext for EndpointContext<'a> {
    fn send_frame(&mut self, frame_bytes: &[u8]) {
        // println!(" <- {:02X?}", frame_bytes);
        self.client.socket_tx.send(frame_bytes);
    }

    fn set_rto_timer(&mut self, time_ms: u64) {
        self.client.rto_timer.timeout_ms = Some(time_ms);
    }

    fn unset_rto_timer(&mut self) {
        self.client.rto_timer.timeout_ms = None;
    }

    fn on_connect(&mut self) {
        self.client.events.push_back(Event::Connect);
    }

    fn on_disconnect(&mut self) {
        self.client.events.push_back(Event::Disconnect);
    }

    fn on_receive(&mut self, packet_bytes: Box<[u8]>) {
        self.client.events.push_back(Event::Receive(packet_bytes))
    }

    fn on_timeout(&mut self) {
        self.client
            .events
            .push_back(Event::Error(ErrorKind::Timeout));
    }
}

fn connection_params_compatible(a: &frame::ConnectionParams, b: &frame::ConnectionParams) -> bool {
    a.packet_size_in_max >= b.packet_size_out_max && b.packet_size_in_max >= a.packet_size_out_max
}

impl ClientCore {
    /// Returns the number of whole milliseconds elapsed since the client object was created.
    fn time_now_ms(&self) -> u64 {
        (time::Instant::now() - self.time_ref).as_millis() as u64
    }

    /// Returns the time remaining until the next timer expires.
    pub fn next_timer_timeout(&self) -> Option<time::Duration> {
        let now_ms = self.time_now_ms();

        if let Some(t_ms) = self.rto_timer.timeout_ms {
            let remaining_ms = if t_ms >= now_ms { t_ms - now_ms } else { 0 };

            return Some(time::Duration::from_millis(remaining_ms));
        }

        return None;
    }

    fn process_timeout(&mut self, now_ms: u64) {
        match &mut self.state {
            State::Handshake(state) => {
                if now_ms >= state.timeout_time_ms {
                    // Connection failed
                    self.events.push_back(Event::Error(ErrorKind::Timeout));
                    self.state = State::Quiescent;
                } else {
                    self.rto_timer.timeout_ms = Some(now_ms + HANDSHAKE_RESEND_TIMEOUT_MS);

                    match state.phase {
                        HandshakePhase::Alpha => {
                            // println!("re-requesting phase α...");
                            self.socket_tx.send(&state.frame);
                        }
                        HandshakePhase::Beta => {
                            // println!("re-requesting phase β...");
                            self.socket_tx.send(&state.frame);
                        }
                    }
                }
            }
            State::Active(state) => {
                let endpoint_ref = Arc::clone(&state.endpoint_ref);
                let mut endpoint = endpoint_ref.write().unwrap();

                let ref mut host_ctx = EndpointContext::new(self);

                match endpoint.handle_rto_timer(now_ms, host_ctx) {
                    endpoint::TimeoutAction::Continue => (),
                    endpoint::TimeoutAction::Terminate => {
                        self.state = State::Quiescent;
                    }
                }
            }
            State::Quiescent => {}
        }
    }

    pub fn process_timeouts(&mut self) {
        let now_ms = self.time_now_ms();

        let ref mut timer = self.rto_timer;

        if let Some(timeout_ms) = timer.timeout_ms {
            if now_ms >= timeout_ms {
                timer.timeout_ms = None;

                self.process_timeout(now_ms);
            }
        }
    }

    fn handle_handshake_alpha_ack(&mut self, payload_bytes: &[u8], now_ms: u64) {
        use frame::serial::SimpleFrameWrite;
        use frame::serial::SimplePayloadRead;

        match self.state {
            State::Handshake(ref mut state) => match state.phase {
                HandshakePhase::Alpha => {
                    if let Some(frame) = frame::HandshakeAlphaAckFrame::read(payload_bytes) {
                        let client_params = frame::ConnectionParams {
                            packet_size_in_max: u32::max_value(),
                            packet_size_out_max: u32::max_value(),
                        };

                        let nonce_valid = frame.client_nonce == state.local_nonce;
                        let params_compatible =
                            connection_params_compatible(&client_params, &frame.server_params);

                        if nonce_valid && params_compatible {
                            state.phase = HandshakePhase::Beta;
                            state.timeout_time_ms = now_ms + state.timeout_ms;
                            self.rto_timer.timeout_ms = Some(now_ms + HANDSHAKE_RESEND_TIMEOUT_MS);

                            state.frame = frame::HandshakeBetaFrame {
                                client_params,
                                client_nonce: frame.client_nonce,
                                server_nonce: frame.server_nonce,
                                server_timestamp: frame.server_timestamp,
                                server_mac: frame.server_mac,
                            }
                            .write_boxed();

                            // println!("requesting phase β...");
                            self.socket_tx.send(&state.frame);
                        }
                    }
                }
                _ => (),
            },
            _ => (),
        }
    }

    fn handle_handshake_beta_ack(&mut self, payload_bytes: &[u8], now_ms: u64) {
        use frame::serial::SimplePayloadRead;

        match self.state {
            State::Handshake(ref mut state) => match state.phase {
                HandshakePhase::Beta => {
                    if let Some(frame) = frame::HandshakeBetaAckFrame::read(payload_bytes) {
                        if frame.client_nonce == state.local_nonce {
                            match frame.error {
                                None => {
                                    // Reset previously used timer
                                    self.rto_timer.timeout_ms = None;

                                    // Keep queued up packets to be sent later
                                    let packet_buffer = std::mem::take(&mut state.packet_buffer);

                                    // Create an endpoint now that we're connected
                                    let endpoint_config = endpoint::Config {
                                        timeout_time_ms: self.config.connection_timeout_ms,
                                        prio_config: self.config.channel_balance.clone().into(),
                                    };

                                    let endpoint = endpoint::Endpoint::new(endpoint_config);
                                    let endpoint_ref = Arc::new(RwLock::new(endpoint));

                                    // Switch to active state
                                    self.state = State::Active(ActiveState {
                                        endpoint_ref: Arc::clone(&endpoint_ref),
                                    });

                                    // Initialize endpoint
                                    let mut endpoint = endpoint_ref.write().unwrap();

                                    let ref mut host_ctx = EndpointContext::new(self);

                                    endpoint.init(now_ms, host_ctx);

                                    for (packet, mode) in packet_buffer.into_iter() {
                                        endpoint.enqueue(packet, mode, now_ms);
                                    }

                                    endpoint.flush(now_ms, host_ctx);
                                }
                                Some(kind) => {
                                    // Connection failed
                                    let kind = match kind {
                                        frame::HandshakeErrorKind::Capacity => ErrorKind::Capacity,
                                        frame::HandshakeErrorKind::Parameter => {
                                            ErrorKind::Parameter
                                        }
                                    };
                                    self.events.push_back(Event::Error(kind));
                                    self.state = State::Quiescent;
                                }
                            }
                        }
                    }
                }
                _ => (),
            },
            _ => (),
        }
    }

    fn handle_frame_other(
        &mut self,
        frame_type: frame::FrameType,
        payload_bytes: &[u8],
        now_ms: u64,
    ) {
        match self.state {
            State::Active(ref mut state) => {
                let endpoint_ref = Arc::clone(&state.endpoint_ref);
                let mut endpoint = endpoint_ref.write().unwrap();

                let ref mut host_ctx = EndpointContext::new(self);

                endpoint.handle_frame(frame_type, payload_bytes, now_ms, host_ctx);
            }
            _ => (),
        }
    }

    fn handle_frame(&mut self, frame_bytes: &[u8]) {
        // println!(" -> {:02X?}", frame_bytes);

        if !frame::serial::verify_minimum_size(frame_bytes) {
            return;
        }

        if let Some(frame_type) = frame::serial::read_type(frame_bytes) {
            if !frame::serial::verify_crc(frame_bytes) {
                // We don't negotiate with entropy
                return;
            }

            let payload_bytes = frame::serial::payload(frame_bytes);

            let now_ms = self.time_now_ms();

            match frame_type {
                frame::FrameType::HandshakeAlphaAck => {
                    self.handle_handshake_alpha_ack(payload_bytes, now_ms);
                }
                frame::FrameType::HandshakeBetaAck => {
                    self.handle_handshake_beta_ack(payload_bytes, now_ms);
                }
                _ => {
                    self.handle_frame_other(frame_type, payload_bytes, now_ms);
                }
            }
        }
    }

    /// Reads and processes as many frames as possible from socket_rx without blocking.
    pub fn handle_frames(&mut self, socket_rx: &mut socket::ConnectedSocketRx) {
        while let Ok(Some(frame_bytes)) = socket_rx.try_read_frame() {
            // Process this frame
            self.handle_frame(frame_bytes);
        }
    }

    /// Reads and processes as many frames as possible from socket_rx, waiting up to
    /// `wait_timeout` for the first.
    pub fn handle_frames_wait(
        &mut self,
        socket_rx: &mut socket::ConnectedSocketRx,
        wait_timeout: Option<time::Duration>,
    ) {
        if let Ok(Some(frame_bytes)) = socket_rx.wait_for_frame(wait_timeout) {
            // Process this frame
            self.handle_frame(frame_bytes);
            // Process any further frames without blocking
            return self.handle_frames(socket_rx);
        }
    }

    pub fn send(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        match &mut self.state {
            State::Handshake(state) => {
                state.packet_buffer.push((packet_bytes, mode));
            }
            State::Active(state) => {
                let endpoint_ref = Arc::clone(&state.endpoint_ref);
                let mut endpoint = endpoint_ref.write().unwrap();

                let now_ms = self.time_now_ms();

                let ref mut host_ctx = EndpointContext::new(self);

                endpoint.enqueue(packet_bytes, mode, now_ms);
                endpoint.flush(now_ms, host_ctx);
            }
            State::Quiescent => {}
        }
    }

    pub fn enqueue(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        match &mut self.state {
            State::Handshake(state) => {
                state.packet_buffer.push((packet_bytes, mode));
            }
            State::Active(state) => {
                let endpoint_ref = Arc::clone(&state.endpoint_ref);
                let mut endpoint = endpoint_ref.write().unwrap();

                let now_ms = self.time_now_ms();

                endpoint.enqueue(packet_bytes, mode, now_ms);
            }
            State::Quiescent => {}
        }
    }

    pub fn flush(&mut self) {
        match &mut self.state {
            State::Handshake(_) => {}
            State::Active(state) => {
                let endpoint_ref = Arc::clone(&state.endpoint_ref);
                let mut endpoint = endpoint_ref.write().unwrap();

                let now_ms = self.time_now_ms();

                let ref mut host_ctx = EndpointContext::new(self);

                endpoint.flush(now_ms, host_ctx);
            }
            State::Quiescent => {}
        }
    }

    pub fn disconnect(&mut self) {
        match &mut self.state {
            State::Handshake(_) => {}
            State::Active(state) => {
                let endpoint_ref = Arc::clone(&state.endpoint_ref);
                let mut endpoint = endpoint_ref.write().unwrap();

                let now_ms = self.time_now_ms();

                let ref mut host_ctx = EndpointContext::new(self);

                endpoint.disconnect(now_ms, host_ctx);
            }
            State::Quiescent => {}
        }
    }
}

impl Client {
    /// Equivalent to calling [`Client::connect_with_config`] with default configuration.
    pub fn connect<A>(server_addr: A) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        Self::connect_with_config(server_addr, Default::default())
    }

    /// Binds a UDP socket to an ephemeral address, initiates a connection to a server at the
    /// provided address, and returns a new client object. Errors encountered during socket
    /// initialization are forwarded to the caller.
    pub fn connect_with_config<A>(server_addr: A, config: Config) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        config.validate();

        let bind_address = (std::net::Ipv4Addr::UNSPECIFIED, 0);

        let (mut socket_tx, socket_rx) =
            socket::new_connected(bind_address, server_addr, FRAME_SIZE_MAX)?;

        let local_nonce = rand::random::<u32>();

        let handshake_frame = frame::HandshakeAlphaFrame {
            protocol_id: frame::serial::PROTOCOL_ID,
            client_nonce: local_nonce,
        };

        let handshake_frame =
            frame::serial::write_handshake_alpha(&handshake_frame, FRAME_SIZE_MAX);

        // println!("requesting phase α...");
        socket_tx.send(&handshake_frame);

        let state = State::Handshake(HandshakeState {
            phase: HandshakePhase::Alpha,
            timeout_time_ms: config.handshake_timeout_ms,
            timeout_ms: config.handshake_timeout_ms,
            packet_buffer: Vec::new(),
            frame: handshake_frame,
            local_nonce,
        });

        let rto_timer = Timer {
            timeout_ms: Some(HANDSHAKE_RESEND_TIMEOUT_MS),
        };

        let core = ClientCore {
            config,
            time_ref: time::Instant::now(),
            socket_tx,
            state,
            rto_timer,
            events: VecDeque::new(),
        };

        Ok(Self { core, socket_rx })
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts in an attempt to produce an event.
    ///
    /// Returns `None` if no events are available.
    pub fn poll_event(&mut self) -> Option<Event> {
        let ref mut core = self.core;

        if core.events.is_empty() {
            core.handle_frames(&mut self.socket_rx);

            core.process_timeouts();
        }

        return core.events.pop_front();
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts until an event can be returned.
    pub fn wait_event(&mut self) -> Event {
        let ref mut core = self.core;

        loop {
            let wait_timeout = core.next_timer_timeout();

            core.handle_frames_wait(&mut self.socket_rx, wait_timeout);

            core.process_timeouts();

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
        let ref mut core = self.core;

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

                core.process_timeouts();

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

    /// Equivalent to calling [`Client::enqueue`] followed by [`Client::flush`].
    pub fn send(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        self.core.send(packet_bytes, mode);
    }

    /// Enqueues a packet to be sent.
    pub fn enqueue(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        self.core.enqueue(packet_bytes, mode);
    }

    /// Sends as much data as possible on the underlying socket.
    pub fn flush(&mut self) {
        self.core.flush();
    }

    /// Disconnects gracefully. No more packets will be sent or received once this function has been
    /// called.
    pub fn disconnect(&mut self) {
        self.core.disconnect();
    }

    /// Returns the local address of the internal UDP socket.
    pub fn local_addr(&self) -> net::SocketAddr {
        self.socket_rx.local_addr()
    }

    /// Returns the server address for this connection.
    pub fn server_addr(&self) -> net::SocketAddr {
        self.socket_rx.peer_addr()
    }
}
