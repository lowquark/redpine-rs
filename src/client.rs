use std::cell::RefCell;
use std::collections::VecDeque;
use std::net;
use std::rc::Rc;
use std::time;

use super::endpoint;
use super::frame;
use super::socket;
use super::SendMode;

const FRAME_SIZE_MAX: usize = 1478;

const HANDSHAKE_TIMEOUT_DEFAULT_MS: u64 = 10_000;
const HANDSHAKE_TIMEOUT_MIN_MS: u64 = 2_000;

const HANDSHAKE_RESEND_TIMEOUT_MS: u64 = 2_000;

const CONNECTION_TIMEOUT_DEFAULT_MS: u64 = 10_000;
const CONNECTION_TIMEOUT_MIN_MS: u64 = 2_000;

#[derive(Clone)]
pub struct Config {
    pub handshake_timeout_ms: u64,
    pub connection_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            handshake_timeout_ms: HANDSHAKE_TIMEOUT_DEFAULT_MS,
            connection_timeout_ms: CONNECTION_TIMEOUT_DEFAULT_MS,
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
    }
}

type EndpointRc = Rc<RefCell<endpoint::Endpoint>>;

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
    endpoint_rc: EndpointRc,
}

enum State {
    Handshake(HandshakeState),
    Active(ActiveState),
    Quiescent,
}

struct Timer {
    timeout_ms: Option<u64>,
}

struct Timers {
    rto_timer: Timer,
    recv_timer: Timer,
}

pub enum Event {
    Connect,
    Disconnect,
    Receive(Box<[u8]>),
    Timeout,
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
    // Pending timer events
    timers: Timers,
    // Queue of events
    events: VecDeque<Event>,
}

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

    fn set_timer(&mut self, timer: endpoint::TimerName, time_ms: u64) {
        match timer {
            endpoint::TimerName::Rto => {
                self.client.timers.rto_timer.timeout_ms = Some(time_ms);
            }
            endpoint::TimerName::Receive => {
                self.client.timers.recv_timer.timeout_ms = Some(time_ms);
            }
        }
    }

    fn unset_timer(&mut self, timer: endpoint::TimerName) {
        match timer {
            endpoint::TimerName::Rto => {
                self.client.timers.rto_timer.timeout_ms = None;
            }
            endpoint::TimerName::Receive => {
                self.client.timers.recv_timer.timeout_ms = None;
            }
        }
    }

    fn destroy_self(&mut self) {
        self.client.state = State::Quiescent;
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
        self.client.events.push_back(Event::Timeout);
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

        let mut timeout_ms = None;

        for timer in [&self.timers.rto_timer, &self.timers.recv_timer] {
            if let Some(timer_timeout_ms) = timer.timeout_ms {
                if let Some(t_ms) = timeout_ms {
                    if timer_timeout_ms < t_ms {
                        timeout_ms = Some(timer_timeout_ms);
                    }
                } else {
                    timeout_ms = Some(timer_timeout_ms);
                }
            }
        }

        if let Some(t_ms) = timeout_ms {
            return Some(time::Duration::from_millis(t_ms - now_ms));
        }

        return None;
    }

    fn process_timeout(&mut self, timer_id: endpoint::TimerName, now_ms: u64) {
        match &mut self.state {
            State::Handshake(state) => {
                match timer_id {
                    endpoint::TimerName::Rto => {
                        if now_ms >= state.timeout_time_ms {
                            // Connection failed
                            self.events.push_back(Event::Timeout);
                            self.state = State::Quiescent;
                        } else {
                            self.timers.rto_timer.timeout_ms =
                                Some(now_ms + HANDSHAKE_RESEND_TIMEOUT_MS);

                            match state.phase {
                                HandshakePhase::Alpha => {
                                    println!("re-requesting phase α...");
                                    self.socket_tx.send(&state.frame);
                                }
                                HandshakePhase::Beta => {
                                    println!("re-requesting phase β...");
                                    self.socket_tx.send(&state.frame);
                                }
                            }
                        }
                    }
                    endpoint::TimerName::Receive => (),
                }
            }
            State::Active(state) => {
                let endpoint_rc = Rc::clone(&state.endpoint_rc);

                let ref mut host_ctx = EndpointContext::new(self);

                endpoint_rc
                    .borrow_mut()
                    .handle_timer(timer_id, now_ms, host_ctx);
            }
            State::Quiescent => {}
        }
    }

    pub fn process_timeouts(&mut self) {
        let now_ms = self.time_now_ms();

        let timer_ids = [endpoint::TimerName::Rto, endpoint::TimerName::Receive];

        for timer_id in timer_ids {
            let timer = match timer_id {
                endpoint::TimerName::Rto => &mut self.timers.rto_timer,
                endpoint::TimerName::Receive => &mut self.timers.recv_timer,
            };

            if let Some(timeout_ms) = timer.timeout_ms {
                if now_ms >= timeout_ms {
                    timer.timeout_ms = None;

                    self.process_timeout(timer_id, now_ms);
                }
            }
        }
    }

    fn handle_handshake_alpha_ack(&mut self, payload_bytes: &[u8], now_ms: u64) {
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
                            self.timers.rto_timer.timeout_ms =
                                Some(now_ms + HANDSHAKE_RESEND_TIMEOUT_MS);

                            use frame::serial::SimpleFrameWrite;
                            state.frame = frame::HandshakeBetaFrame {
                                client_params,
                                client_nonce: frame.client_nonce,
                                server_nonce: frame.server_nonce,
                                server_timestamp: frame.server_timestamp,
                                server_mac: frame.server_mac,
                            }
                            .write_boxed();

                            println!("requesting phase β...");
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
                            // Reset any previous timers
                            self.timers.rto_timer.timeout_ms = None;
                            self.timers.recv_timer.timeout_ms = None;

                            // Keep queued up packets to be sent later
                            let packet_buffer = std::mem::take(&mut state.packet_buffer);

                            // Create an endpoint now that we're connected
                            let endpoint_config = endpoint::Config {
                                timeout_time_ms: self.config.connection_timeout_ms,
                            };

                            let endpoint = endpoint::Endpoint::new(endpoint_config);
                            let endpoint_rc = Rc::new(RefCell::new(endpoint));

                            // Switch to active state
                            self.state = State::Active(ActiveState {
                                endpoint_rc: Rc::clone(&endpoint_rc),
                            });

                            // Initialize endpoint
                            let mut endpoint = endpoint_rc.borrow_mut();

                            let ref mut host_ctx = EndpointContext::new(self);

                            endpoint.init(now_ms, host_ctx);

                            for (packet, mode) in packet_buffer.into_iter() {
                                endpoint.send(packet, mode, now_ms, host_ctx);
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
                let endpoint_rc = Rc::clone(&state.endpoint_rc);

                let ref mut host_ctx = EndpointContext::new(self);

                endpoint_rc
                    .borrow_mut()
                    .handle_frame(frame_type, payload_bytes, now_ms, host_ctx);
            }
            _ => (),
        }
    }

    fn handle_frame(&mut self, frame_bytes: &[u8]) {
        // println!(" -> {:02X?}", frame_bytes);

        if !frame::serial::check_size(frame_bytes) {
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
                let endpoint_rc = Rc::clone(&state.endpoint_rc);

                let now_ms = self.time_now_ms();

                let ref mut host_ctx = EndpointContext::new(self);

                endpoint_rc
                    .borrow_mut()
                    .send(packet_bytes, mode, now_ms, host_ctx);
            }
            State::Quiescent => {}
        }
    }

    pub fn flush(&mut self) {
        match &mut self.state {
            State::Handshake(_) => {}
            State::Active(state) => {
                let endpoint_rc = Rc::clone(&state.endpoint_rc);

                let now_ms = self.time_now_ms();

                let ref mut host_ctx = EndpointContext::new(self);

                endpoint_rc.borrow_mut().flush(now_ms, host_ctx);
            }
            State::Quiescent => {}
        }
    }

    pub fn disconnect(&mut self) {
        match &mut self.state {
            State::Handshake(_) => {}
            State::Active(state) => {
                let endpoint_rc = Rc::clone(&state.endpoint_rc);

                let now_ms = self.time_now_ms();

                let ref mut host_ctx = EndpointContext::new(self);

                endpoint_rc.borrow_mut().disconnect(now_ms, host_ctx);
            }
            State::Quiescent => {}
        }
    }
}

impl Client {
    pub fn connect<A>(server_addr: A) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        Self::connect_with_config(server_addr, Default::default())
    }

    pub fn connect_with_config<A>(server_addr: A, config: Config) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        config.validate();

        let bind_address = (std::net::Ipv4Addr::UNSPECIFIED, 0);

        let (mut socket_tx, socket_rx) =
            socket::new_connected(bind_address, server_addr, FRAME_SIZE_MAX)?;

        let local_nonce = rand::random::<u32>();

        use frame::serial::SimpleFrameWrite;

        let handshake_frame = frame::HandshakeAlphaFrame {
            protocol_id: frame::serial::PROTOCOL_ID,
            client_nonce: local_nonce,
        }
        .write_boxed();

        println!("requesting phase α...");
        socket_tx.send(&handshake_frame);

        let state = State::Handshake(HandshakeState {
            phase: HandshakePhase::Alpha,
            timeout_time_ms: config.handshake_timeout_ms,
            timeout_ms: config.handshake_timeout_ms,
            packet_buffer: Vec::new(),
            frame: handshake_frame,
            local_nonce,
        });

        let timers = Timers {
            rto_timer: Timer {
                timeout_ms: Some(HANDSHAKE_RESEND_TIMEOUT_MS),
            },
            recv_timer: Timer { timeout_ms: None },
        };

        let core = ClientCore {
            config,
            time_ref: time::Instant::now(),
            socket_tx,
            state,
            timers,
            events: VecDeque::new(),
        };

        Ok(Self { core, socket_rx })
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts in an attempt to produce an event.
    ///
    /// Returns `None` if no events are available, or if an error was encountered while reading
    /// from the internal socket.
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

    pub fn send(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        self.core.send(packet_bytes, mode);
    }

    pub fn flush(&mut self) {
        self.core.flush();
    }

    pub fn disconnect(&mut self) {
        self.core.disconnect();
    }

    pub fn local_addr(&self) -> net::SocketAddr {
        self.socket_rx.local_addr()
    }

    pub fn server_addr(&self) -> net::SocketAddr {
        self.socket_rx.peer_addr()
    }
}
