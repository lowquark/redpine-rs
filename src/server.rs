use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::net;
use std::rc::{Rc, Weak};
use std::time;

use super::endpoint;
use super::timer_wheel;
use super::SendMode;

struct SocketReceiver {
    // Reference to non-blocking, server socket
    socket: Rc<net::UdpSocket>,
    // Always-allocated receive buffer
    recv_buffer: Box<[u8]>,
    // Polling objects
    poller: polling::Poller,
    poller_events: polling::Events,
}

type PeerId = u32;

struct PeerTimerData {
    peer_rc: PeerRc,
    timer_name: endpoint::TimerName,
}

struct PeerTimers {
    rto_timer_id: Option<timer_wheel::TimerId>,
    receive_timer_id: Option<timer_wheel::TimerId>,
}

struct PeerCore {
    // Peer identifier, 0 is a valid identifier
    id: PeerId,
    // Remote address
    addr: net::SocketAddr,
    // Current timer states
    timers: PeerTimers,
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
    peers: HashMap<net::SocketAddr, PeerRc>,
    // Stack of unused IDs
    free_ids: Vec<PeerId>,
}

type TimerWheel = timer_wheel::TimerWheel<PeerTimerData>;

pub enum Event {
    Connect(PeerHandle),
    Disconnect(PeerHandle),
    Receive(PeerHandle, Box<[u8]>),
    Timeout(PeerHandle),
}

struct EndpointContext<'a> {
    server: &'a mut ServerCore,
    peer: &'a mut PeerCore,
    peer_rc: &'a PeerRc,
}

struct ServerCore {
    // Timestamps are computed relative to this instant
    time_ref: time::Instant,
    // Server socket
    socket: Rc<net::UdpSocket>,
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
    // Permits both blocking and non-blocking reads from a non-blocking socket
    receiver: SocketReceiver,
    // Always-allocated timer expiration buffer
    timer_data_buffer: Vec<PeerTimerData>,
    // Cached from socket initialization
    local_addr: net::SocketAddr,
}

pub struct PeerHandle {
    peer: PeerRc,
}

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
    pub fn try_read_frame<'a>(
        &'a mut self,
    ) -> std::io::Result<Option<(&'a [u8], net::SocketAddr)>> {
        match self.socket.recv_from(&mut self.recv_buffer) {
            Ok((frame_len, sender_addr)) => {
                let ref frame_bytes = self.recv_buffer[..frame_len];
                Ok(Some((frame_bytes, sender_addr)))
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
    ) -> std::io::Result<Option<(&'a [u8], net::SocketAddr)>> {
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

impl PeerTimers {
    fn new() -> Self {
        Self {
            rto_timer_id: None,
            receive_timer_id: None,
        }
    }

    fn get_mut(&mut self, name: endpoint::TimerName) -> &mut Option<timer_wheel::TimerId> {
        match name {
            endpoint::TimerName::Rto => &mut self.rto_timer_id,
            endpoint::TimerName::Receive => &mut self.receive_timer_id,
        }
    }
}

impl Peer {
    fn new(id: PeerId, addr: net::SocketAddr, server_weak: ServerCoreWeak) -> Self {
        Self {
            core: PeerCore {
                id,
                addr,
                timers: PeerTimers::new(),
            },
            endpoint: endpoint::Endpoint::new(),
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
        self.peers.get(addr)
    }

    pub fn insert(
        &mut self,
        addr: &net::SocketAddr,
        server_weak: ServerCoreWeak,
    ) -> Option<PeerRc> {
        // Ensure a peer does not already exist at this address
        if !self.peers.contains_key(addr) {
            // Allocate an ID (an empty stack means we have reached the peer limit)
            if let Some(new_id) = self.free_ids.pop() {
                // Create new peer object
                let peer = Peer::new(new_id, addr.clone(), server_weak);
                let peer_rc = Rc::new(RefCell::new(peer));

                // Associate with given address
                self.peers.insert(addr.clone(), Rc::clone(&peer_rc));

                return Some(peer_rc);
            }
        }

        return None;
    }

    pub fn remove(&mut self, addr: &net::SocketAddr) {
        if let Some(peer_rc) = self.peers.remove(addr) {
            self.free_ids.push(peer_rc.borrow().core.id);
        } else {
            println!("double free");
        }
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
        println!("{:?} <- {:02X?}", self.peer.addr, frame_bytes);
        let _ = self.server.socket.send_to(frame_bytes, &self.peer.addr);
    }

    fn set_timer(&mut self, name: endpoint::TimerName, time_ms: u64) {
        let timer_id = self.peer.timers.get_mut(name);

        if let Some(id) = timer_id.take() {
            self.server.timer_wheel.unset_timer(id);
        }

        *timer_id = Some(self.server.timer_wheel.set_timer(
            time_ms,
            PeerTimerData {
                peer_rc: Rc::clone(self.peer_rc),
                timer_name: name,
            },
        ))
    }

    fn unset_timer(&mut self, name: endpoint::TimerName) {
        let timer_id = self.peer.timers.get_mut(name);

        if let Some(id) = timer_id.take() {
            self.server.timer_wheel.unset_timer(id);
        }
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

        self.server.peer_table.remove(&self.peer.addr);
    }

    fn on_receive(&mut self, packet_bytes: Box<[u8]>) {
        let handle = PeerHandle::new(Rc::clone(&self.peer_rc));
        let event = Event::Receive(handle, packet_bytes);
        self.server.events.push_back(event);
    }

    fn on_timeout(&mut self) {
        let handle = PeerHandle::new(Rc::clone(&self.peer_rc));
        let event = Event::Timeout(handle);
        self.server.events.push_back(event);

        self.server.peer_table.remove(&self.peer.addr);
    }
}

impl ServerCore {
    /// Returns the number of whole milliseconds elapsed since the server object was created.
    fn time_now_ms(&self) -> u64 {
        (time::Instant::now() - self.time_ref).as_millis() as u64
    }

    fn step_timer_wheel(&mut self, timer_data_buffer: &mut Vec<PeerTimerData>) -> u64 {
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
    pub fn handle_timeouts(&mut self, timer_data_buffer: &mut Vec<PeerTimerData>) {
        let now_ms = self.step_timer_wheel(timer_data_buffer);

        for timer_data in timer_data_buffer.drain(..) {
            let ref mut peer = *timer_data.peer_rc.borrow_mut();

            let ref mut ctx = EndpointContext::new(self, &mut peer.core, &timer_data.peer_rc);

            peer.endpoint
                .handle_timer(timer_data.timer_name, now_ms, ctx);
        }
    }

    fn handle_handshake_alpha(&self, sender_addr: &net::SocketAddr) {
        println!("acking phase α...");
        let _ = self.socket.send_to(&[0xA1], sender_addr);
    }

    fn handle_handshake_beta(&mut self, sender_addr: &net::SocketAddr) {
        if let Some(peer_rc) = self
            .peer_table
            .insert(sender_addr, Weak::clone(&self.self_weak))
        {
            let now_ms = self.time_now_ms();

            let ref mut peer = *peer_rc.borrow_mut();

            let ref mut endpoint = peer.endpoint;
            let ref mut peer_core = peer.core;

            let ref mut ctx = EndpointContext::new(self, peer_core, &peer_rc);

            endpoint.init(now_ms, ctx);

            println!(
                "created peer {}, for sender address {:?}",
                peer_core.id, sender_addr
            );
        }

        // Always send an ack in case a previous ack was dropped.
        println!("acking phase β...");
        let _ = self.socket.send_to(&[0xB1], sender_addr);
    }

    fn handle_frame_other(&mut self, frame_bytes: &[u8], sender_addr: &net::SocketAddr) {
        // If a peer is associated with this address, deliver this frame to its endpoint
        if let Some(peer_rc) = self.peer_table.find(sender_addr) {
            let peer_rc = Rc::clone(&peer_rc);

            let ref mut peer = *peer_rc.borrow_mut();

            let ref mut endpoint = peer.endpoint;
            let ref mut peer_core = peer.core;

            let ref mut ctx = EndpointContext::new(self, peer_core, &peer_rc);

            endpoint.handle_frame(frame_bytes, ctx);
        }
    }

    fn handle_frame(&mut self, frame_bytes: &[u8], sender_addr: &net::SocketAddr) {
        println!("{:?} -> {:02X?}", sender_addr, frame_bytes);

        let crc_valid = true;

        if crc_valid {
            // Initial handshakes are handled without an allocation in the peer table. Once a valid
            // open request is received, the sender's address is assumed valid (i.e. blockable) and
            // an entry in the peer table is created. Other frame types are handled by the
            // associated peer object.

            if frame_bytes == [0xA0] {
                let valid = true;

                if valid {
                    self.handle_handshake_alpha(sender_addr);
                }
            } else if frame_bytes == [0xB0] {
                let valid = true;

                if valid {
                    self.handle_handshake_beta(sender_addr);
                }
            } else {
                let valid = true;

                if valid {
                    self.handle_frame_other(frame_bytes, sender_addr);
                }
            }
        } else {
            // We don't negotiate with entropy
        }
    }

    /// Reads and processes as many frames as possible from receiver without blocking.
    pub fn handle_frames(&mut self, receiver: &mut SocketReceiver) {
        while let Ok(Some((frame_bytes, sender_addr))) = receiver.try_read_frame() {
            // Process this frame
            self.handle_frame(frame_bytes, &sender_addr);
        }
    }

    /// Reads and processes as many frames as possible from receiver, waiting up to `wait_timeout`
    /// for the first.
    pub fn handle_frames_wait(
        &mut self,
        receiver: &mut SocketReceiver,
        wait_timeout: Option<time::Duration>,
    ) {
        if let Ok(Some((frame_bytes, sender_addr))) = receiver.wait_for_frame(wait_timeout) {
            // Process this frame
            self.handle_frame(frame_bytes, &sender_addr);
            // Process any further frames without blocking
            self.handle_frames(receiver);
        }
    }
}

impl Server {
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

        let socket_rc = Rc::new(socket);

        let receiver = SocketReceiver::new(Rc::clone(&socket_rc), frame_size_max)?;

        // In order to do interesting things with Peer objects, PeerHandles require Peers to keep a
        // (weak) pointer to the ServerCore. The most convenient way to accomplish this is to give
        // ServerCores a weak pointer which can be cloned upon Peer construction. The alternative
        // is to pass pointers to the ServerCore down the frame handling call stack, which is
        // cumbersome.
        let core = Rc::new_cyclic(|self_weak| {
            RefCell::new(ServerCore {
                time_ref: time::Instant::now(),
                socket: socket_rc,
                peer_table: PeerTable::new(peer_count_max),
                timer_wheel: TimerWheel::new(&TIMER_WHEEL_ARRAY_CONFIG, 0),
                events: VecDeque::new(),
                self_weak: Weak::clone(self_weak),
            })
        });

        Ok(Self {
            core,
            receiver,
            timer_data_buffer: Vec::new(),
            local_addr,
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
            core.handle_frames(&mut self.receiver);

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

            core.handle_frames_wait(&mut self.receiver, wait_timeout);

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

                core.handle_frames_wait(&mut self.receiver, Some(wait_timeout));

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
        self.local_addr
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

            let ref mut ctx = EndpointContext::new(server, &mut peer.core, &self.peer);

            peer.endpoint.send(packet_bytes, mode, ctx);
        }
    }

    pub fn flush(&mut self) {
        let ref mut peer = *self.peer.borrow_mut();

        if let Some(server_rc) = Weak::upgrade(&peer.server_weak) {
            let ref mut server = *server_rc.borrow_mut();

            let ref mut ctx = EndpointContext::new(server, &mut peer.core, &self.peer);

            peer.endpoint.flush(ctx);
        }
    }

    pub fn disconnect(&mut self) {
        let ref mut peer = *self.peer.borrow_mut();

        if let Some(server_rc) = Weak::upgrade(&peer.server_weak) {
            let ref mut server = *server_rc.borrow_mut();

            let ref mut ctx = EndpointContext::new(server, &mut peer.core, &self.peer);

            peer.endpoint.disconnect(ctx);
        }
    }

    pub fn id(&self) -> PeerId {
        self.peer.borrow().core.id
    }
}
