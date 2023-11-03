use std::cell::RefCell;
use std::collections::VecDeque;
use std::net;
use std::rc::Rc;
use std::time;

use super::endpoint;
use super::SendMode;

struct SocketReceiver {
    // Reference to non-blocking, connected, server socket
    socket: Rc<net::UdpSocket>,
    // Always-allocated receive buffer
    recv_buffer: Box<[u8]>,
    // Polling objects
    poller: polling::Poller,
    poller_events: polling::Events,
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
}

struct ActiveState {
    endpoint_rc: EndpointRc,
}

enum State {
    Handshake(HandshakeState),
    Active(ActiveState),
    Closed,
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
    // Timestamps are computed relative to this instant
    time_ref: time::Instant,
    // Client socket
    socket: Rc<net::UdpSocket>,
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
    // Permits both blocking and non-blocking reads from a non-blocking socket
    receiver: SocketReceiver,
    // Cached from socket initialization
    local_addr: net::SocketAddr,
    peer_addr: net::SocketAddr,
}

const SOCKET_POLLING_KEY: usize = 0;

const SYN_TIMEOUT_MS: u64 = 2000;
const HANDSHAKE_TIMEOUT_MS: u64 = 10000;

impl SocketReceiver {
    fn new(socket: Rc<net::UdpSocket>, frame_size_max: usize) -> std::io::Result<Self> {
        let poller = polling::Poller::new()?;

        unsafe {
            poller.add(&socket, polling::Event::readable(SOCKET_POLLING_KEY))?;
        }

        Ok(Self {
            socket,
            recv_buffer: vec![0; frame_size_max].into_boxed_slice(),
            poller,
            poller_events: polling::Events::new(),
        })
    }

    /// If a valid frame can be read from the socket, returns the frame. Returns Ok(None)
    /// otherwise.
    pub fn try_read_frame<'a>(&'a mut self) -> std::io::Result<Option<&'a [u8]>> {
        match self.socket.recv(&mut self.recv_buffer) {
            Ok(frame_len) => {
                let ref frame_bytes = self.recv_buffer[..frame_len];
                Ok(Some(frame_bytes))
            }
            Err(err) => match err.kind() {
                // The only acceptable error is WouldBlock, indicating no packet
                std::io::ErrorKind::WouldBlock => Ok(None),
                _ => Err(err),
            },
        }
    }

    /// Blocks for a duration of up to `timeout` for an incoming frame and returns it. Returns
    /// Ok(None) if no frame could be read in the alloted time, or if polling awoke spuriously.
    pub fn wait_for_frame<'a>(
        &'a mut self,
        timeout: Option<time::Duration>,
    ) -> std::io::Result<Option<&'a [u8]>> {
        // Wait for a readable event (must be done prior to each wait() call)
        // TODO: Does this work if the socket is already readable?
        self.poller
            .modify(&*self.socket, polling::Event::readable(SOCKET_POLLING_KEY))?;

        self.poller_events.clear();

        let n = self.poller.wait(&mut self.poller_events, timeout)?;

        if n > 0 {
            // The socket is readable - read in confidence
            self.try_read_frame()
        } else {
            Ok(None)
        }
    }
}

impl<'a> EndpointContext<'a> {
    fn new(client: &'a mut ClientCore) -> Self {
        Self { client }
    }
}

impl<'a> endpoint::HostContext for EndpointContext<'a> {
    fn send_frame(&mut self, frame_bytes: &[u8]) {
        // println!(" <- {:02X?}", frame_bytes);
        let _ = self.client.socket.send(frame_bytes);
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

    fn on_connect(&mut self) {
        self.client.events.push_back(Event::Connect);
    }

    fn on_disconnect(&mut self) {
        self.client.events.push_back(Event::Disconnect);
        self.client.state = State::Closed;
    }

    fn on_receive(&mut self, packet_bytes: Box<[u8]>) {
        self.client.events.push_back(Event::Receive(packet_bytes))
    }

    fn on_timeout(&mut self) {
        self.client.events.push_back(Event::Timeout);
        self.client.state = State::Closed;
    }
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
                            self.state = State::Closed;
                        } else {
                            self.timers.rto_timer.timeout_ms = Some(now_ms + SYN_TIMEOUT_MS);

                            match state.phase {
                                HandshakePhase::Alpha => {
                                    println!("re-requesting phase α...");
                                    let _ = self.socket.send(&[0xA0]);
                                }
                                HandshakePhase::Beta => {
                                    println!("re-requesting phase β...");
                                    let _ = self.socket.send(&[0xB0]);
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
            State::Closed => {}
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

    fn handle_frame(&mut self, frame_bytes: &[u8]) {
        let now_ms = self.time_now_ms();

        // println!(" -> {:02X?}", frame_bytes);

        let crc_valid = true;

        if crc_valid {
            match self.state {
                State::Handshake(ref mut state) => match state.phase {
                    HandshakePhase::Alpha => {
                        if frame_bytes == [0xA1] {
                            state.phase = HandshakePhase::Beta;
                            state.timeout_time_ms = now_ms + state.timeout_ms;

                            println!("requesting phase β...");
                            let _ = self.socket.send(&[0xB0]);
                        }
                    }
                    HandshakePhase::Beta => {
                        if frame_bytes == [0xB1] {
                            // Reset any previous timers
                            self.timers.rto_timer.timeout_ms = None;
                            self.timers.recv_timer.timeout_ms = None;

                            // Keep queued up packets to be sent later
                            let packet_buffer = std::mem::take(&mut state.packet_buffer);

                            // Create an endpoint now that we're connected
                            let endpoint = endpoint::Endpoint::new();
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
                },
                State::Active(ref mut state) => {
                    let endpoint_rc = Rc::clone(&state.endpoint_rc);

                    let ref mut host_ctx = EndpointContext::new(self);

                    endpoint_rc
                        .borrow_mut()
                        .handle_frame(frame_bytes, now_ms, host_ctx);
                }
                State::Closed => {}
            }
        } else {
            // We don't negotiate with entropy
        }
    }

    /// Reads and processes as many frames as possible from receiver without blocking.
    pub fn handle_frames(&mut self, receiver: &mut SocketReceiver) {
        while let Ok(Some(frame_bytes)) = receiver.try_read_frame() {
            // Process this frame
            self.handle_frame(frame_bytes);
        }
    }

    /// Reads and processes as many frames as possible from receiver, waiting up to
    /// `wait_timeout` for the first.
    pub fn handle_frames_wait(
        &mut self,
        receiver: &mut SocketReceiver,
        wait_timeout: Option<time::Duration>,
    ) {
        if let Ok(Some(frame_bytes)) = receiver.wait_for_frame(wait_timeout) {
            // Process this frame
            self.handle_frame(frame_bytes);
            // Process any further frames without blocking
            return self.handle_frames(receiver);
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
            State::Closed => {}
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
            State::Closed => {}
        }
    }
}

impl Client {
    pub fn connect<A>(server_addr: A) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        let frame_size_max: usize = 1478;

        let bind_address = (std::net::Ipv4Addr::UNSPECIFIED, 0);

        let socket = net::UdpSocket::bind(bind_address)?;
        socket.set_nonblocking(true)?;
        socket.connect(server_addr)?;

        let local_addr = socket.local_addr()?;
        let peer_addr = socket.peer_addr()?;

        println!("requesting phase α...");
        let _ = socket.send(&[0xA0]);

        let socket_rc = Rc::new(socket);

        let receiver = SocketReceiver::new(Rc::clone(&socket_rc), frame_size_max)?;

        Ok(Self {
            core: ClientCore {
                time_ref: time::Instant::now(),
                socket: Rc::clone(&socket_rc),
                state: State::Handshake(HandshakeState {
                    phase: HandshakePhase::Alpha,
                    timeout_time_ms: HANDSHAKE_TIMEOUT_MS,
                    timeout_ms: HANDSHAKE_TIMEOUT_MS,
                    packet_buffer: Vec::new(),
                }),
                timers: Timers {
                    rto_timer: Timer {
                        timeout_ms: Some(SYN_TIMEOUT_MS),
                    },
                    recv_timer: Timer { timeout_ms: None },
                },
                events: VecDeque::new(),
            },
            receiver,
            local_addr,
            peer_addr,
        })
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts in an attempt to produce an event.
    ///
    /// Returns `None` if no events are available, or if an error was encountered while reading
    /// from the internal socket.
    pub fn poll_event(&mut self) -> Option<Event> {
        let ref mut core = self.core;

        if core.events.is_empty() {
            core.handle_frames(&mut self.receiver);

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

            core.handle_frames_wait(&mut self.receiver, wait_timeout);

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

                core.handle_frames_wait(&mut self.receiver, Some(wait_timeout));

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

    pub fn local_addr(&self) -> net::SocketAddr {
        self.local_addr
    }

    pub fn server_addr(&self) -> net::SocketAddr {
        self.peer_addr
    }
}
