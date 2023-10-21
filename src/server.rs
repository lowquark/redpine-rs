use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::net;
use std::rc::Rc;
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

pub enum Event {
    Connect(PeerHandle),
    Disconnect(PeerHandle),
    Receive(PeerHandle, Box<[u8]>),
    Timeout(PeerHandle),
}

struct PeerTimerData {
    peer_rc: PeerRc,
    name: endpoint::TimerId,
}

type TimerWheel = timer_wheel::TimerWheel<PeerTimerData>;

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
    server: ServerCoreRc,
}

type PeerRc = Rc<RefCell<Peer>>;

struct PeerTable {
    // Array of peers indexed by ID
    array: Vec<Option<PeerRc>>,
    // Mapping from sender address to peer ID
    addr_map: HashMap<net::SocketAddr, PeerId>,
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
}

type ServerCoreRc = Rc<RefCell<ServerCore>>;

pub struct Server {
    // Interesting server data
    core: ServerCoreRc,
    // Permits both blocking and non-blocking reads from a non-blocking socket
    receiver: SocketReceiver,
    // Always-allocated timer expiration buffer
    timer_data_expired: Vec<PeerTimerData>,
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

    fn get_mut(&mut self, id: endpoint::TimerId) -> &mut Option<timer_wheel::TimerId> {
        match id {
            endpoint::TimerId::Rto => &mut self.rto_timer_id,
            endpoint::TimerId::Receive => &mut self.receive_timer_id,
        }
    }
}

impl Peer {
    fn new(id: PeerId, addr: net::SocketAddr, server: ServerCoreRc) -> Self {
        Self {
            core: PeerCore {
                id,
                addr,
                timers: PeerTimers::new(),
            },
            endpoint: endpoint::Endpoint::new(),
            server,
        }
    }
}

impl PeerTable {
    fn find_free_id(&self) -> PeerId {
        let mut idx = 0;
        for idx in 0..self.array.len() {
            if self.array[idx].is_none() {
                return idx as PeerId;
            }
        }
        panic!("no free IDs");
    }

    pub fn new(count_max: usize) -> Self {
        let array = (0..count_max).map(|_| None).collect::<Vec<_>>();
        let addr_map = HashMap::new();

        Self { array, addr_map }
    }

    pub fn get_mut(&mut self, id: PeerId) -> Option<&mut PeerRc> {
        if let Some(opt) = self.array.get_mut(id as usize) {
            opt.as_mut()
        } else {
            None
        }
    }

    pub fn find_mut(&mut self, addr: &net::SocketAddr) -> Option<&mut PeerRc> {
        if let Some(&id) = self.addr_map.get(addr) {
            self.get_mut(id)
        } else {
            None
        }
    }

    pub fn insert(
        &mut self,
        addr: &net::SocketAddr,
        server_rc: ServerCoreRc,
    ) -> Option<(PeerId, PeerRc)> {
        if self.addr_map.len() < self.array.len() && !self.addr_map.contains_key(addr) {
            let id = self.find_free_id();
            let peer_rc = Rc::new(RefCell::new(Peer::new(id, addr.clone(), server_rc)));

            self.array[id as usize] = Some(Rc::clone(&peer_rc));
            self.addr_map.insert(addr.clone(), id);

            return Some((id, peer_rc));
        }
        return None;
    }

    pub fn remove(&mut self, id: PeerId) {
        if let Some(ref peer_rc) = self.array[id as usize] {
            self.addr_map.remove(&peer_rc.borrow().core.addr);
            self.array[id as usize] = None;
        } else {
            panic!("double free");
        }
    }

    pub fn count(&self) -> usize {
        self.addr_map.len()
    }
}

impl<'a> EndpointContext<'a> {
    fn new(server: &'a mut ServerCore, peer: &'a mut PeerCore, peer_rc: &'a PeerRc) -> Self {
        Self { server, peer, peer_rc }
    }
}

impl<'a> endpoint::HostContext for EndpointContext<'a> {
    fn send(&mut self, frame_bytes: &[u8]) {
        println!("sending frame to {:?}!", self.peer.addr);
        let _ = self.server.socket.send_to(frame_bytes, &self.peer.addr);
    }

    fn set_timer(&mut self, name: endpoint::TimerId, time_ms: u64) {
        let timer_id = self.peer.timers.get_mut(name);

        if let Some(id) = timer_id.take() {
            self.server.timer_wheel.unset_timer(id);
        }

        *timer_id = Some(self.server.timer_wheel.set_timer(
            time_ms,
            PeerTimerData {
                peer_rc: Rc::clone(self.peer_rc),
                name,
            },
        ))
    }

    fn unset_timer(&mut self, name: endpoint::TimerId) {
        let timer_id = self.peer.timers.get_mut(name);

        if let Some(id) = timer_id.take() {
            self.server.timer_wheel.unset_timer(id);
        }
    }
}

impl ServerCore {
    /// Returns the number of whole milliseconds elapsed since the server object was created.
    fn time_now_ms(&self) -> u64 {
        (time::Instant::now() - self.time_ref).as_millis() as u64
    }

    fn step_timer_wheel(&mut self, timer_data_expired: &mut Vec<PeerTimerData>) -> u64 {
        let now_ms = self.time_now_ms();

        self.timer_wheel.step(now_ms, timer_data_expired);

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
}

fn handle_handshake_alpha(server_rc: &ServerCoreRc, sender_addr: &net::SocketAddr) {
    println!("acking phase α...");
    let _ = server_rc.borrow().socket.send_to(&[0xA1], sender_addr);
}

fn handle_handshake_beta(server_rc: &ServerCoreRc, sender_addr: &net::SocketAddr) {
    let mut server_ref = server_rc.borrow_mut();
    let ref mut server = *server_ref;

    let now_ms = server.time_now_ms();

    if let Some((new_peer_id, peer_rc)) = server_ref
        .peer_table
        .insert(sender_addr, Rc::clone(server_rc))
    {
        // Notify user of inbound connection
        let handle = PeerHandle::new(Rc::clone(&peer_rc));
        server_ref.events.push_back(Event::Connect(handle));

        // Set a dummy timer just for fun
        server_ref.timer_wheel.set_timer(
            now_ms + 500,
            PeerTimerData {
                peer_rc,
                name: endpoint::TimerId::Rto,
            },
        );

        println!(
            "created peer {}, for sender address {:?}",
            new_peer_id, sender_addr
        );
    }

    // Always send an ack in case a previous ack was dropped.
    println!("acking phase β...");
    let _ = server_ref.socket.send_to(&[0xB1], sender_addr);
}

fn handle_connected_frame(server_rc: &ServerCoreRc, peer_rc: &PeerRc, frame_bytes: &[u8]) {
    let mut server_ref = server_rc.borrow_mut();
    let ref mut server = *server_ref;

    let mut peer_ref = peer_rc.borrow_mut();
    let ref mut peer = *peer_ref;

    let ref mut endpoint = peer.endpoint;

    let ref mut host_ctx = EndpointContext::new(server, &mut peer.core, peer_rc);

    endpoint.handle_frame(frame_bytes, host_ctx);

    while let Some(packet) = endpoint.pop_packet() {
        let handle = PeerHandle::new(Rc::clone(&peer_rc));
        server.events.push_back(Event::Receive(handle, packet));
    }

    if endpoint.is_closed() {
        let handle = PeerHandle::new(Rc::clone(&peer_rc));
        server.events.push_back(Event::Disconnect(handle));

        server.peer_table.remove(peer.core.id);
    }
}

fn handle_frame(server_rc: &ServerCoreRc, frame_bytes: &[u8], sender_addr: &net::SocketAddr) {
    println!(
        "received a packet of length {} from {:?}",
        frame_bytes.len(),
        sender_addr
    );
    println!("{:02X?}", sender_addr);

    let crc_valid = true;

    if crc_valid {
        // Initial handshakes are handled without an allocation in the peer table. Once a valid
        // open request is received, the sender's address is assumed valid (i.e. blockable) and
        // an entry in the peer table is created. Other frame types are handled by the
        // associated peer object.

        if frame_bytes == [0xA0] {
            let valid = true;

            if valid {
                handle_handshake_alpha(server_rc, sender_addr);
            }
        } else if frame_bytes == [0xB0] {
            let valid = true;

            if valid {
                handle_handshake_beta(server_rc, sender_addr);
            }
        } else {
            let valid = true;

            if valid {
                let mut server_ref = server_rc.borrow_mut();

                if let Some(peer_rc) = server_ref.peer_table.find_mut(sender_addr) {
                    let peer_rc = Rc::clone(&peer_rc);

                    std::mem::drop(server_ref);

                    handle_connected_frame(server_rc, &peer_rc, frame_bytes);
                }
            }
        }
    } else {
        // We don't negotiate with entropy
    }
}

fn handle_timer(
    server_rc: &ServerCoreRc,
    peer_rc: &PeerRc,
    timer_id: endpoint::TimerId,
    now_ms: u64,
) {
    let mut server_ref = server_rc.borrow_mut();
    let ref mut server = *server_ref;

    let mut peer_ref = peer_rc.borrow_mut();
    let ref mut peer = *peer_ref;

    println!("Timer expired for peer {}!", peer.core.id);

    let ref mut endpoint = peer.endpoint;

    let ref mut host_ctx = EndpointContext::new(server, &mut peer.core, peer_rc);

    endpoint.handle_timer(timer_id, now_ms, host_ctx);

    while let Some(packet) = endpoint.pop_packet() {
        let handle = PeerHandle::new(Rc::clone(&peer_rc));
        server.events.push_back(Event::Receive(handle, packet));
    }

    if endpoint.is_closed() {
        let handle = PeerHandle::new(Rc::clone(&peer_rc));
        server.events.push_back(Event::Disconnect(handle));

        server.peer_table.remove(peer.core.id);
    }
}

fn handle_send(
    server_rc: &ServerCoreRc,
    peer_rc: &PeerRc,
    packet_bytes: Box<[u8]>,
    mode: SendMode,
) {
    let mut peer_ref = peer_rc.borrow_mut();
    let ref mut peer = *peer_ref;

    println!("sending packet to {:?}!", peer.core.addr);
    let _ = peer.endpoint.enqueue_packet(packet_bytes, mode);
}

fn handle_flush(server_rc: &ServerCoreRc, peer_rc: &PeerRc) {
    let mut server_ref = server_rc.borrow_mut();
    let ref mut server = *server_ref;

    let mut peer_ref = peer_rc.borrow_mut();
    let ref mut peer = *peer_ref;

    let ref mut endpoint = peer.endpoint;

    let ref mut host_ctx = EndpointContext::new(server, &mut peer.core, peer_rc);

    endpoint.flush(host_ctx);

    while let Some(packet) = endpoint.pop_packet() {
        let handle = PeerHandle::new(Rc::clone(&peer_rc));
        server.events.push_back(Event::Receive(handle, packet));
    }

    if endpoint.is_closed() {
        let handle = PeerHandle::new(Rc::clone(&peer_rc));
        server.events.push_back(Event::Disconnect(handle));

        server.peer_table.remove(peer.core.id);
    }
}

fn handle_disconnect(server_rc: &ServerCoreRc, peer_rc: &PeerRc) {
    let mut server_ref = server_rc.borrow_mut();
    let ref mut server = *server_ref;

    let mut peer_ref = peer_rc.borrow_mut();
    let ref mut peer = *peer_ref;

    let ref mut endpoint = peer.endpoint;

    let ref mut host_ctx = EndpointContext::new(server, &mut peer.core, peer_rc);

    endpoint.disconnect(host_ctx);

    while let Some(packet) = endpoint.pop_packet() {
        let handle = PeerHandle::new(Rc::clone(&peer_rc));
        server.events.push_back(Event::Receive(handle, packet));
    }

    if endpoint.is_closed() {
        let handle = PeerHandle::new(Rc::clone(&peer_rc));
        server.events.push_back(Event::Disconnect(handle));

        server.peer_table.remove(peer.core.id);
    }
}

impl Server {
    /// Reads and processes as many frames as possible without blocking.
    fn process_available_frames(&mut self) {
        while let Ok(Some((frame_bytes, sender_addr))) = self.receiver.try_read_frame() {
            // Process this frame
            handle_frame(&self.core, frame_bytes, &sender_addr);
        }
    }

    /// Reads and processes as many frames as possible, waiting up to `wait_timeout` for the first.
    fn process_available_frames_wait(&mut self, wait_timeout: Option<time::Duration>) {
        if let Ok(Some((frame_bytes, sender_addr))) = self.receiver.wait_for_frame(wait_timeout) {
            // Process this frame
            handle_frame(&self.core, frame_bytes, &sender_addr);
            // Process any further frames without blocking
            return self.process_available_frames();
        }
    }

    /// Processes all pending timer events.
    fn process_timeouts(&mut self) {
        let now_ms = self
            .core
            .borrow_mut()
            .step_timer_wheel(&mut self.timer_data_expired);

        for timer_data in self.timer_data_expired.drain(..) {
            handle_timer(&self.core, &timer_data.peer_rc, timer_data.name, now_ms);
        }
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

        let socket_rc = Rc::new(socket);

        let receiver = SocketReceiver::new(Rc::clone(&socket_rc), frame_size_max)?;

        Ok(Self {
            core: Rc::new(RefCell::new(ServerCore {
                time_ref: time::Instant::now(),
                socket: socket_rc,
                peer_table: PeerTable::new(peer_count_max),
                timer_wheel: TimerWheel::new(&TIMER_WHEEL_ARRAY_CONFIG, 0),
                events: VecDeque::new(),
            })),
            receiver,
            timer_data_expired: Vec::new(),
            local_addr,
        })
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts in an attempt to produce an event.
    ///
    /// Returns `None` if no events are available, or if an error was encountered while reading
    /// from the internal socket.
    pub fn poll_event(&mut self) -> Option<Event> {
        if self.core.borrow().events.is_empty() {
            self.process_available_frames();

            self.process_timeouts();
        }

        return self.core.borrow_mut().events.pop_front();
    }

    /// If any events are ready to be processed, returns the next event immediately. Otherwise,
    /// reads inbound frames and processes timeouts until an event can be returned.
    pub fn wait_event(&mut self) -> Event {
        loop {
            let wait_timeout = self.core.borrow().next_timer_timeout();

            self.process_available_frames_wait(wait_timeout);

            self.process_timeouts();

            if let Some(event) = self.core.borrow_mut().events.pop_front() {
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
        if self.core.borrow_mut().events.is_empty() {
            let mut remaining_timeout = timeout;
            let mut wait_begin = time::Instant::now();

            loop {
                let wait_timeout =
                    if let Some(timer_timeout) = self.core.borrow().next_timer_timeout() {
                        remaining_timeout.min(timer_timeout)
                    } else {
                        remaining_timeout
                    };

                self.process_available_frames_wait(Some(wait_timeout));

                self.process_timeouts();

                let mut core_ref = self.core.borrow_mut();

                if !core_ref.events.is_empty() {
                    // Found what we're looking for
                    return core_ref.events.pop_front();
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

        return self.core.borrow_mut().events.pop_front();
    }

    pub fn local_addr(&self) -> net::SocketAddr {
        self.local_addr
    }

    pub fn peer_count(&self) -> usize {
        self.core.borrow().peer_table.count().into()
    }
}

impl PeerHandle {
    fn new(peer: PeerRc) -> Self {
        Self { peer }
    }

    pub fn send(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        let server_rc = Rc::clone(&self.peer.borrow().server);

        handle_send(&server_rc, &self.peer, packet_bytes, mode);
    }

    pub fn flush(&mut self) {
        let server_rc = Rc::clone(&self.peer.borrow().server);

        handle_flush(&server_rc, &self.peer);
    }

    pub fn disconnect(&mut self) {
        let server_rc = Rc::clone(&self.peer.borrow().server);

        handle_disconnect(&server_rc, &self.peer);
    }

    pub fn id(&self) -> PeerId {
        self.peer.borrow().core.id
    }
}
