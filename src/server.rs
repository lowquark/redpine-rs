use std::collections::HashMap;
use std::collections::VecDeque;
use std::net;
use std::rc::Rc;
use std::time;

use super::endpoint;
use super::timer_wheel;
use super::SendMode;

type PeerId = u32;

const SOCKET_POLLING_KEY: usize = 0;

const TIMER_WHEEL_ARRAY_CONFIG: [timer_wheel::ArrayConfig; 3] = [
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

struct PeerTimerData {
    peer_id: PeerId,
    name: endpoint::TimerId,
}

type TimerWheel = timer_wheel::TimerWheel<PeerTimerData>;

struct PeerTimers {
    rto_timer_id: Option<timer_wheel::TimerId>,
    receive_timer_id: Option<timer_wheel::TimerId>,
}

impl PeerTimers {
    fn get_mut(&mut self, id: endpoint::TimerId) -> &mut Option<timer_wheel::TimerId> {
        match id {
            endpoint::TimerId::Rto => &mut self.rto_timer_id,
            endpoint::TimerId::Receive => &mut self.receive_timer_id,
        }
    }
}

struct HostContext<'a> {
    socket: &'a net::UdpSocket,
    peer_id: PeerId,
    peer_addr: &'a net::SocketAddr,
    timer_wheel: &'a mut TimerWheel,
    timers: &'a mut PeerTimers,
}

impl<'a> endpoint::HostContext for HostContext<'a> {
    fn send(&mut self, frame_bytes: &[u8]) {
        println!("sending frame to {:?}!", self.peer_addr);
        let _ = self.socket.send_to(frame_bytes, self.peer_addr);
    }

    fn set_timer(&mut self, name: endpoint::TimerId, time_ms: u64) {
        let timer_id = self.timers.get_mut(name);

        if let Some(id) = timer_id.take() {
            self.timer_wheel.unset_timer(id);
        }

        *timer_id = Some(self.timer_wheel.set_timer(
            time_ms,
            PeerTimerData {
                peer_id: self.peer_id,
                name,
            },
        ))
    }

    fn unset_timer(&mut self, name: endpoint::TimerId) {
        let timer_id = self.timers.get_mut(name);

        if let Some(id) = timer_id.take() {
            self.timer_wheel.unset_timer(id);
        }
    }
}

struct Peer {
    id: PeerId,
    addr: net::SocketAddr,
    endpoint: endpoint::Endpoint,
    timers: PeerTimers,
}

impl Peer {
    fn new(id: PeerId, addr: net::SocketAddr) -> Self {
        Self {
            id,
            addr,
            endpoint: endpoint::Endpoint::new(),
            timers: PeerTimers {
                rto_timer_id: None,
                receive_timer_id: None,
            },
        }
    }
}

pub struct PeerHandle<'a> {
    peer: &'a mut Peer,
    socket: &'a net::UdpSocket,
    timer_wheel: &'a mut TimerWheel,
}

impl<'a> PeerHandle<'a> {
    pub fn send(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        println!("sending packet to {:?}!", self.peer.addr);
        let _ = self.peer.endpoint.enqueue_packet(packet_bytes, mode);
    }

    pub fn flush(&mut self) {
        let ref mut host_ctx = HostContext {
            socket: &*self.socket,
            peer_id: self.peer.id,
            peer_addr: &self.peer.addr,
            timer_wheel: &mut self.timer_wheel,
            timers: &mut self.peer.timers,
        };

        self.peer.endpoint.flush(host_ctx);

        // flush() generates no events, so no need to poll here
    }
}

pub enum Event {
    Connect(PeerId),
    Disconnect(PeerId),
    Receive(PeerId, Box<[u8]>),
    Timeout(PeerId),
}

fn find_free_index(peer_table: &Vec<Option<Peer>>) -> Option<usize> {
    let mut idx = 0;

    for idx in 0..peer_table.len() {
        if peer_table[idx].is_none() {
            return Some(idx);
        }
    }

    return None;
}

pub struct Server {
    socket: Rc<net::UdpSocket>,
    local_addr: net::SocketAddr,

    poller: polling::Poller,
    poller_events: polling::Events,

    recv_buffer: Box<[u8]>,

    peer_table: Vec<Option<Peer>>,
    address_table: HashMap<net::SocketAddr, PeerId>,
    peer_count_max: usize,

    events: VecDeque<Event>,

    time_ref: time::Instant,
    timer_wheel: TimerWheel,
    timer_data_expired: Vec<PeerTimerData>,
}

impl Server {
    /// Returns the number of whole milliseconds elapsed since the server object was created.
    fn time_now_ms(&self) -> u64 {
        (time::Instant::now() - self.time_ref).as_millis() as u64
    }

    fn process_handshake_alpha(&mut self, sender_addr: &net::SocketAddr) {
        println!("acking phase α...");
        let _ = self.socket.send_to(&[0xA1], sender_addr);
    }

    fn process_handshake_beta(&mut self, sender_addr: &net::SocketAddr, now_ms: u64) {
        if self.address_table.len() < self.peer_count_max
            && !self.address_table.contains_key(&sender_addr)
        {
            // Generate a new peer ID
            let mut idx = find_free_index(&self.peer_table).expect("size mismatch");
            let id = idx as PeerId;

            // Create a new peer object
            let peer = Peer::new(id, sender_addr.clone());

            // Place peer in the ID table
            self.peer_table[idx] = Some(peer);

            // Associate peer with sender address
            self.address_table.insert(sender_addr.clone(), id);

            // Notify user of inbound connection
            self.events.push_back(Event::Connect(id));

            // Set a dummy timer just for fun
            self.timer_wheel.set_timer(
                now_ms + 500,
                PeerTimerData {
                    peer_id: id,
                    name: endpoint::TimerId::Rto,
                },
            );

            println!(
                "created client {}, for sender address {:?}",
                idx, sender_addr
            );
        }

        // Always send an ack in case a previous ack was dropped.
        println!("acking phase β...");
        let _ = self.socket.send_to(&[0xB1], sender_addr);
    }

    fn process_peer_frame(&mut self, peer_id: PeerId, frame_size: usize, now_ms: u64) {
        let ref frame_bytes = self.recv_buffer[..frame_size];

        if let Some(Some(peer)) = self.peer_table.get_mut(peer_id as usize) {
            let ref mut endpoint = peer.endpoint;

            let ref mut host_ctx = HostContext {
                socket: &*self.socket,
                peer_id,
                peer_addr: &peer.addr,
                timer_wheel: &mut self.timer_wheel,
                timers: &mut peer.timers,
            };

            endpoint.handle_frame(frame_bytes, host_ctx);

            while let Some(packet) = endpoint.pop_packet() {
                self.events.push_back(Event::Receive(peer_id, packet));
            }

            if endpoint.is_closed() {
                self.events.push_back(Event::Disconnect(peer_id));

                self.address_table.remove(&peer.addr);
                self.peer_table[peer_id as usize] = None;
            }
        }
    }

    fn process_frame(&mut self, frame_size: usize, sender_addr: &net::SocketAddr) {
        let now_ms = self.time_now_ms();

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
                    self.process_handshake_alpha(sender_addr);
                }
            } else if frame_bytes == [0xB0] {
                let valid = true;

                if valid {
                    self.process_handshake_beta(sender_addr, now_ms);
                }
            } else if let Some(&peer_id) = self.address_table.get(sender_addr) {
                let valid = true;

                if valid {
                    self.process_peer_frame(peer_id, frame_size, now_ms);
                }
            }
        } else {
            // We don't negotiate with entropy
        }
    }

    fn process_timeouts(&mut self) {
        let now_ms = self.time_now_ms();

        self.timer_wheel.step(now_ms, &mut self.timer_data_expired);

        for timer_data in self.timer_data_expired.drain(..) {
            println!("Timer expired!");

            let peer_id = timer_data.peer_id;
            let timer_name = timer_data.name;

            if let Some(Some(peer)) = self.peer_table.get_mut(peer_id as usize) {
                let ref mut endpoint = peer.endpoint;

                let ref mut host_ctx = HostContext {
                    socket: &*self.socket,
                    peer_id,
                    peer_addr: &peer.addr,
                    timer_wheel: &mut self.timer_wheel,
                    timers: &mut peer.timers,
                };

                endpoint.handle_timer(timer_name, now_ms, host_ctx);

                while let Some(packet) = endpoint.pop_packet() {
                    self.events.push_back(Event::Receive(peer_id, packet));
                }

                if endpoint.is_closed() {
                    let peer_addr = peer.addr.clone();

                    self.events.push_back(Event::Disconnect(peer_id));

                    self.address_table.remove(&peer_addr);
                    self.peer_table[peer_id as usize] = None;
                }
            }
        }
    }

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

    pub fn bind<A>(bind_addr: A) -> std::io::Result<Self>
    where
        A: net::ToSocketAddrs,
    {
        let frame_size_max: usize = 1478;
        let peer_count_max: usize = 8192;

        assert!(peer_count_max > 0);
        assert!(peer_count_max - 1 <= PeerId::max_value() as usize);

        let socket = net::UdpSocket::bind(bind_addr)?;
        socket.set_nonblocking(true)?;

        let local_addr = socket.local_addr()?;

        let poller = polling::Poller::new()?;

        unsafe {
            poller.add(&socket, polling::Event::readable(SOCKET_POLLING_KEY))?;
        }

        let peer_table = (0..peer_count_max).map(|_| None).collect::<Vec<_>>();

        Ok(Self {
            socket: Rc::new(socket),
            local_addr,

            poller,
            poller_events: polling::Events::new(),

            recv_buffer: vec![0; frame_size_max].into_boxed_slice(),

            peer_table,
            address_table: HashMap::new(),
            peer_count_max,

            events: VecDeque::new(),

            time_ref: time::Instant::now(),
            timer_wheel: TimerWheel::new(&TIMER_WHEEL_ARRAY_CONFIG, 0),
            timer_data_expired: Vec::new(),
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

    pub fn peer<'a>(&'a mut self, id: PeerId) -> Option<PeerHandle<'a>> {
        if let Some(Some(peer)) = self.peer_table.get_mut(id as usize) {
            return Some(PeerHandle {
                peer,
                socket: &self.socket,
                timer_wheel: &mut self.timer_wheel,
            });
        }

        return None;
    }
}
