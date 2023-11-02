use super::buffer;
use super::frame;
use super::SendMode;

mod segment_rx;
mod segment_tx;

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

const SATURATION_MAX: i32 = 1_000_000;

struct TxPrioState {
    counter: i32,
    w_r: u8,
    w_u: u8,
    sat_r: i32,
    sat_u: i32,
}

#[derive(PartialEq)]
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

    pub fn next_mode(&self) -> SendModeType {
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

struct FragmentTxBuffers {
    unrel: buffer::UnreliableTxBuffer,
    rel: buffer::ReliableTxBuffer,
    prio_state: TxPrioState,
}

impl FragmentTxBuffers {
    pub fn new(window_base_id: u32, window_size: u32, fragment_size: usize) -> Self {
        Self {
            unrel: buffer::UnreliableTxBuffer::new(window_base_id, window_size, fragment_size),
            rel: buffer::ReliableTxBuffer::new(window_base_id, window_size, fragment_size),
            prio_state: TxPrioState::new(1, 2, 10_000, 20_000),
        }
    }

    pub fn enqueue(&mut self, packet_bytes: Box<[u8]>, mode: SendMode) {
        match mode {
            SendMode::Reliable => {
                self.rel.push(packet_bytes);
            }
            SendMode::Unreliable(_timeout_ms) => {
                self.unrel.push(packet_bytes);
            }
        }
    }

    pub fn acknowledge(&mut self, unrel_id: Option<u32>, rel_id: Option<u32>) {
        if let Some(unrel_id) = unrel_id {
            self.unrel.acknowledge(unrel_id);
        }

        if let Some(rel_id) = rel_id {
            self.rel.acknowledge(rel_id);
        }
    }

    pub fn data_ready(&self) -> bool {
        self.unrel.peek_sendable().is_some() || self.rel.peek_sendable().is_some()
    }

    pub fn flush(&mut self, writer: &mut frame::serial::FrameWriter) -> usize {
        let mut data_size_total = 0;

        loop {
            let unrel_fragment = self.unrel.peek_sendable();
            let rel_fragment = self.rel.peek_sendable();

            let (fragment, mode) = if unrel_fragment.is_some() && rel_fragment.is_some() {
                match self.prio_state.next_mode() {
                    SendModeType::Unreliable => (unrel_fragment, SendModeType::Unreliable),
                    SendModeType::Reliable => (rel_fragment, SendModeType::Reliable),
                }
            } else if unrel_fragment.is_some() {
                (unrel_fragment, SendModeType::Unreliable)
            } else if rel_fragment.is_some() {
                (rel_fragment, SendModeType::Reliable)
            } else {
                break;
            };

            let fragment = fragment.unwrap();

            let datagram = frame::Datagram {
                id: fragment.id,
                unrel: mode == SendModeType::Unreliable,
                first: fragment.first,
                last: fragment.last,
                data: &fragment.data[fragment.data_range.clone()],
            };

            if writer.write(&datagram) {
                let data_size = datagram.data.len();

                self.prio_state.mark_sent(data_size);
                data_size_total += data_size;

                match mode {
                    SendModeType::Unreliable => {
                        self.unrel.pop_sendable().expect("NANI?");
                    }
                    SendModeType::Reliable => {
                        self.rel.pop_sendable().expect("NANI?");
                    }
                }
            } else {
                break;
            }
        }

        // TODO: Include size of datagram headers here
        return data_size_total;
    }
}

const RESEND_TIMEOUT_MS: u64 = 3000;

pub struct Endpoint {
    // Sender state
    segment_tx: segment_tx::SegmentTx,
    fragment_tx: FragmentTxBuffers,

    cwnd: usize,

    tx_buffer: [u8; 1478],

    rto_timer_set: bool,

    // Receiver state
    segment_rx_buffer: segment_rx::SegmentRx,
    unreliable_rx: buffer::UnreliableRxBuffer,
    reliable_rx: buffer::ReliableRxBuffer,
}

impl Endpoint {
    pub fn new() -> Self {
        let fragment_size = 1478;
        let fragment_window_size = 1024;
        let segment_window_size = 128;

        Self {
            segment_tx: segment_tx::SegmentTx::new(0, segment_window_size),
            fragment_tx: FragmentTxBuffers::new(0, fragment_window_size, fragment_size),
            cwnd: 15000,
            tx_buffer: [0; 1478],
            rto_timer_set: false,
            segment_rx_buffer: segment_rx::SegmentRx::new(0, segment_window_size, fragment_size),
            unreliable_rx: buffer::UnreliableRxBuffer::new(0, fragment_window_size, fragment_size),
            reliable_rx: buffer::ReliableRxBuffer::new(0, fragment_size),
        }
    }

    pub fn init<C>(&mut self, _now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        ctx.on_connect();
    }

    pub fn send<C>(&mut self, packet_bytes: Box<[u8]>, mode: SendMode, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        self.fragment_tx.enqueue(packet_bytes, mode);

        self.actual_flush(false, false, now_ms, ctx);
    }

    fn handle_primary<C>(
        &mut self,
        frame_reader: &mut frame::serial::FrameReader,
        frame_type: frame::FrameType,
        now_ms: u64,
        ctx: &mut C,
    ) where
        C: HostContext,
    {
        let (read_ack, read_data) = match frame_type {
            frame::FrameType::StreamData => (false, true),
            frame::FrameType::StreamAck => (true, false),
            frame::FrameType::StreamDataAck => (true, true),
            _ => panic!("NANI?"),
        };

        let mut ack_unrel = false;
        let mut ack_rel = false;

        if read_ack {
            if let Some(stream_ack) = frame_reader.read::<frame::StreamAck>() {
                if self.segment_tx.acknowledge(stream_ack.data_id, 0x1F, false) {
                    println!("drop detected");
                }

                self.fragment_tx
                    .acknowledge(stream_ack.unrel_id, stream_ack.rel_id);

                if self.rto_timer_set {
                    if stream_ack.data_id == self.segment_tx.next_id() {
                        // All caught up
                        ctx.unset_timer(TimerName::Rto);
                        self.rto_timer_set = false;
                    } else {
                        // More acks expected
                        ctx.set_timer(TimerName::Rto, now_ms + RESEND_TIMEOUT_MS);
                    }
                }
            } else {
                // Truncated packet?
                return;
            }
        }

        if read_data {
            if let Some(stream_data_header) = frame_reader.read::<frame::StreamDataHeader>() {
                let data_id = stream_data_header.id;
                let data_bytes = frame_reader.remaining_bytes();

                self.segment_rx_buffer.receive(
                    data_id,
                    false,
                    data_bytes,
                    |stream_data: &[u8]| {
                        let mut frame_reader = frame::serial::EzReader::new(stream_data);

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
                    },
                );

                // TODO: Set the receive timer if there are frames in the receive buffer
            }
        }

        self.actual_flush(ack_unrel, ack_rel, now_ms, ctx);
    }

    pub fn handle_frame<C>(&mut self, frame_bytes: &[u8], now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        if let Some((mut frame_reader, frame_type)) = frame::serial::FrameReader::new(frame_bytes) {
            match frame_type {
                frame::FrameType::StreamData
                | frame::FrameType::StreamAck
                | frame::FrameType::StreamDataAck => {
                    self.handle_primary(&mut frame_reader, frame_type, now_ms, ctx)
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
                debug_assert!(self.rto_timer_set);

                // TODO: Is it correct to assume more data can be sent, considering an ack has not
                // been received that would have otherwise advanced send windows?

                // TODO: Should a sync frame be sent instead of data?

                // TODO: Should the reliable fragment queue be reset here?

                self.actual_flush(false, false, now_ms, ctx);

                ctx.set_timer(TimerName::Rto, now_ms + RESEND_TIMEOUT_MS);
            }
            TimerName::Receive => {
                println!("Receive timer expired!");
            }
        }
    }

    fn actual_flush<C>(&mut self, ack_unrel: bool, ack_rel: bool, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        let mut send_ack = ack_unrel || ack_rel;

        loop {
            /*
            println!(
                "U Total: {}  R Total: {}  U/R Ratio: {}",
                self.u_total,
                self.r_total,
                self.u_total as f64 / self.r_total as f64
            );
            */

            let frame_window_limited = !self.segment_tx.can_send();
            let congestion_window_limited = self.segment_tx.bytes_in_transit() >= self.cwnd;
            let data_ready = self.fragment_tx.data_ready();

            let send_data = !frame_window_limited && !congestion_window_limited && data_ready;

            if !send_data {
                println!(
                    "can't send data: {}{}{}",
                    if frame_window_limited { 'f' } else { '-' },
                    if congestion_window_limited { 'c' } else { '-' },
                    if !data_ready { 'd' } else { '-' }
                );
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
                    let data_id = self.segment_rx_buffer.next_expected_id();

                    let unrel_id = if ack_unrel {
                        Some(self.unreliable_rx.next_expected_id())
                    } else {
                        None
                    };

                    let rel_id = if ack_rel {
                        Some(self.reliable_rx.next_expected_id())
                    } else {
                        None
                    };

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
                    let header = frame::StreamDataHeader {
                        id: self.segment_tx.next_id(),
                    };

                    frame_writer.write(&header);

                    let size_written = self.fragment_tx.flush(&mut frame_writer);

                    debug_assert!(size_written > 0);

                    self.segment_tx.mark_sent(size_written);

                    // Acks are expected, set the resend timeout if not already
                    if !self.rto_timer_set {
                        ctx.set_timer(TimerName::Rto, now_ms + RESEND_TIMEOUT_MS);
                        self.rto_timer_set = true;
                    }
                }

                let frame = frame_writer.finalize();

                ctx.send_frame(frame);
            } else {
                break;
            }
        }
    }

    pub fn flush<C>(&mut self, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        self.actual_flush(false, false, now_ms, ctx);
    }

    pub fn disconnect<C>(&mut self, _now_ms: u64, _ctx: &mut C)
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

        endpoint.actual_flush(true, true, 0, &mut host_ctx);
    }
}
