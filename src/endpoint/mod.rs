use super::buffer;
use super::frame;
use super::SendMode;

mod segment_rx;
mod segment_tx;

// TODO: Use generic names like A, B
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

    // Called when the endpoint object itself should be destroyed
    fn destroy_self(&mut self);

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

    pub fn enqueue(&mut self, packet_bytes: Box<[u8]>, mode: SendMode, now_ms: u64) {
        match mode {
            SendMode::Reliable => {
                self.rel.push(packet_bytes);
            }
            SendMode::Unreliable(timeout_ms) => {
                // The timeout field is the minimum amount of time for which a packet should be
                // sent, when possible
                const CLOCK_GRANULARITY_MS: u64 = 1;
                self.unrel.push(packet_bytes, now_ms + CLOCK_GRANULARITY_MS + timeout_ms as u64);
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

    pub fn data_ready(&mut self, now_ms: u64) -> bool {
        self.unrel.pop_expired(now_ms);

        self.unrel.peek_sendable().is_some() || self.rel.peek_sendable().is_some()
    }

    pub fn flush(&mut self, writer: &mut frame::serial::FrameWriter, now_ms: u64) -> usize {
        let mut data_size_total = 0;

        loop {
            self.unrel.pop_expired(now_ms);

            let unrel_next = self.unrel.peek_sendable();
            let rel_next = self.rel.peek_sendable();

            let (mode, next) = if unrel_next.is_some() && rel_next.is_some() {
                match self.prio_state.next_mode() {
                    SendModeType::Unreliable => (SendModeType::Unreliable, unrel_next),
                    SendModeType::Reliable => (SendModeType::Reliable, rel_next),
                }
            } else if unrel_next.is_some() {
                (SendModeType::Unreliable, unrel_next)
            } else if rel_next.is_some() {
                (SendModeType::Reliable, rel_next)
            } else {
                break;
            };

            let (id, fragment) = next.unwrap();

            let datagram = frame::Datagram {
                id,
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

// TODO: RTO calculation
const RESEND_TIMEOUT_MS: u64 = 3000;

const CLOSE_RESEND_TIMEOUT_MS: u64 = 2000;
const DISCONNECT_TIMEOUT_MS: u64 = 10000;

#[derive(Copy, Clone, PartialEq)]
pub enum StateId {
    PreInit,
    Active,
    Closing,
    Closed,
    Zombie,
}

pub struct Endpoint {
    state_id: StateId,

    local_nonce: u32,
    remote_nonce: u32,

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

        let local_nonce = 0;
        let remote_nonce = 0;

        Self {
            state_id: StateId::PreInit,
            local_nonce,
            remote_nonce,
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
        debug_assert!(self.state_id == StateId::PreInit);

        self.state_id = StateId::Active;
        ctx.on_connect();
    }

    pub fn send<C>(&mut self, packet_bytes: Box<[u8]>, mode: SendMode, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        if self.state_id == StateId::Active {
            self.fragment_tx.enqueue(packet_bytes, mode, now_ms);
            self.flush_stream(false, false, now_ms, ctx);
        }
    }

    fn handle_stream_frame<C>(
        &mut self,
        frame_type: frame::FrameType,
        payload_bytes: &[u8],
        now_ms: u64,
        ctx: &mut C,
    ) where
        C: HostContext,
    {
        let mut frame_reader = frame::serial::PayloadReader::new(payload_bytes);

        let (read_ack, read_data) = match frame_type {
            frame::FrameType::StreamSegment => (false, true),
            frame::FrameType::StreamAck => (true, false),
            frame::FrameType::StreamSegmentAck => (true, true),
            _ => panic!("NANI?"),
        };

        let mut ack_unrel = false;
        let mut ack_rel = false;

        if read_ack {
            if let Some(stream_ack) = frame_reader.read::<frame::StreamAck>() {
                println!(
                    "RECEIVE ACK {}, {:05b}, {}",
                    stream_ack.segment_id, stream_ack.segment_history, stream_ack.segment_checksum
                );

                if self.segment_tx.acknowledge(
                    stream_ack.segment_id,
                    stream_ack.segment_history.into(),
                    stream_ack.segment_checksum,
                ) {
                    println!("DROP DETECTED");
                }

                self.fragment_tx
                    .acknowledge(stream_ack.unrel_id, stream_ack.rel_id);

                if self.rto_timer_set {
                    if stream_ack.segment_id == self.segment_tx.next_id() {
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
            if let Some(stream_data_header) = frame_reader.read::<frame::StreamSegmentHeader>() {
                let segment_id = stream_data_header.id;
                let segment_nonce = stream_data_header.nonce;
                let segment_bytes = frame_reader.remaining_bytes();

                println!("RECEIVE SEGMENT {} {}", segment_id, segment_nonce);

                self.segment_rx_buffer.receive(
                    segment_id,
                    segment_nonce,
                    segment_bytes,
                    |segment_bytes: &[u8]| {
                        let mut frame_reader = frame::serial::PayloadReader::new(segment_bytes);

                        while let Some(datagram) = frame_reader.read::<frame::Datagram>() {
                            let unrel = datagram.unrel;

                            let id = datagram.id;

                            let ref fragment = buffer::FragmentRef {
                                first: datagram.first,
                                last: datagram.last,
                                data: datagram.data,
                            };

                            if unrel {
                                if let Some(packet) = self.unreliable_rx.receive(id, fragment) {
                                    ctx.on_receive(packet);
                                }

                                ack_unrel = true;
                            } else {
                                if let Some(packet) = self.reliable_rx.receive(id, fragment) {
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

        self.flush_stream(ack_unrel, ack_rel, now_ms, ctx);
    }

    fn validate_close_frame(&self, payload_bytes: &[u8]) -> bool {
        use frame::serial::SimplePayloadRead;

        if let Some(frame) = frame::CloseFrame::read(payload_bytes) {
            frame.remote_nonce == self.local_nonce
        } else {
            false
        }
    }

    fn validate_close_ack_frame(&self, payload_bytes: &[u8]) -> bool {
        use frame::serial::SimplePayloadRead;

        if let Some(frame) = frame::CloseAckFrame::read(payload_bytes) {
            frame.remote_nonce == self.local_nonce
        } else {
            false
        }
    }

    pub fn handle_frame<C>(
        &mut self,
        frame_type: frame::FrameType,
        payload_bytes: &[u8],
        now_ms: u64,
        ctx: &mut C,
    ) where
        C: HostContext,
    {
        match self.state_id {
            StateId::Active => match frame_type {
                frame::FrameType::StreamSegment
                | frame::FrameType::StreamAck
                | frame::FrameType::StreamSegmentAck => {
                    self.handle_stream_frame(frame_type, payload_bytes, now_ms, ctx)
                }
                frame::FrameType::Close => {
                    if self.validate_close_frame(payload_bytes) {
                        // Acknowledge
                        self.send_close_ack_frame(ctx);

                        // Unset to prevent spurious timer event
                        ctx.unset_timer(TimerName::Rto);
                        // Set to ensure eventual destruction
                        ctx.set_timer(TimerName::Receive, now_ms + DISCONNECT_TIMEOUT_MS);

                        // We are now disconnected
                        self.state_id = StateId::Closed;
                        ctx.on_disconnect();
                    }
                }
                _ => (),
            },
            StateId::Closing => match frame_type {
                frame::FrameType::Close => {
                    if self.validate_close_frame(payload_bytes) {
                        // Acknowledge
                        self.send_close_ack_frame(ctx);

                        // Unset to prevent spurious timer event
                        ctx.unset_timer(TimerName::Rto);

                        // We are now disconnected
                        self.state_id = StateId::Closed;
                        ctx.on_disconnect();
                    }
                }
                frame::FrameType::CloseAck => {
                    if self.validate_close_ack_frame(payload_bytes) {
                        // Unset to prevent spurious timer event
                        ctx.unset_timer(TimerName::Rto);

                        // We are now disconnected
                        self.state_id = StateId::Closed;
                        ctx.on_disconnect();
                    }
                }
                _ => (),
            },
            StateId::Closed => match frame_type {
                frame::FrameType::Close => {
                    if self.validate_close_frame(payload_bytes) {
                        // Acknowledge further requests until destruction
                        self.send_close_ack_frame(ctx);
                    }
                }
                _ => (),
            },
            _ => (),
        }
    }

    pub fn handle_timer<C>(&mut self, timer: TimerName, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        match self.state_id {
            StateId::Active => match timer {
                TimerName::Rto => {
                    println!("STREAM RESEND TIMEOUT");
                    debug_assert!(self.rto_timer_set);

                    // TODO: Is it correct to assume more data can be sent, considering an ack
                    // has not been received that would have otherwise advanced send windows?

                    // TODO: Should a sync frame be sent instead of data?

                    // TODO: Should the reliable fragment queue be reset here?

                    // TODO: Time out connection

                    self.flush_stream(false, false, now_ms, ctx);

                    ctx.set_timer(TimerName::Rto, now_ms + RESEND_TIMEOUT_MS);
                }
                TimerName::Receive => {
                    println!("STREAM RECEIVE FLUSH TIMEOUT");
                }
            },
            StateId::Closing => match timer {
                TimerName::Rto => {
                    // No ack received within resend timeout
                    println!("CLOSING TIMEOUT");
                    self.send_close_frame(ctx);

                    ctx.set_timer(TimerName::Rto, now_ms + CLOSE_RESEND_TIMEOUT_MS);
                }
                TimerName::Receive => {
                    // Disconnection timed out entirely
                    println!("CLOSING FINAL TIMEOUT");
                    self.state_id = StateId::Zombie;
                    ctx.on_timeout();

                    ctx.destroy_self();
                }
            },
            StateId::Closed => match timer {
                TimerName::Rto => (),
                TimerName::Receive => {
                    // Finished acking resent close requests and/or soaking up stray packets
                    println!("FIN");
                    self.state_id = StateId::Zombie;

                    ctx.destroy_self();
                }
            },
            _ => (),
        }
    }

    fn flush_stream<C>(&mut self, ack_unrel: bool, ack_rel: bool, now_ms: u64, ctx: &mut C)
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
            let data_ready = self.fragment_tx.data_ready(now_ms);

            let send_data = !frame_window_limited && !congestion_window_limited && data_ready;

            /*
            if !send_data {
                println!(
                    "can't send data: {}{}{}",
                    if frame_window_limited { 'f' } else { '-' },
                    if congestion_window_limited { 'c' } else { '-' },
                    if !data_ready { 'd' } else { '-' }
                );
            }
            */

            if send_ack || send_data {
                let frame_type = match (send_ack, send_data) {
                    (false, true) => frame::FrameType::StreamSegment,
                    (true, false) => frame::FrameType::StreamAck,
                    (true, true) => frame::FrameType::StreamSegmentAck,
                    _ => panic!("NANI?"),
                };

                let mut frame_writer =
                    frame::serial::FrameWriter::new(&mut self.tx_buffer, frame_type);

                if send_ack {
                    let segment_id = self.segment_rx_buffer.next_expected_id();
                    let (segment_history, segment_checksum) =
                        self.segment_rx_buffer.next_ack_info();

                    println!(
                        "SEND ACK {}, {:05b}, {}",
                        segment_id, segment_history, segment_checksum
                    );

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
                        segment_id,
                        segment_history,
                        segment_checksum,
                        unrel_id,
                        rel_id,
                    };

                    frame_writer.write(&ack);

                    // Only append one ack per flush
                    send_ack = false;
                }

                if send_data {
                    let header = frame::StreamSegmentHeader {
                        id: self.segment_tx.next_id(),
                        nonce: self.segment_tx.compute_next_nonce(),
                    };

                    println!("SEND SEGMENT {} {}", header.id, header.nonce);

                    frame_writer.write(&header);

                    let size_written = self.fragment_tx.flush(&mut frame_writer, now_ms);

                    debug_assert!(size_written > 0);

                    self.segment_tx.mark_sent(size_written);

                    // Acks are expected, set the resend timeout if not set already
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
        if self.state_id == StateId::Active {
            self.flush_stream(false, false, now_ms, ctx);
        }
    }

    fn send_close_frame<C>(&mut self, ctx: &mut C)
    where
        C: HostContext,
    {
        use frame::serial::SimpleFrameWrite;

        let frame = frame::CloseFrame {
            remote_nonce: self.remote_nonce,
        };

        ctx.send_frame(frame.write(&mut self.tx_buffer));
    }

    fn send_close_ack_frame<C>(&mut self, ctx: &mut C)
    where
        C: HostContext,
    {
        use frame::serial::SimpleFrameWrite;

        let frame = frame::CloseAckFrame {
            remote_nonce: self.remote_nonce,
        };

        ctx.send_frame(frame.write(&mut self.tx_buffer));
    }

    pub fn disconnect<C>(&mut self, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        if self.state_id == StateId::Active {
            self.state_id = StateId::Closing;

            self.send_close_frame(ctx);

            // Set to resend close resquests
            ctx.set_timer(TimerName::Rto, now_ms + CLOSE_RESEND_TIMEOUT_MS);
            // Set to ensure eventual destruction
            ctx.set_timer(TimerName::Receive, now_ms + DISCONNECT_TIMEOUT_MS);
        }
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

        fn destroy_self(&mut self) {
            println!("destroy self");
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

        endpoint.flush_stream(true, true, 0, &mut host_ctx);
    }
}
