use std::collections::VecDeque;

use super::buffer;
use super::frame;
use super::SendMode;

#[derive(Clone, Copy, Debug)]
pub enum TimerName {
    // Resend time out (no ack from remote)
    Rto,
    // When to skip a hole in the reorder buffer
    Receive,
}

pub trait HostContext {
    // Called to send a frame to the remote host.
    fn send_frame(&mut self, frame_bytes: &[u8]);

    // Called to set the given timer
    fn set_timer(&mut self, timer: TimerName, time_ms: u64);

    // Called to unset the given timer
    fn unset_timer(&mut self, timer: TimerName);

    // Called when the connection has been successfully initialized
    fn on_connect(&mut self);

    // Called when the connection has been terminated gracefully
    fn on_disconnect(&mut self);

    // Called when a packet has been received from the remote host
    fn on_receive(&mut self, packet_bytes: Box<[u8]>);

    // Called when the connection has been terminated due to a timeout
    fn on_timeout(&mut self);
}

struct FrameTxWindow {
    //   base    next    base+size
    //   v       v       v
    // --########________--------> frame IDs
    //
    // #: in transit
    // _: sendable
    base_id: u32,
    next_id: u32,
    size: u32,

    // sizes of [base, next)
    transit_buf: VecDeque<usize>, // TODO: Static buffer
    transit_total: usize,
}

impl FrameTxWindow {
    pub fn new(base_id: u32, size: u32) -> Self {
        Self {
            base_id,
            next_id: base_id,
            size,

            transit_buf: VecDeque::new(),
            transit_total: 0,
        }
    }

    fn next_delta(&self) -> u32 {
        self.next_id.wrapping_sub(self.base_id)
    }

    pub fn next_id(&self) -> u32 {
        self.next_id
    }

    pub fn can_send(&self) -> bool {
        self.next_delta() < self.size
    }

    pub fn mark_sent(&mut self, size: usize) {
        assert!(self.can_send());

        self.next_id = self.next_id.wrapping_add(1);
        self.transit_buf.push_back(size);
        self.transit_total += size;
    }

    pub fn bytes_in_transit(&self) -> usize {
        self.transit_total
    }

    /// Acknowledges previously sent frames by indicating the receiver's next expected frame
    pub fn acknowledge(&mut self, frame_id: u32) -> bool {
        let frame_delta = frame_id.wrapping_sub(self.base_id);

        // Do nothing unless:
        //   * frame_id represents an advancement,
        //   * frame_id acknowledges at most the entire current window, and
        //   * frame_id does not exceed the next frame ID which will be sent
        if frame_delta > 0 && frame_delta <= self.size && frame_delta <= self.next_delta() {
            self.base_id = frame_id;

            for size in self
                .transit_buf
                .drain(0..usize::try_from(frame_delta).unwrap())
            {
                self.transit_total -= size;
            }

            true
        } else {
            false
        }
    }
}

struct FrameRxWindow {
    base_id: u32,
    size: u32,
}

impl FrameRxWindow {
    pub fn new(base_id: u32, size: u32) -> Self {
        Self {
            base_id,
            size,
        }
    }

    pub fn can_receive(&self, id: u32) -> bool {
        id.wrapping_sub(self.base_id) < self.size
    }

    pub fn mark_received(&mut self, id: u32) {
        debug_assert!(self.can_receive(id));

        self.base_id = id.wrapping_add(1);
    }

    pub fn next_expected_id(&self) -> u32 {
        self.base_id
    }
}

struct TxPrioState {
    counter: i32,
    w_r: u8,
    w_u: u8,
    sat_r: i32,
    sat_u: i32,
}

const SATURATION_MAX: i32 = 1_000_000;

enum SendModeType {
    Unreliable,
    Reliable,
}

impl TxPrioState {
    pub fn new(w_r: u8, w_u: u8, sat_r: i32, sat_u: i32) -> Self {
        assert!(w_r != 0);
        assert!(w_u != 0);

        assert!(sat_r > 0 && sat_r <= SATURATION_MAX);
        assert!(sat_u > 0 && sat_u <= SATURATION_MAX);

        Self {
            counter: 0,
            w_r,
            w_u,
            sat_r,
            sat_u,
        }
    }

    pub fn next_mode(&mut self) -> SendModeType {
        if self.counter >= 0 {
            SendModeType::Unreliable
        } else {
            SendModeType::Reliable
        }
    }

    pub fn mark_sent(&mut self, size: usize) {
        if self.counter >= 0 {
            self.counter = self
                .counter
                .saturating_sub(i32::try_from(size * usize::from(self.w_r)).unwrap());

            if self.counter < -self.sat_r {
                self.counter = -self.sat_r;
            }
        } else {
            self.counter = self
                .counter
                .saturating_add(i32::try_from(size * usize::from(self.w_u)).unwrap());

            if self.counter > self.sat_u {
                self.counter = self.sat_u;
            }
        }
    }
}

pub struct Endpoint {
    // Sender state
    tx_window: FrameTxWindow,
    rx_window: FrameRxWindow,

    cwnd: usize,

    prio_state: TxPrioState,

    unreliable_tx: buffer::UnreliableTxBuffer,
    unreliable_rx: buffer::UnreliableRxBuffer,

    reliable_tx: buffer::ReliableTxBuffer,
    reliable_rx: buffer::ReliableRxBuffer,

    u_total: usize,
    r_total: usize,

    tx_buffer: [u8; 1478],

    // Receiver state
    rx_packets: VecDeque<Box<[u8]>>,
}

impl Endpoint {
    pub fn new() -> Self {
        let fragment_size = 1478;
        let fragment_window_size = 1024;

        Self {
            tx_window: FrameTxWindow::new(0, 20),
            rx_window: FrameRxWindow::new(0, 20),
            cwnd: 15000,

            prio_state: TxPrioState::new(1, 2, 10_000, 20_000),

            unreliable_tx: buffer::UnreliableTxBuffer::new(0, fragment_window_size, fragment_size),
            unreliable_rx: buffer::UnreliableRxBuffer::new(0, fragment_window_size, fragment_size),

            reliable_tx: buffer::ReliableTxBuffer::new(0, fragment_window_size, fragment_size),
            reliable_rx: buffer::ReliableRxBuffer::new(0, fragment_size),

            u_total: 0,
            r_total: 0,

            tx_buffer: [0; 1478],

            rx_packets: VecDeque::new(),
        }
    }

    pub fn init<C>(&mut self, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        ctx.on_connect();

        // Set a dummy timer, just for fun
        ctx.set_timer(TimerName::Rto, now_ms + 500);
    }

    pub fn send<C>(&mut self, packet_bytes: Box<[u8]>, mode: SendMode, ctx: &mut C)
    where
        C: HostContext,
    {
        match mode {
            SendMode::Reliable => {
                self.reliable_tx.push(packet_bytes);
            }
            SendMode::Unreliable(timeout_ms) => {
                self.unreliable_tx.push(packet_bytes);
            }
        }

        self.actual_flush(false, false, true, ctx);
    }

    fn handle_primary<C>(
        &mut self,
        frame_reader: &mut frame::serial::FrameReader,
        frame_type: frame::FrameType,
        ctx: &mut C,
    ) where
        C: HostContext,
    {
        let mut read_ack = false;
        let mut read_data = false;

        let (read_ack, read_data) = match frame_type {
            frame::FrameType::StreamData => (false, true),
            frame::FrameType::StreamAck => (true, false),
            frame::FrameType::StreamDataAck => (true, true),
            _ => panic!("NANI?"),
        };

        let mut try_send_data = false;
        let mut ack_unrel = false;
        let mut ack_rel = false;

        if read_ack {
            if let Some(stream_ack) = frame_reader.read::<frame::StreamAck>() {
                self.tx_window.acknowledge(stream_ack.data_id);

                if let Some(unrel_id) = stream_ack.unrel_id {
                    self.unreliable_tx.acknowledge(unrel_id);
                }

                if let Some(rel_id) = stream_ack.rel_id {
                    self.reliable_tx.acknowledge(rel_id);
                }
            }

            // TODO: When wouldn't we try to send data?
            try_send_data = true;
        }

        if read_data {
            if let Some(stream_data_header) = frame_reader.read::<frame::StreamDataHeader>() {
                // TODO: Permit out-of-order frames

                if self.rx_window.can_receive(stream_data_header.id) {
                    self.rx_window.mark_received(stream_data_header.id);

                    while let Some(datagram) = frame_reader.read::<frame::Datagram>() {
                        let unrel = datagram.unrel;

                        let ref fragment = buffer::FragmentRef {
                            id: datagram.id,
                            first: datagram.first,
                            last: datagram.last,
                            data: datagram.data,
                        };

                        if unrel {
                            if let Some(packet) = self.unreliable_rx.receive(fragment) {
                                ctx.on_receive(packet);
                            }

                            ack_unrel = true;
                        } else {
                            if let Some(packet) = self.reliable_rx.receive(fragment) {
                                ctx.on_receive(packet);
                            }

                            ack_rel = true;
                        }
                    }
                }
            }
        }

        self.actual_flush(ack_unrel, ack_rel, try_send_data, ctx);
    }

    pub fn handle_frame<C>(&mut self, frame_bytes: &[u8], ctx: &mut C)
    where
        C: HostContext,
    {
        if let Some((mut frame_reader, frame_type)) = frame::serial::FrameReader::new(frame_bytes) {
            match frame_type {
                frame::FrameType::StreamData => {
                    self.handle_primary(&mut frame_reader, frame_type, ctx)
                }
                frame::FrameType::StreamAck => {
                    self.handle_primary(&mut frame_reader, frame_type, ctx)
                }
                frame::FrameType::StreamDataAck => {
                    self.handle_primary(&mut frame_reader, frame_type, ctx)
                }
                _ => (),
            }
        }
    }

    pub fn handle_timer<C>(&mut self, timer: TimerName, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        match timer {
            TimerName::Rto => {
                println!("Rto timer expired!");
            }
            TimerName::Receive => {}
        }
    }

    fn actual_flush<C>(&mut self, ack_unrel: bool, ack_rel: bool, try_send_data: bool, ctx: &mut C)
    where
        C: HostContext,
    {
        let mut send_ack = ack_unrel || ack_rel;

        'frame_loop: loop {
            /*
            println!(
                "U Total: {}  R Total: {}  U/R Ratio: {}",
                self.u_total,
                self.r_total,
                self.u_total as f64 / self.r_total as f64
            );
            */

            let frame_window_limited = !self.tx_window.can_send();
            let congestion_window_limited = self.tx_window.bytes_in_transit() >= self.cwnd;
            let unrel_data_ready = self.unreliable_tx.peek_sendable().is_some();
            let rel_data_ready = self.reliable_tx.peek_sendable().is_some();
            let data_ready = unrel_data_ready | rel_data_ready;

            let send_data =
                try_send_data && !frame_window_limited && !congestion_window_limited && data_ready;

            if !send_data {
                println!("can't send data: {}{}{}",
                         if frame_window_limited { 'f' } else { '-' },
                         if congestion_window_limited { 'c' } else { '-' },
                         if !data_ready { 'd' } else { '-' });
            }

            if send_ack || send_data {
                let frame_type = match (send_ack, send_data) {
                    (false, true) => frame::FrameType::StreamData,
                    (true, false) => frame::FrameType::StreamAck,
                    (true, true) => frame::FrameType::StreamDataAck,
                    _ => panic!("NANI?"),
                };

                let mut frame_writer =
                    frame::serial::FrameWriter::new(&mut self.tx_buffer, frame_type).unwrap();

                if send_ack {
                    let data_id = self.rx_window.next_expected_id();

                    let unrel_id = if ack_unrel { Some(self.unreliable_rx.next_expected_id()) } else { None };

                    let rel_id = if ack_rel { Some(self.reliable_rx.next_expected_id()) } else { None };

                    let ack = frame::StreamAck {
                        data_id,
                        unrel_id,
                        rel_id,
                    };

                    frame_writer.write(&ack);

                    // Only append one ack per flush
                    send_ack = false;
                }

                if send_data {
                    let header = frame::StreamDataHeader { id: self.tx_window.next_id() };

                    frame_writer.write(&header);

                    let mut data_size_total = 0;

                    'outer: loop {
                        let order = match self.prio_state.next_mode() {
                            SendModeType::Unreliable => {
                                [SendModeType::Unreliable, SendModeType::Reliable]
                            }
                            SendModeType::Reliable => {
                                [SendModeType::Reliable, SendModeType::Unreliable]
                            }
                        };

                        let mut no_data = true;

                        'inner: for mode in order {
                            match mode {
                                SendModeType::Unreliable => {
                                    if let Some(fragment) = self.unreliable_tx.peek_sendable() {
                                        let size = fragment.data_range.len();

                                        let datagram = frame::Datagram {
                                            id: fragment.id,
                                            unrel: true,
                                            first: fragment.first,
                                            last: fragment.last,
                                            data: &fragment.data[fragment.data_range.clone()],
                                        };

                                        if !frame_writer.write(&datagram) {
                                            // Frame finished, send what we have
                                            break 'outer;
                                        }

                                        self.unreliable_tx.pop_sendable().expect("NANI?");

                                        self.prio_state.mark_sent(size);
                                        self.u_total += size;
                                        data_size_total += size;
                                        no_data = false;

                                        // println!("U {size}");

                                        // Query prio state before sending next fragment
                                        break 'inner;
                                    }
                                }
                                SendModeType::Reliable => {
                                    if let Some(fragment) = self.reliable_tx.peek_sendable() {
                                        let size = fragment.data_range.len();

                                        let datagram = frame::Datagram {
                                            id: fragment.id,
                                            unrel: false,
                                            first: fragment.first,
                                            last: fragment.last,
                                            data: &fragment.data[fragment.data_range.clone()],
                                        };

                                        if !frame_writer.write(&datagram) {
                                            // Frame finished, send what we have
                                            break 'outer;
                                        }

                                        self.reliable_tx.pop_sendable().expect("NANI?");

                                        self.prio_state.mark_sent(size);
                                        self.r_total += size;
                                        data_size_total += size;
                                        no_data = false;

                                        // println!("R {size}");

                                        // Query prio state before sending next fragment
                                        break 'inner;
                                    }
                                }
                            }
                        }

                        if no_data {
                            // No more data to be sent
                            break 'outer;
                        }
                    }

                    self.tx_window.mark_sent(data_size_total);
                }

                let frame = frame_writer.finalize();

                ctx.send_frame(frame);
            } else {
                break 'frame_loop;
            }
        }
    }

    pub fn flush<C>(&mut self, ctx: &mut C)
    where
        C: HostContext,
    {
        self.actual_flush(false, false, true, ctx);
    }

    pub fn disconnect<C>(&mut self, ctx: &mut C)
    where
        C: HostContext,
    {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockHostContext {}

    impl MockHostContext {
        fn new() -> Self {
            Self {}
        }
    }

    impl HostContext for MockHostContext {
        fn send_frame(&mut self, frame_bytes: &[u8]) {
            println!("send frame {:02X?}", frame_bytes);
        }

        fn set_timer(&mut self, timer: TimerName, time_ms: u64) {
            println!("set timer {:?} for {}", timer, time_ms);
        }

        fn unset_timer(&mut self, timer: TimerName) {
            println!("unset timer {:?}", timer);
        }

        fn on_connect(&mut self) {
            println!("connect");
        }

        fn on_disconnect(&mut self) {
            println!("disconnect");
        }

        fn on_receive(&mut self, packet_bytes: Box<[u8]>) {
            println!("receive {:02X?}", packet_bytes);
        }

        fn on_timeout(&mut self) {
            println!("disconnect");
        }
    }

    #[test]
    fn tx_state() {
        let mut host_ctx = MockHostContext::new();
        let mut endpoint = Endpoint::new();

        endpoint.actual_flush(true, true, true, &mut host_ctx);
    }
}
