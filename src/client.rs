use std::collections::VecDeque;
use std::net;
use std::time;

use super::endpoint;
use super::SendMode;

const SOCKET_POLLING_KEY: usize = 0;

const SYN_TIMEOUT_MS: u64 = 2000;
const HANDSHAKE_TIMEOUT_MS: u64 = 10000;

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
    endpoint: endpoint::Endpoint,
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
    rto_timer: Timer,  // No response from remote
    recv_timer: Timer, // When to skip a hole in the reorder buffer
}

struct HostContext<'a> {
    timers: &'a mut Timers,
    socket: &'a net::UdpSocket,
    events: &'a mut VecDeque<Event>,
}

pub enum Event {
    Connect,
    Disconnect,
    Receive(Box<[u8]>),
    Timeout,
}

pub struct Client {
    socket: net::UdpSocket,
    local_addr: net::SocketAddr,
    peer_addr: net::SocketAddr,

    poller: polling::Poller,
    poller_events: polling::Events,

    recv_buffer: Box<[u8]>,

    events: VecDeque<Event>,

    timers: Timers,

    time_ref: time::Instant,

    state: State,
}

impl<'a> endpoint::HostContext for HostContext<'a> {
    fn send(&mut self, frame_bytes: &[u8]) {
        let _ = self.socket.send(frame_bytes);
    }

    fn set_timer(&mut self, timer: endpoint::TimerName, time_ms: u64) {
        match timer {
            endpoint::TimerName::Rto => {
                self.timers.rto_timer.timeout_ms = Some(time_ms);
            }
            endpoint::TimerName::Receive => {
                self.timers.recv_timer.timeout_ms = Some(time_ms);
            }
        }
    }

    fn unset_timer(&mut self, timer: endpoint::TimerName) {
        match timer {
            endpoint::TimerName::Rto => {
                self.timers.rto_timer.timeout_ms = None;
            }
            endpoint::TimerName::Receive => {
                self.timers.recv_timer.timeout_ms = None;
            }
        }
    }

    fn on_connect(&mut self) {
        self.events.push_back(Event::Connect);
    }

    fn on_disconnect(&mut self) {
        self.events.push_back(Event::Disconnect);

        todo!();
        // self.state = State::Closed;
    }

    fn on_receive(&mut self, packet_bytes: Box<[u8]>) {
        self.events.push_back(Event::Receive(packet_bytes))
    }

    fn on_timeout(&mut self) {
        self.events.push_back(Event::Timeout);

        todo!();
        // self.state = State::Closed;
    }
}

impl Client {
    /// Returns the number of whole milliseconds elapsed since the client object was created.
    fn time_now_ms(&self) -> u64 {
        (time::Instant::now() - self.time_ref).as_millis() as u64
    }

    /// If a valid frame can be read from the socket, returns the frame. Returns Ok(None)
    /// otherwise.
    fn try_read_frame(&mut self) -> std::io::Result<Option<usize>> {
        match self.socket.recv(&mut self.recv_buffer) {
            Ok(frame_len) => Ok(Some(frame_len)),
            Err(err) => match err.kind() {
                // The only acceptable error is WouldBlock, indicating no packet
                std::io::ErrorKind::WouldBlock => Ok(None),
                _ => Err(err),
            },
        }
    }

    /// Blocks for a duration of up to `timeout` for an incoming frame and returns it. Returns
    /// Ok(None) if no frame could be read in the alloted time, or if polling awoke spuriously.
    fn wait_for_frame(
        &mut self,
        timeout: Option<time::Duration>,
    ) -> std::io::Result<Option<usize>> {
        // Wait for a readable event (must be done prior to each wait() call)
        // TODO: Does this work if the socket is already readable?
        self.poller
            .modify(&self.socket, polling::Event::readable(SOCKET_POLLING_KEY))?;

        self.poller_events.clear();

        let n = self.poller.wait(&mut self.poller_events, timeout)?;

        if n > 0 {
            // The socket is readable - read in confidence
            self.try_read_frame()
        } else {
            Ok(None)
        }
    }

    fn process_frame(&mut self, frame_size: usize) {
        let now_ms = self.time_now_ms();

        let frame_bytes = &self.recv_buffer[..frame_size];

        println!("received a packet of length {}", frame_bytes.len());
        println!("{:02X?}", frame_bytes);

        let crc_valid = true;

        if crc_valid {
            match &mut self.state {
                State::Handshake(state) => match state.phase {
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
                            let ref mut host_ctx = HostContext {
                                timers: &mut self.timers,
                                socket: &self.socket,
                                events: &mut self.events,
                            };

                            let mut endpoint = endpoint::Endpoint::new();

                            endpoint.init(now_ms, host_ctx);

                            for (packet, mode) in state.packet_buffer.drain(..) {
                                endpoint.send(packet, mode, host_ctx);
                            }

                            self.state = State::Active(ActiveState { endpoint });

                            self.timers.rto_timer.timeout_ms = None;
                            self.timers.recv_timer.timeout_ms = None;
                        }
                    }
                },
                State::Active(state) => {
                    let ref mut host_ctx = HostContext {
                        timers: &mut self.timers,
                        socket: &self.socket,
                        events: &mut self.events,
                    };

                    state.endpoint.handle_frame(frame_bytes, host_ctx);
                }
                State::Closed => {}
            }
        } else {
            // We don't negotiate with entropy
        }
    }

    /// Reads and processes as many frames as possible from the socket without blocking.
    fn process_available_frames(&mut self) {
        while let Ok(Some(frame_size)) = self.try_read_frame() {
            // Process this frame
            self.process_frame(frame_size);
        }
    }

    /// Reads and processes as many frames as possible from the socket, waiting up to
    /// `wait_timeout` for the first.
    fn process_available_frames_wait(&mut self, wait_timeout: Option<time::Duration>) {
        if let Ok(Some(frame_size)) = self.wait_for_frame(wait_timeout) {
            // Process this frame
            self.process_frame(frame_size);
            // Process any further frames without blocking
            return self.process_available_frames();
        }
    }

    /// Returns the time remaining until the next timer expires.
    fn next_timer_timeout(&self) -> Option<time::Duration> {
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
                let ref mut host_ctx = HostContext {
                    timers: &mut self.timers,
                    socket: &self.socket,
                    events: &mut self.events,
                };

                state.endpoint.handle_timer(timer_id, now_ms, host_ctx);
            }
            State::Closed => {}
        }
    }

    fn process_timeouts(&mut self) {
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

        let poller = polling::Poller::new()?;

        unsafe {
            poller.add(&socket, polling::Event::readable(SOCKET_POLLING_KEY))?;
        }

        println!("requesting phase α...");
        let _ = socket.send(&[0xA0]);

        Ok(Self {
            socket,
            local_addr,
            peer_addr,

            poller,
            poller_events: polling::Events::new(),

            recv_buffer: vec![0; frame_size_max].into_boxed_slice(),

            events: VecDeque::new(),

            timers: Timers {
                rto_timer: Timer {
                    timeout_ms: Some(SYN_TIMEOUT_MS),
                },
                recv_timer: Timer { timeout_ms: None },
            },

            time_ref: time::Instant::now(),

            state: State::Handshake(HandshakeState {
                phase: HandshakePhase::Alpha,
                timeout_time_ms: HANDSHAKE_TIMEOUT_MS,
                timeout_ms: HANDSHAKE_TIMEOUT_MS,
                packet_buffer: Vec::new(),
            }),
        })
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts in an attempt to produce an event.
    ///
    /// Returns `None` if no events are available, or if an error was encountered while reading
    /// from the internal socket.
    pub fn poll_event(&mut self) -> Option<Event> {
        if self.events.is_empty() {
            self.process_available_frames();

            self.process_timeouts();
        }

        return self.events.pop_front();
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts until an event can be returned.
    pub fn wait_event(&mut self) -> Event {
        loop {
            let wait_timeout = self.next_timer_timeout();

            self.process_available_frames_wait(wait_timeout);

            self.process_timeouts();

            if let Some(event) = self.events.pop_front() {
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
        if self.events.is_empty() {
            let mut remaining_timeout = timeout;
            let mut wait_begin = time::Instant::now();

            loop {
                let wait_timeout = if let Some(timer_timeout) = self.next_timer_timeout() {
                    remaining_timeout.min(timer_timeout)
                } else {
                    remaining_timeout
                };

                self.process_available_frames_wait(Some(wait_timeout));

                self.process_timeouts();

                if !self.events.is_empty() {
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

        return self.events.pop_front();
    }

    pub fn local_addr(&self) -> net::SocketAddr {
        self.local_addr
    }

    pub fn send(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        match &mut self.state {
            State::Handshake(state) => {
                state.packet_buffer.push((packet_bytes, mode));
            }
            State::Active(state) => {
                let ref mut host_ctx = HostContext {
                    timers: &mut self.timers,
                    socket: &self.socket,
                    events: &mut self.events,
                };

                state.endpoint.send(packet_bytes, mode, host_ctx);
            }
            State::Closed => {}
        }
    }

    pub fn flush(&mut self) {
        match &mut self.state {
            State::Handshake(_) => {}
            State::Active(state) => {
                let ref mut host_ctx = HostContext {
                    timers: &mut self.timers,
                    socket: &self.socket,
                    events: &mut self.events,
                };

                state.endpoint.flush(host_ctx);
            }
            State::Closed => {}
        }
    }
}
