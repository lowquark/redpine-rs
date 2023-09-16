use std::collections::VecDeque;
use std::net;
use std::time;

use super::endpoint;
use super::SendMode;

const SOCKET_POLLING_KEY: usize = 0;

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
}

impl Client {
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
        let frame_bytes = &self.recv_buffer[..frame_size];

        println!(
            "received a packet of length {}",
            frame_bytes.len()
        );
        println!("{:02X?}", frame_bytes);

        self.events.push_back(Event::Receive(frame_bytes.into()));
    }

    /// Reads and processes as many frames as possible from the socket without blocking.
    fn process_available_frames(&mut self) -> std::io::Result<()> {
        while let Some(frame_size) = self.try_read_frame()? {
            // Process this frame
            self.process_frame(frame_size);
        }

        Ok(())
    }

    /// Reads and processes as many frames as possible from the socket, waiting up to
    /// `wait_timeout` for the first.
    fn process_available_frames_wait(
        &mut self,
        wait_timeout: Option<time::Duration>,
    ) -> std::io::Result<()> {
        if let Some(frame_size) = self.wait_for_frame(wait_timeout)? {
            // Process this frame
            self.process_frame(frame_size);
            // Process any further frames without blocking
            return self.process_available_frames();
        }

        Ok(())
    }

    /// Returns the time remaining until the next timer expires.
    fn next_timer_timeout(&self) -> Option<time::Duration> {
        None
    }

    fn process_timeouts(&mut self) {}

    pub fn connect<A>(server_addr: A) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
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

        let max_recv_size: usize = 1478;
        let max_peers: usize = 8192;

        Ok(Self {
            socket,
            local_addr,
            peer_addr,

            poller,
            poller_events: polling::Events::new(),

            recv_buffer: vec![0; max_recv_size].into_boxed_slice(),

            events: vec![Event::Connect].into(),
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

    pub fn send(&mut self, packet_bytes: &[u8], _mode: SendMode) {
        self.socket.send(packet_bytes);
    }
}
