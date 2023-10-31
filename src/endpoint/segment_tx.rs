//            base    next    base+size
//            v       v       v
// -----------########________--------> frame IDs
// *********_*
// ack history
//
// *: acknowledged
// #: in transit
// _: sendable

pub struct SegmentTx {
    base_id: u32,
    next_id: u32,
    size: u32,

    // Ring buffer of segment sizes corresponding to [base, next)
    transit_buf: Box<[usize]>,
    transit_buf_mask: u32,

    // Sum of sizes in transit_buf
    transit_total: usize,

    // Bitfield representing segments acknowledged by receiver
    ack_history: u32,
}

impl SegmentTx {
    pub fn new(base_id: u32, size: u32) -> Self {
        assert!(
            size > 0 && (size - 1) & size == 0,
            "size must be a power of two"
        );

        Self {
            base_id,
            next_id: base_id,
            size,

            transit_buf: vec![0; size as usize].into(),
            transit_buf_mask: size - 1,
            transit_total: 0,

            ack_history: u32::MAX,
        }
    }

    pub fn next_id(&self) -> u32 {
        self.next_id
    }

    pub fn can_send(&self) -> bool {
        self.next_id.wrapping_sub(self.base_id) < self.size
    }

    pub fn mark_sent(&mut self, size: usize) {
        debug_assert!(self.can_send());

        let idx = (self.next_id & self.transit_buf_mask) as usize;
        self.transit_buf[idx] = size;
        self.transit_total += size;

        self.next_id = self.next_id.wrapping_add(1);
    }

    pub fn bytes_in_transit(&self) -> usize {
        self.transit_total
    }

    /// Acknowledges previously sent frames
    pub fn acknowledge(
        &mut self,
        rx_base_id: u32,
        rx_history: u32,
        _rx_history_nonce: bool,
    ) -> bool {
        let ack_delta = rx_base_id.wrapping_sub(self.base_id);

        // The delta ack must not exceed the next ID to be sent, else it is invalid
        if ack_delta <= self.next_id.wrapping_sub(self.base_id) {
            if ack_delta > 0 {
                // Anything between the current base_id and the acknowledged base_id has very likely
                // left the network; subtract from running total
                for i in 0..ack_delta {
                    let idx = (self.base_id.wrapping_add(i) & self.transit_buf_mask) as usize;
                    let size = self.transit_buf[idx];

                    self.transit_total -= size;
                }

                // Advance the segment tx window
                self.base_id = rx_base_id;
            }

            // This history queue incrementally tracks acknowledged segments from the receiver
            // until a drop is detected. A drop is detected when three segments have been received
            // after an unacknowledged segment, or any unacknowledged segment is behind the base ID
            // by ≥32 IDs. When a drop is detected, the queue resets to the default state where all
            // segments have been acknowledged in order to detect the next drop.

            //                     base (prev)    base' (latest ack)
            //                     v              v
            // xxxxxxxx xxxxxxxx xx000000 00000000   self.ack_history
            //                     000000 000yyyyy   rx_history

            const N: u32 = u32::BITS;

            if ack_delta > self.ack_history.leading_ones() {
                // Shifting would cause a zero to be >32 bits behind, that's a drop
                self.ack_history = u32::MAX;
                return true;
            }

            // Shift in zeros corresponding to this delta
            if ack_delta == N {
                self.ack_history = 0;
            } else {
                self.ack_history = self.ack_history.wrapping_shl(ack_delta);
            }

            // TODO: Validate rx_history_nonce against sent nonces

            // Add received history bits to history buffer
            self.ack_history |= rx_history;

            if self.ack_history == u32::MAX {
                // All segments accounted for, not a drop
                return false;
            }

            if self.ack_history.wrapping_shr(N - 1) == 0 {
                // Oldest unacknowledged segment is 32 IDs old, that's a drop
                self.ack_history = u32::MAX;
                return true;
            }

            let leading_ones = self.ack_history.leading_ones();
            debug_assert!(leading_ones < N);

            // Count the ones after the first zero
            let ones_after_zero = self
                .ack_history
                .wrapping_shl(leading_ones)
                .wrapping_shr(leading_ones)
                .count_ones();

            if ones_after_zero >= 3 {
                // Three segments have been reported since the first unacknowledged segment, that's
                // equivalent to a TCP NDUPACK=3 condition
                self.ack_history = u32::MAX;
                return true;
            } else {
                // Not a drop, yet
                return false;
            }
        }

        // Invalid ack, not a drop
        return false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_receive_window() {
        let mut tx = SegmentTx::new(0, 128);

        for i in 0..128 {
            assert_eq!(tx.can_send(), true);
            assert_eq!(tx.next_id(), i);
            assert_eq!(tx.bytes_in_transit(), (i * 5) as usize);
            tx.mark_sent(5);
        }

        assert_eq!(tx.can_send(), false);

        for i in 0..128 {
            assert_eq!(tx.acknowledge(i + 1, 0x1, false), false);
            assert_eq!(tx.bytes_in_transit(), ((128 - i - 1) * 5) as usize);
            assert_eq!(tx.can_send(), true);
        }
    }

    fn new_filled_window(size: usize) -> SegmentTx {
        let mut tx = SegmentTx::new(0, 128);

        for _ in 0..size {
            tx.mark_sent(0);
        }

        tx
    }

    #[test]
    fn drop_detection() {
        const WINDOW_SIZE: usize = 128;

        const DUMMY_NONCE: bool = false;

        // Drop declared if 3 segments are acked after the first unacked segment
        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(5, 0b01001, DUMMY_NONCE), false);

        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(5, 0b01001, DUMMY_NONCE), false);

        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(5, 0b01101, DUMMY_NONCE), true);

        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(5, 0b10011, DUMMY_NONCE), false);

        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(5, 0b10111, DUMMY_NONCE), true);

        // History can be updated without causing a drop
        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(5, 0b10011, DUMMY_NONCE), false);
        assert_eq!(tx.acknowledge(5, 0b11011, DUMMY_NONCE), false);
        assert_eq!(tx.acknowledge(7, 0b11101, DUMMY_NONCE), false);
        assert_eq!(tx.acknowledge(7, 0b11111, DUMMY_NONCE), false);

        // History can be updated and cause a drop
        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(5, 0b10011, DUMMY_NONCE), false);
        assert_eq!(tx.acknowledge(7, 0b11111, DUMMY_NONCE), true);

        // Maximum gap between acknowledged segments
        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(32, (0b1 << 31) | 0b001, DUMMY_NONCE), false);

        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(32, (0b1 << 31) | 0b011, DUMMY_NONCE), false);

        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(32, (0b1 << 31) | 0b111, DUMMY_NONCE), true);

        // Maximum possible acknowledgement, 32 segments total
        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(32, u32::MAX, DUMMY_NONCE), false);

        // Past-maximum acknowledgement always fails
        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(33, u32::MAX, DUMMY_NONCE), true);

        // Any unacknowledged segment older than 32 is a drop, even if < 3 acked thereafter
        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(31, 0b0, DUMMY_NONCE), false);
        assert_eq!(tx.acknowledge(32, 0b0, DUMMY_NONCE), true);

        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(32, 0b1, DUMMY_NONCE), true);

        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(16, 0b1, DUMMY_NONCE), false);
        assert_eq!(tx.acknowledge(32, 0b1, DUMMY_NONCE), true);

        let mut tx = new_filled_window(WINDOW_SIZE);
        assert_eq!(tx.acknowledge(16, 0b1, DUMMY_NONCE), false);
        assert_eq!(tx.acknowledge(31, 0b1, DUMMY_NONCE), false);
        assert_eq!(tx.acknowledge(32, 0b0, DUMMY_NONCE), true);
    }
}
