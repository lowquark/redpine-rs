use std::any;
use std::collections::VecDeque;
use std::fmt;
use std::net;
use std::sync::{Arc, Mutex, RwLock};

use crate::endpoint;
use crate::frame;
use crate::socket;
use crate::timer_queue;
use crate::ErrorKind;
use crate::SendMode;

use super::epoch;
use super::Event;

struct PeerCore {
    // Remote address
    addr: net::SocketAddr,
    // Independent user data associated with this peer
    data: Option<Arc<dyn any::Any + Send + Sync>>,
    // Server epoch
    epoch: Arc<epoch::Epoch>,
    // Permits sending frames from a peer handle
    socket_tx: Arc<socket::SocketTx>,
    // Next RTO timer expiration time
    rto_timer: Option<u64>,
    // Permits signaling events from a peer handle
    events: Arc<Mutex<VecDeque<Event>>>,
}

pub struct Peer {
    // Peer metadata
    core: PeerCore,
    // Endpoint representing this peer's connection
    endpoint: endpoint::Endpoint,
}

impl Peer {
    pub fn new(
        addr: net::SocketAddr,
        endpoint_config: endpoint::Config,
        epoch: Arc<epoch::Epoch>,
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

    pub fn addr(&self) -> &net::SocketAddr {
        &self.core.addr
    }
}

struct EndpointContext<'a> {
    peer: &'a mut PeerCore,
    peer_ref: &'a PeerRef,
}

impl<'a> EndpointContext<'a> {
    pub fn new(peer: &'a mut PeerCore, peer_ref: &'a PeerRef) -> Self {
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

pub type PeerRef = Arc<RwLock<Peer>>;

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

/// Represents a connected client.
#[derive(Clone)]
pub struct PeerHandle {
    peer: PeerRef,
}

impl PeerHandle {
    pub(super) fn new(peer: PeerRef) -> Self {
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

pub fn init(peer_ref: &PeerRef) {
    let ref mut peer = *peer_ref.write().unwrap();

    let ref mut endpoint = peer.endpoint;
    let ref mut peer_core = peer.core;

    let now_ms = peer_core.epoch.time_now_ms();

    let ref mut ctx = EndpointContext::new(peer_core, peer_ref);

    endpoint.init(now_ms, ctx);
}

pub fn handle_frame(peer_ref: &PeerRef, frame_type: frame::FrameType, payload_bytes: &[u8]) {
    let ref mut peer = *peer_ref.write().unwrap();

    let ref mut endpoint = peer.endpoint;
    let ref mut peer_core = peer.core;

    let now_ms = peer_core.epoch.time_now_ms();

    let ref mut ctx = EndpointContext::new(peer_core, &peer_ref);

    endpoint.handle_frame(frame_type, payload_bytes, now_ms, ctx);
}

pub fn handle_rto_timer(peer_ref: &PeerRef) -> endpoint::TimeoutAction {
    let ref mut peer = *peer_ref.write().unwrap();

    let now_ms = peer.core.epoch.time_now_ms();

    let ref mut ctx = EndpointContext::new(&mut peer.core, &peer_ref);

    peer.endpoint.handle_rto_timer(now_ms, ctx)
}
