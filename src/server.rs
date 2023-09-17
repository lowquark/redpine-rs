use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::net;
use std::rc::Rc;
use std::time;

use super::endpoint;
use super::SendMode;

type PeerId = u32;

const SOCKET_POLLING_KEY: usize = 0;

struct HostContext<'a> {
    socket: &'a net::UdpSocket,
    peer_addr: &'a net::SocketAddr,
}

impl<'a> endpoint::HostContext for HostContext<'a> {
    fn send(&mut self, frame_bytes: &[u8]) {
        let _ = self.socket.send_to(frame_bytes, self.peer_addr);
    }

    fn set_timer(&mut self, timer: endpoint::TimerId, time_ms: u64) {}

    fn unset_timer(&mut self, timer: endpoint::TimerId) {}
}

pub struct Peer {
    id: PeerId,

    socket: Rc<net::UdpSocket>,
    peer_addr: net::SocketAddr,

    endpoint: endpoint::Endpoint,
}

impl Peer {
    fn new(id: PeerId, socket: Rc<net::UdpSocket>, peer_addr: net::SocketAddr) -> Self {
        Self {
            id,

            socket,
            peer_addr,

            endpoint: endpoint::Endpoint::new(),
        }
    }

    pub fn id(&self) -> PeerId {
        self.id
    }

    pub fn send(&mut self, packet_bytes: &[u8], _mode: SendMode) {
        println!("sending packet to {:?}!", self.peer_addr);
        let _ = self.socket.send_to(packet_bytes, &self.peer_addr);
    }

    pub fn flush(&mut self) {
        let ref mut host_ctx = HostContext {
            socket: &*self.socket,
            peer_addr: &self.peer_addr,
        };

        self.endpoint.flush(host_ctx);
    }

    fn handle_frame(&mut self, frame_bytes: &[u8]) {
        let ref mut host_ctx = HostContext {
            socket: &*self.socket,
            peer_addr: &self.peer_addr,
        };

        self.endpoint.handle_frame(frame_bytes, host_ctx);
    }
}

type PeerRc = Rc<RefCell<Peer>>;

pub enum Event {
    Connect(PeerRc),
    Disconnect(PeerRc),
    Receive(PeerRc, Box<[u8]>),
    Timeout(PeerRc),
}

pub struct Server {
    socket: Rc<net::UdpSocket>,
    local_addr: net::SocketAddr,

    poller: polling::Poller,
    poller_events: polling::Events,

    recv_buffer: Box<[u8]>,

    // peer_ids: PeerIdReel,
    peers: HashMap<net::SocketAddr, PeerRc>,
    peer_count_max: usize,

    events: VecDeque<Event>,
}

impl Server {
    /// If a valid frame can be read from the socket, returns the frame. Returns Ok(None)
    /// otherwise.
    fn try_read_frame(&mut self) -> std::io::Result<Option<(usize, net::SocketAddr)>> {
        match self.socket.recv_from(&mut self.recv_buffer) {
            Ok((frame_len, sender_addr)) => Ok(Some((frame_len, sender_addr))),
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
    ) -> std::io::Result<Option<(usize, net::SocketAddr)>> {
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

    fn process_frame(&mut self, frame_size: usize, sender_addr: &net::SocketAddr) {
        let ref frame_bytes = self.recv_buffer[..frame_size];

        println!(
            "received a packet of length {} from {:?}",
            frame_bytes.len(),
            sender_addr
        );
        println!("{:02X?}", frame_bytes);

        let crc_valid = true;

        if crc_valid {
            // Initial handshakes are handled without an allocation in the peer table. Once a valid
            // open request is received, the sender's address is assumed valid (i.e. blockable) and
            // an entry in the peer table is created. Other frame types are handled by the
            // associated peer object.

            if frame_bytes == [0xA0] {
                let valid = true;

                if valid {
                    println!("acking phase α...");
                    let _ = self.socket.send_to(&[0xA1], sender_addr);
                }
            } else if frame_bytes == [0xB0] {
                let valid = true;

                if valid {
                    if self.peers.len() < self.peer_count_max
                        && !self.peers.contains_key(&sender_addr)
                    {
                        // Associate a new peer object with this address
                        let peer_id = 0;
                        let peer = Peer::new(peer_id, Rc::clone(&self.socket), sender_addr.clone());
                        let peer_rc = Rc::new(RefCell::new(peer));
                        self.peers.insert(sender_addr.clone(), Rc::clone(&peer_rc));

                        // Notify user of inbound connection
                        self.events.push_back(Event::Connect(peer_rc));
                    }

                    // Always send an ack in case a previous ack was dropped.
                    println!("acking phase β...");
                    let _ = self.socket.send_to(&[0xB1], sender_addr);
                }
            } else if let Some(peer_rc) = self.peers.get(sender_addr) {
                let mut peer = peer_rc.borrow_mut();

                println!("associated peer id: {}", peer.id());

                peer.handle_frame(frame_bytes);

                while let Some(packet) = peer.endpoint.pop_packet() {
                    self.events
                        .push_back(Event::Receive(Rc::clone(peer_rc), packet));
                }

                if peer.endpoint.is_closed() {
                    self.events.push_back(Event::Disconnect(Rc::clone(peer_rc)));

                    std::mem::drop(peer);
                    self.peers.remove(sender_addr);
                }
            }
        } else {
            // We don't negotiate with entropy
        }
    }

    /// Reads and processes as many frames as possible from the socket without blocking.
    fn process_available_frames(&mut self) {
        while let Ok(Some((frame_size, sender_addr))) = self.try_read_frame() {
            // Process this frame
            self.process_frame(frame_size, &sender_addr);
        }
    }

    /// Reads and processes as many frames as possible from the socket, waiting up to
    /// `wait_timeout` for the first.
    fn process_available_frames_wait(&mut self, wait_timeout: Option<time::Duration>) {
        if let Ok(Some((frame_size, sender_addr))) = self.wait_for_frame(wait_timeout) {
            // Process this frame
            self.process_frame(frame_size, &sender_addr);
            // Process any further frames without blocking
            return self.process_available_frames();
        }
    }

    /// Returns the time remaining until the next timer expires.
    fn next_timer_timeout(&self) -> Option<time::Duration> {
        None
    }

    fn process_timeouts(&mut self) {}

    pub fn bind<A>(bind_addr: A) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        let socket = net::UdpSocket::bind(bind_addr)?;
        socket.set_nonblocking(true)?;
        let local_addr = socket.local_addr()?;

        let poller = polling::Poller::new()?;

        unsafe {
            poller.add(&socket, polling::Event::readable(SOCKET_POLLING_KEY))?;
        }

        let max_recv_size: usize = 1478;
        let max_peers: usize = 8192;

        Ok(Self {
            socket: Rc::new(socket),
            local_addr,

            poller,
            poller_events: polling::Events::new(),

            recv_buffer: vec![0; max_recv_size].into_boxed_slice(),

            //peer_ids: PeerIdReel::new(max_peers),
            peers: HashMap::new(),
            peer_count_max: max_peers,

            events: VecDeque::new(),
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
}
