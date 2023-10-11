use std::collections::VecDeque;

use super::frame;
use super::SendMode;

#[derive(Clone, Copy, Debug)]
pub enum TimerId {
    Rto,
    Receive,
}

pub trait HostContext {
    fn send(&mut self, frame_bytes: &[u8]);
    fn set_timer(&mut self, timer: TimerId, time_ms: u64);
    fn unset_timer(&mut self, timer: TimerId);
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

use std::ops::Range;
use std::rc::Rc;

pub struct Fragment {
    pub id: u32,
    pub first: bool,
    pub last: bool,
    pub packet: Rc<Box<[u8]>>,
    pub range: Range<usize>,
}

pub struct DummyTxQueue {
    buffer: VecDeque<Fragment>,
}

impl DummyTxQueue {
    pub fn new() -> Self {
        let packet = Rc::new(
            (0..1000)
                .map(|_| rand::random::<u8>())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );

        Self {
            buffer: (0..200)
                .map(|_| Fragment {
                    id: 0xBAADBEEF,
                    first: true,
                    last: true,
                    packet: Rc::clone(&packet),
                    range: 0..rand::random::<usize>() % packet.len(),
                })
                .collect::<Vec<_>>()
                .into(),
        }
    }

    pub fn can_send(&self) -> bool {
        !self.buffer.is_empty()
    }

    pub fn peek(&self) -> Option<&Fragment> {
        if let Some(x) = self.buffer.front() {
            return Some(x);
        }
        return None;
    }

    pub fn pop(&mut self) -> Option<Fragment> {
        self.buffer.pop_front()
    }
}

type UnreliableTxQueue = DummyTxQueue;
type ReliableTxQueue = DummyTxQueue;

pub struct Endpoint {
    // Sender state
    tx_window: FrameTxWindow,
    cwnd: usize,

    prio_state: TxPrioState,

    unreliable_tx: UnreliableTxQueue,
    reliable_tx: ReliableTxQueue,

    u_total: usize,
    r_total: usize,

    tx_buffer: [u8; 1478],

    // Receiver state
    rx_packets: VecDeque<Box<[u8]>>,
}

impl Endpoint {
    pub fn new() -> Self {
        Self {
            tx_window: FrameTxWindow::new(0, 20),
            cwnd: 15000,

            prio_state: TxPrioState::new(1, 2, 10_000, 20_000),

            unreliable_tx: UnreliableTxQueue::new(),
            reliable_tx: ReliableTxQueue::new(),

            u_total: 0,
            r_total: 0,

            tx_buffer: [0; 1478],

            rx_packets: VecDeque::new(),
        }
    }

    pub fn enqueue_packet(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        /*
        match mode {
            SendMode::Reliable => {
                self.reliable_queue.push_back(packet_bytes.into());
            }
            SendMode::Unreliable(timeout_ms) => {
                self.unreliable_queue.push_back(packet_bytes.into());
            }
        }
        */
    }

    pub fn pop_packet(&mut self) -> Option<Box<[u8]>> {
        self.rx_packets.pop_front()
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

                if let Some(unrel_id) = stream_ack.unrel_id {}

                if let Some(rel_id) = stream_ack.rel_id {}
            }

            // TODO: When wouldn't we try to send data?
            try_send_data = true;
        }

        if read_data {
            if let Some(stream_data_header) = frame_reader.read::<frame::StreamDataHeader>() {
                // TODO: Validate data ID

                while let Some(datagram) = frame_reader.read::<frame::Datagram>() {
                    if datagram.unrel {
                        ack_unrel = true;
                    } else {
                        ack_rel = true;
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

    pub fn handle_timer<C>(&mut self, timer: TimerId, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
    }

    fn actual_flush<C>(&mut self, ack_unrel: bool, ack_rel: bool, try_send_data: bool, ctx: &mut C)
    where
        C: HostContext,
    {
        let mut send_ack = ack_unrel || ack_rel;

        'frame_loop: loop {
            println!(
                "U Total: {}  R Total: {}  U/R Ratio: {}",
                self.u_total,
                self.r_total,
                self.u_total as f64 / self.r_total as f64
            );

            let frame_window_limited = !self.tx_window.can_send();
            let congestion_window_limited = self.tx_window.bytes_in_transit() >= self.cwnd;
            let data_ready =
                self.reliable_tx.peek().is_some() || self.unreliable_tx.peek().is_some();

            let send_data =
                try_send_data && !frame_window_limited && !congestion_window_limited && data_ready;

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
                    let data_id = 0x44556677;

                    let unrel_id = if ack_unrel { Some(0xAABBCCDD) } else { None };

                    let rel_id = if ack_rel { Some(0x11223344) } else { None };

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
                    let header = frame::StreamDataHeader { id: 0 };

                    frame_writer.write(&header);

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
                                    if let Some(fragment) = self.unreliable_tx.peek() {
                                        let size = fragment.range.len();

                                        let datagram = frame::Datagram {
                                            id: fragment.id,
                                            unrel: true,
                                            first: fragment.first,
                                            last: fragment.last,
                                            data: &fragment.packet[fragment.range.clone()],
                                        };

                                        if !frame_writer.write(&datagram) {
                                            // Frame finished, send what we have
                                            break 'outer;
                                        }

                                        self.unreliable_tx.pop().expect("NANI?");

                                        self.prio_state.mark_sent(size);
                                        self.u_total += size;
                                        no_data = false;

                                        println!("U {size}");

                                        // Query prio state before sending next fragment
                                        break 'inner;
                                    }
                                }
                                SendModeType::Reliable => {
                                    if let Some(fragment) = self.reliable_tx.peek() {
                                        let size = fragment.range.len();

                                        let datagram = frame::Datagram {
                                            id: fragment.id,
                                            unrel: false,
                                            first: fragment.first,
                                            last: fragment.last,
                                            data: &fragment.packet[fragment.range.clone()],
                                        };

                                        if !frame_writer.write(&datagram) {
                                            // Frame finished, send what we have
                                            break 'outer;
                                        }

                                        self.reliable_tx.pop().expect("NANI?");

                                        self.prio_state.mark_sent(size);
                                        self.r_total += size;
                                        no_data = false;

                                        println!("R {size}");

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
                }

                let frame = frame_writer.finalize();

                ctx.send(frame);
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

    pub fn is_closed(&self) -> bool {
        false
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
        fn send(&mut self, frame_bytes: &[u8]) {
            println!("send {:02X?}", frame_bytes);
        }

        fn set_timer(&mut self, timer: TimerId, time_ms: u64) {
            println!("Set timer!");
        }

        fn unset_timer(&mut self, timer: TimerId) {
            println!("Unset timer!");
        }
    }

    #[test]
    fn tx_state() {
        let mut host_ctx = MockHostContext::new();
        let mut endpoint = Endpoint::new();

        endpoint.actual_flush(true, true, true, &mut host_ctx);
    }
}
