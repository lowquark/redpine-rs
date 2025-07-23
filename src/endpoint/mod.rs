use super::buffer;
use super::frame;
use super::SendMode;

mod cc;
pub mod prio;
mod segment_rx;
mod segment_tx;

const DISCONNECT_TIMEOUT_MS: u64 = 10_000;

const FRAME_SIZE_MAX: usize = 1472;
const SEGMENT_WINDOW_SIZE: u32 = 4096;
const FRAGMENT_SIZE: usize = 1024;
const FRAGMENT_WINDOW_SIZE: u32 = 4096;

const RTO_INITIAL_MS: u64 = 1_000;
const RTO_MAX_MS: u64 = 60_000;
const RTO_MIN_MS: u64 = 1_000;

const ALPHA: f32 = 0.125;
const BETA: f32 = 0.25;
const G_MS: f32 = 4.0;
const K: f32 = 4.0;

/// Parameters for unreliable / reliable channel prioritization.
#[derive(Clone)]
pub struct ChannelBalanceConfig {
    /// Relative weight with which to send unreliable packets.
    ///
    /// Minimum value: 1 \
    /// Maximum value: 256 \
    /// Default value: 1
    ///
    /// *Note*: The product of weights may not exceed 256
    /// ([CHANNEL_WEIGHT_MAX](crate::CHANNEL_WEIGHT_MAX)).
    pub unreliable_weight: u32,

    /// Relative weight with which to send reliable packets.
    ///
    /// Minimum value: 1 \
    /// Maximum value: 256 \
    /// Default value: 1
    ///
    /// *Note*: The product of weights may not exceed 256
    /// ([CHANNEL_WEIGHT_MAX](crate::CHANNEL_WEIGHT_MAX)).
    pub reliable_weight: u32,

    /// Defines the maximum amount of data sent by a channel after an idle period. If a channel has
    /// a weight of 2, it may send 2 times this value in a burst, and so on.
    ///
    /// Minimum value: 1 \
    /// Maximum value: 65535 * 255 ([CHANNEL_BURST_SIZE_MAX](crate::CHANNEL_BURST_SIZE_MAX)) \
    /// Default value: 10_000
    ///
    /// *Note*: This value determines the time horizon over which channels are prioritized. If this
    /// value is too low, all channels will send with an effectively equal weight. If this value is
    /// too high, an idle channel will dominate the stream once it resumes sending.
    pub burst_size: usize,
}

pub enum TimeoutAction {
    Continue,
    Terminate,
}

impl ChannelBalanceConfig {
    pub(super) fn validate(&self) {
        // Hackily construct the underlying config type, which performs its own assertions
        let _: prio::Config = self.clone().into();
    }
}

impl Into<prio::Config> for ChannelBalanceConfig {
    fn into(self) -> prio::Config {
        prio::Config::new(
            &[self.unreliable_weight, self.reliable_weight, 1, 1],
            self.burst_size,
        )
    }
}

impl Default for ChannelBalanceConfig {
    fn default() -> Self {
        Self {
            unreliable_weight: 1,
            reliable_weight: 1,
            burst_size: 10_000,
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub timeout_time_ms: u64,
    pub prio_config: prio::Config,
}

pub trait HostContext {
    // Called to send a frame to the remote host.
    fn send_frame(&mut self, frame_bytes: &[u8]);

    // Called to set the rto timer
    fn set_rto_timer(&mut self, time_ms: u64);

    // Called to unset the rto timer
    fn unset_rto_timer(&mut self);

    // Called when the connection has been successfully initialized
    fn on_connect(&mut self);

    // Called when the connection has been terminated gracefully
    fn on_disconnect(&mut self);

    // Called when a packet has been received from the remote host
    fn on_receive(&mut self, packet_bytes: Box<[u8]>);

    // Called when the connection has been terminated due to a timeout
    fn on_timeout(&mut self);
}

struct FragmentTxBuffers {
    unrel: buffer::UnreliableTxBuffer,
    rel: buffer::ReliableTxBuffer,
    prio: prio::Prio,
}

impl FragmentTxBuffers {
    pub fn new(
        window_base_id: u32,
        window_size: u32,
        fragment_size: usize,
        prio_config: prio::Config,
    ) -> Self {
        Self {
            unrel: buffer::UnreliableTxBuffer::new(window_base_id, fragment_size),
            rel: buffer::ReliableTxBuffer::new(window_base_id, fragment_size, window_size),
            prio: prio::Prio::new(prio_config),
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
                self.unrel
                    .push(packet_bytes, now_ms + timeout_ms as u64 + 1);
            }
        }
    }

    pub fn acknowledge(&mut self, rel_fragment_id: Option<u32>) {
        if let Some(id) = rel_fragment_id {
            self.rel.acknowledge(id);
        }
    }

    pub fn handle_timeout(&mut self) {
        self.rel.resend_all();
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

            let next_channel_idx =
                self.prio
                    .next(unrel_next.is_some(), rel_next.is_some(), false, false);

            if let Some(channel_idx) = next_channel_idx {
                let next = match channel_idx {
                    0 => unrel_next,
                    1 => rel_next,
                    _ => panic!("NANI?"),
                };

                let (id, fragment) = next.unwrap();

                let datagram = frame::Datagram {
                    id,
                    unrel: channel_idx == 0,
                    first: fragment.first,
                    last: fragment.last,
                    data: &fragment.data[fragment.data_range.clone()],
                };

                if writer.write(&datagram) {
                    let data_size = datagram.data.len();

                    self.prio.mark_sent(channel_idx, data_size);
                    data_size_total += data_size;

                    match channel_idx {
                        0 => {
                            self.unrel.pop_sendable().expect("NANI?");
                        }
                        1 => {
                            self.rel.pop_sendable().expect("NANI?");
                        }
                        _ => panic!("NANI?"),
                    }
                } else {
                    // No more datagrams can be written
                    break;
                }
            } else {
                // No more data to send
                break;
            }
        }

        // TODO: Include size of datagram headers here
        return data_size_total;
    }
}

struct RttState {
    srtt_ms: f32,
    rttvar_ms: f32,
}

struct RtoTimerState {
    // Original timer timeout value
    rto_ms: u64,
    // Total timeout time
    rto_sum_ms: u64,
}

struct RttTimerState {
    segment_id: u32,
    send_time_ms: u64,
    expire_time_ms: u64,
}

#[derive(Copy, Clone, PartialEq)]
pub enum StateId {
    PreInit,
    Active,
    Closing,
    Closed,
    Zombie,
}

pub struct Endpoint {
    config: Config,

    state_id: StateId,

    local_nonce: u32,
    remote_nonce: u32,

    // Sender state
    segment_tx: segment_tx::SegmentTx,
    fragment_tx: FragmentTxBuffers,

    cc_state: cc::AimdReno,

    tx_buffer: Box<[u8]>,

    rto_ms: u64,
    rto_timer_state: Option<RtoTimerState>,

    rtt_state: Option<RttState>,
    rtt_timer_state: Option<RttTimerState>,

    // Receiver state
    segment_rx: segment_rx::SegmentRx,
    unreliable_rx: buffer::UnreliableRxBuffer,
    reliable_rx: buffer::ReliableRxBuffer,
}

impl Endpoint {
    pub fn new(config: Config) -> Self {
        let local_nonce = 0;
        let remote_nonce = 0;

        let prio_config = config.prio_config.clone();

        Self {
            config,
            state_id: StateId::PreInit,
            local_nonce,
            remote_nonce,
            segment_tx: segment_tx::SegmentTx::new(0, SEGMENT_WINDOW_SIZE),
            fragment_tx: FragmentTxBuffers::new(
                0,
                FRAGMENT_WINDOW_SIZE,
                FRAGMENT_SIZE,
                prio_config,
            ),
            cc_state: cc::AimdReno::new(FRAGMENT_SIZE),
            tx_buffer: vec![0; FRAME_SIZE_MAX].into_boxed_slice(),
            rto_ms: RTO_INITIAL_MS,
            rto_timer_state: None,
            rtt_state: None,
            rtt_timer_state: None,
            segment_rx: segment_rx::SegmentRx::new(0, SEGMENT_WINDOW_SIZE, FRAME_SIZE_MAX),
            unreliable_rx: buffer::UnreliableRxBuffer::new(0, FRAGMENT_SIZE),
            reliable_rx: buffer::ReliableRxBuffer::new(0, FRAGMENT_SIZE),
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

    pub fn enqueue(&mut self, packet_bytes: Box<[u8]>, mode: SendMode, now_ms: u64) {
        if self.state_id == StateId::Active {
            self.fragment_tx.enqueue(packet_bytes, mode, now_ms);
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

    pub fn disconnect<C>(&mut self, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        if self.state_id == StateId::Active {
            self.enter_closing(now_ms, ctx);
        }
    }

    fn update_rtt(&mut self, segment_id: u32, now_ms: u64) {
        if let Some(ref state) = self.rtt_timer_state {
            if segment_id == state.segment_id.wrapping_add(1) {
                // Source: RFC 6298, Computing TCP's Retransmission Timer

                // This is the ack the RTT timer is looking for
                let rtt_ms = (now_ms - state.send_time_ms) as f32;

                if let Some(ref mut rtt_state) = self.rtt_state {
                    rtt_state.rttvar_ms = (1.0 - BETA) * rtt_state.rttvar_ms
                        + BETA * (rtt_state.srtt_ms - rtt_ms).abs();

                    rtt_state.srtt_ms = (1.0 - ALPHA) * rtt_state.srtt_ms + ALPHA * rtt_ms;
                } else {
                    self.rtt_state = Some(RttState {
                        srtt_ms: rtt_ms,
                        rttvar_ms: rtt_ms / 2.0,
                    });
                }

                let ref mut rtt_state = self.rtt_state.as_mut().unwrap();

                // println!("RTTVAR: {}", rtt_state.rttvar_ms);
                // println!("SRTT: {}", rtt_state.srtt_ms);

                self.rto_ms = (rtt_state.srtt_ms + G_MS.max(K * rtt_state.rttvar_ms)).ceil() as u64;

                self.rto_ms = self.rto_ms.max(RTO_MIN_MS).min(RTO_MAX_MS);

                self.rtt_timer_state = None;
            } else if state.expire_time_ms <= now_ms {
                // RTT timer timed out, better luck next time
                self.rtt_timer_state = None;
            }
        }
    }

    fn update_rto_timer<C>(&mut self, segment_id: u32, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        if let Some(ref mut state) = self.rto_timer_state {
            if segment_id == self.segment_tx.next_id() {
                // All caught up
                ctx.unset_rto_timer();
                self.rto_timer_state = None;
            } else {
                // More acks expected
                // TODO: Only reset when new data has been acknowledged
                ctx.set_rto_timer(now_ms + self.rto_ms);
                state.rto_ms = self.rto_ms;
                state.rto_sum_ms = 0;
            }
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

        let mut send_ack = false;
        let mut ack_rel = false;

        if read_ack {
            if let Some(ack) = frame_reader.read::<frame::StreamAck>() {
                /*
                println!(
                    "RECEIVE ACK {}, {:05b}, {}",
                    ack.segment_id, ack.segment_history, ack.segment_checksum
                );
                */

                // TODO: Only signal when new data has been acknowledged
                self.cc_state.handle_ack();

                if self.segment_tx.acknowledge(
                    ack.segment_id,
                    ack.segment_history.into(),
                    ack.segment_checksum,
                ) {
                    // println!("DROP DETECTED");
                    self.cc_state.handle_drop();
                }

                self.fragment_tx.acknowledge(ack.rel_fragment_id);

                self.update_rtt(ack.segment_id, now_ms);
                self.update_rto_timer(ack.segment_id, now_ms, ctx);
            } else {
                // Truncated packet?
                return;
            }
        }

        if read_data {
            if let Some(segment_header) = frame_reader.read::<frame::StreamSegmentHeader>() {
                let segment_id = segment_header.id;
                let segment_nonce = segment_header.nonce;
                let segment_bytes = frame_reader.remaining_bytes();

                // println!("RECEIVE SEGMENT {} {}", segment_id, segment_nonce);

                self.segment_rx.receive(
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
                            } else {
                                if let Some(packet) = self.reliable_rx.receive(id, fragment) {
                                    ctx.on_receive(packet);
                                }

                                ack_rel = true;
                            }

                            send_ack = true;
                        }
                    },
                );
            }
        }

        self.flush_stream(send_ack, ack_rel, now_ms, ctx);
    }

    fn handle_stream_sync_frame<C>(&mut self, payload_bytes: &[u8], now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        let mut frame_reader = frame::serial::PayloadReader::new(payload_bytes);

        if let Some(sync) = frame_reader.read::<frame::StreamSync>() {
            self.segment_rx
                .sync(sync.next_segment_id, |segment_bytes: &[u8]| {
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
                        } else {
                            if let Some(packet) = self.reliable_rx.receive(id, fragment) {
                                ctx.on_receive(packet);
                            }
                        }
                    }
                });

            // The unreliable rx buffer may be expecting fragment IDs that are made ambiguous by
            // advancing the segment rx window. Thus, any in-progress packets must be cleared in
            // order to avoid combining unrelated fragments. Since all fragments which were not
            // delivered can be assumed to have been dropped (the sender only sends a sync frame
            // after an RTO), this will not artificially introduce data loss.
            self.unreliable_rx.reset();

            // Send an ack in response to sync
            self.flush_stream(true, true, now_ms, ctx);
        }
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

    fn enter_closing<C>(&mut self, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        self.state_id = StateId::Closing;

        ctx.set_rto_timer(now_ms + self.rto_ms);
        self.rto_timer_state = Some(RtoTimerState {
            rto_ms: self.rto_ms,
            rto_sum_ms: 0,
        });

        self.send_close_frame(ctx);
    }

    fn enter_closed<C>(&mut self, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        self.state_id = StateId::Closed;
        ctx.on_disconnect();

        // Use RTO timer to leave closed state
        ctx.set_rto_timer(now_ms + DISCONNECT_TIMEOUT_MS);
        self.rto_timer_state = None;
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
                    self.handle_stream_frame(frame_type, payload_bytes, now_ms, ctx);
                }
                frame::FrameType::StreamSync => {
                    self.handle_stream_sync_frame(payload_bytes, now_ms, ctx);
                }
                frame::FrameType::Close => {
                    if self.validate_close_frame(payload_bytes) {
                        // Acknowledge
                        self.send_close_ack_frame(ctx);
                        // We are now disconnected
                        self.enter_closed(now_ms, ctx);
                    }
                }
                _ => (),
            },
            StateId::Closing => match frame_type {
                frame::FrameType::Close => {
                    if self.validate_close_frame(payload_bytes) {
                        // Acknowledge
                        self.send_close_ack_frame(ctx);
                        // We are now disconnected
                        self.enter_closed(now_ms, ctx);
                    }
                }
                frame::FrameType::CloseAck => {
                    if self.validate_close_ack_frame(payload_bytes) {
                        // We are now disconnected
                        self.enter_closed(now_ms, ctx);
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

    pub fn handle_rto_timer<C>(&mut self, now_ms: u64, ctx: &mut C) -> TimeoutAction
    where
        C: HostContext,
    {
        match self.state_id {
            StateId::Active => {
                let timer_state = self
                    .rto_timer_state
                    .as_mut()
                    .expect("rto timer fired but state is none");

                timer_state.rto_sum_ms += timer_state.rto_ms;

                if timer_state.rto_sum_ms >= self.config.timeout_time_ms {
                    // No acks received for longer than user-specified timeout
                    self.state_id = StateId::Zombie;
                    ctx.on_timeout();

                    // I've seen things you people wouldn't believe... Attack ships on fire off
                    // the shoulder of Orion... I watched C-beams glitter in the dark near the
                    // Tannhäuser Gate. All those moments will be lost in time, like tears in
                    // rain... Time to die.

                    TimeoutAction::Terminate
                } else {
                    self.rto_ms *= 2;
                    self.rto_ms = self.rto_ms.max(RTO_MIN_MS).min(RTO_MAX_MS);

                    ctx.set_rto_timer(now_ms + self.rto_ms);
                    timer_state.rto_ms = self.rto_ms;

                    // Notify congestion control of timeout
                    self.cc_state.handle_timeout();

                    // Resend everything in the reliable queue
                    self.fragment_tx.handle_timeout();

                    // Attempt to resynchronize now that the pipe has drained
                    self.send_stream_sync_frame(self.segment_tx.next_id(), ctx);

                    // Don't waste an opportunity to send data
                    self.flush_stream(false, false, now_ms, ctx);

                    TimeoutAction::Continue
                }
            }
            StateId::Closing => {
                let timer_state = self
                    .rto_timer_state
                    .as_mut()
                    .expect("rto timer fired but state is none");

                timer_state.rto_sum_ms += timer_state.rto_ms;

                if timer_state.rto_sum_ms >= self.config.timeout_time_ms {
                    // No ack received, give up trying to disconnect
                    self.state_id = StateId::Zombie;
                    ctx.on_timeout();

                    // (See previous)
                    TimeoutAction::Terminate
                } else {
                    self.rto_ms *= 2;
                    self.rto_ms = self.rto_ms.max(RTO_MIN_MS).min(RTO_MAX_MS);

                    ctx.set_rto_timer(now_ms + self.rto_ms);
                    timer_state.rto_ms = self.rto_ms;

                    // Send another close frame
                    self.send_close_frame(ctx);

                    TimeoutAction::Continue
                }
            }
            StateId::Closed => {
                // Finished acking resent close requests and/or soaking up stray packets
                self.state_id = StateId::Zombie;

                // (See previous)
                TimeoutAction::Terminate
            }
            _ => TimeoutAction::Continue,
        }
    }

    fn flush_stream<C>(&mut self, mut send_ack: bool, ack_rel: bool, now_ms: u64, ctx: &mut C)
    where
        C: HostContext,
    {
        loop {
            /*
            println!(
                "U Total: {}  R Total: {}  U/R Ratio: {}",
                self.u_total,
                self.r_total,
                self.u_total as f64 / self.r_total as f64
            );
            */

            let cwnd = self.cc_state.cwnd();

            let frame_window_limited = !self.segment_tx.can_send();
            let congestion_window_limited = self.segment_tx.bytes_in_transit() >= cwnd;
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
                    let segment_id = self.segment_rx.next_expected_id();
                    let (segment_history, segment_checksum) = self.segment_rx.next_ack_info();

                    /*
                    println!(
                        "SEND ACK {}, {:05b}, {}",
                        segment_id, segment_history, segment_checksum
                    );
                    */

                    let rel_fragment_id = if ack_rel {
                        Some(self.reliable_rx.next_expected_id())
                    } else {
                        None
                    };

                    let ack = frame::StreamAck {
                        segment_id,
                        segment_history,
                        segment_checksum,
                        rel_fragment_id,
                    };

                    frame_writer.write(&ack);

                    // Only append one ack per flush
                    send_ack = false;
                }

                if send_data {
                    let segment_id = self.segment_tx.next_id();
                    let segment_nonce = self.segment_tx.compute_next_nonce();

                    let header = frame::StreamSegmentHeader {
                        id: segment_id,
                        nonce: segment_nonce,
                    };

                    // println!("SEND SEGMENT {} {}", header.id, header.nonce);

                    frame_writer.write(&header);

                    let size_written = self.fragment_tx.flush(&mut frame_writer, now_ms);

                    debug_assert!(size_written > 0);

                    self.segment_tx.mark_sent(size_written);

                    // Acks are expected, set the resend timeout if not set already
                    if self.rto_timer_state.is_none() {
                        ctx.set_rto_timer(now_ms + self.rto_ms);
                        self.rto_timer_state = Some(RtoTimerState {
                            rto_ms: self.rto_ms,
                            rto_sum_ms: 0,
                        });
                    }

                    if self.rtt_timer_state.is_none() {
                        self.rtt_timer_state = Some(RttTimerState {
                            segment_id,
                            send_time_ms: now_ms,
                            expire_time_ms: now_ms + self.rto_ms,
                        });
                    }
                }

                let frame = frame_writer.finalize();

                ctx.send_frame(frame);
            } else {
                break;
            }
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

    fn send_stream_sync_frame<C>(&mut self, next_segment_id: u32, ctx: &mut C)
    where
        C: HostContext,
    {
        use frame::serial::SimpleFrameWrite;

        let frame = frame::StreamSync { next_segment_id };

        ctx.send_frame(frame.write(&mut self.tx_buffer));
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

        fn set_rto_timer(&mut self, time_ms: u64) {
            println!("set rto timer for {}", time_ms);
        }

        fn unset_rto_timer(&mut self) {
            println!("unset rto timer");
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
        let config = Config {
            timeout_time_ms: 10_000,
            prio_config: prio::Config::new(&[1, 1, 1, 1], 10_000),
        };
        let mut endpoint = Endpoint::new(config);

        endpoint.flush_stream(true, true, 0, &mut host_ctx);
    }
}
