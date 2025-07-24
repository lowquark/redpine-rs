use std::net;
use std::sync::Arc;
use std::time;

const SOCKET_POLLING_KEY: usize = 0;

pub struct SocketTx {
    // Reference to non-blocking server socket
    socket: Arc<net::UdpSocket>,
}

pub struct SocketRx {
    // Reference to non-blocking server socket
    socket: Arc<net::UdpSocket>,
    // Cached from socket initialization
    local_addr: net::SocketAddr,
    // Polling objects
    poller: polling::Poller,
    poller_events: polling::Events,
    // Always-allocated receive buffer
    recv_buffer: Box<[u8]>,
}

pub struct ConnectedSocketTx {
    // Reference to non-blocking server socket
    socket: Arc<net::UdpSocket>,
}

pub struct ConnectedSocketRx {
    // Reference to non-blocking server socket
    socket: Arc<net::UdpSocket>,
    // Cached from socket initialization
    local_addr: net::SocketAddr,
    peer_addr: net::SocketAddr,
    // Polling objects
    poller: polling::Poller,
    poller_events: polling::Events,
    // Always-allocated receive buffer
    recv_buffer: Box<[u8]>,
}

impl SocketTx {
    pub fn send(&self, frame: &[u8], addr: &net::SocketAddr) {
        let _ = self.socket.send_to(frame, addr);
    }
}

impl SocketRx {
    /// If a valid frame can be read from the socket, returns the frame. Returns Ok(None)
    /// otherwise.
    pub fn try_read_frame(
        &mut self,
    ) -> std::io::Result<Option<(&[u8], net::SocketAddr)>> {
        match self.socket.recv_from(&mut self.recv_buffer) {
            Ok((frame_len, sender_addr)) => {
                let frame_bytes = &self.recv_buffer[..frame_len];
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
    pub fn wait_for_frame(
        &mut self,
        timeout: Option<time::Duration>,
    ) -> std::io::Result<Option<(&[u8], net::SocketAddr)>> {
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

    pub fn local_addr(&self) -> net::SocketAddr {
        self.local_addr
    }
}

pub fn new<A>(bind_address: A, frame_size_max: usize) -> std::io::Result<(SocketTx, SocketRx)>
where
    A: net::ToSocketAddrs,
{
    let socket = net::UdpSocket::bind(bind_address)?;
    socket.set_nonblocking(true)?;

    let local_addr = socket.local_addr()?;

    let poller = polling::Poller::new()?;

    unsafe {
        poller.add(&socket, polling::Event::readable(SOCKET_POLLING_KEY))?;
    }

    let socket_rc = Arc::new(socket);

    let tx = SocketTx {
        socket: Arc::clone(&socket_rc),
    };

    let rx = SocketRx {
        socket: socket_rc,
        local_addr,
        poller,
        poller_events: polling::Events::new(),
        recv_buffer: vec![0; frame_size_max].into_boxed_slice(),
    };

    Ok((tx, rx))
}

impl ConnectedSocketTx {
    pub fn send(&self, frame: &[u8]) {
        let _ = self.socket.send(frame);
    }
}

impl ConnectedSocketRx {
    /// If a valid frame can be read from the socket, returns the frame. Returns Ok(None)
    /// otherwise.
    pub fn try_read_frame(&mut self) -> std::io::Result<Option<&[u8]>> {
        match self.socket.recv(&mut self.recv_buffer) {
            Ok(frame_len) => {
                let frame_bytes = &self.recv_buffer[..frame_len];
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
    pub fn wait_for_frame(
        &mut self,
        timeout: Option<time::Duration>,
    ) -> std::io::Result<Option<&[u8]>> {
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

    pub fn local_addr(&self) -> net::SocketAddr {
        self.local_addr
    }

    pub fn peer_addr(&self) -> net::SocketAddr {
        self.peer_addr
    }
}

pub fn new_connected<A, B>(
    bind_address: A,
    connect_address: B,
    frame_size_max: usize,
) -> std::io::Result<(ConnectedSocketTx, ConnectedSocketRx)>
where
    A: net::ToSocketAddrs,
    B: net::ToSocketAddrs,
{
    let socket = net::UdpSocket::bind(bind_address)?;
    socket.set_nonblocking(true)?;
    socket.connect(connect_address)?;

    let local_addr = socket.local_addr()?;
    let peer_addr = socket.peer_addr()?;

    let poller = polling::Poller::new()?;

    unsafe {
        poller.add(&socket, polling::Event::readable(SOCKET_POLLING_KEY))?;
    }

    let socket_rc = Arc::new(socket);

    let tx = ConnectedSocketTx {
        socket: Arc::clone(&socket_rc),
    };

    let rx = ConnectedSocketRx {
        socket: socket_rc,
        local_addr,
        peer_addr,
        poller,
        poller_events: polling::Events::new(),
        recv_buffer: vec![0; frame_size_max].into_boxed_slice(),
    };

    Ok((tx, rx))
}
