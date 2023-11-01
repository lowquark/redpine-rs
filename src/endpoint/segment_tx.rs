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

    // Ring buffer of nonce bits corresponding to [base, next)
    nonce_buf: Box<[u64]>,
    nonce_buf_mask: u32,

    // Sum of sizes in transit_buf
    transit_total: usize,

    // Bitfield representing segments acknowledged by receiver
    ack_history: u32,
    // Bitfield representing expected nonces corresponding to history
    nonce_history: u32,
}

impl SegmentTx {
    pub fn new(base_id: u32, size: u32) -> Self {
        assert!(
            size > 0 && (size - 1) & size == 0,
            "size must be a power of two"
        );

        assert!(size >= 64, "size must be 64 or greater");

        Self {
            base_id,
            next_id: base_id,
            size,

            transit_buf: vec![0; size as usize].into(),
            transit_buf_mask: size - 1,
            transit_total: 0,

            nonce_buf: vec![0; (size / 64) as usize].into(),
            nonce_buf_mask: (size / 64) - 1,

            ack_history: u32::MAX,
            nonce_history: 0,
        }
    }

    pub fn next_id(&self) -> u32 {
        self.next_id
    }

    pub fn compute_next_nonce(&mut self) -> bool {
        let nonce = rand::random::<bool>();

        //  id |  0  1  2 ... 61 62 63 | 64 65 66 ...
        // bit | 63 62 61 ...  2  1  0 | 63 62 61 ...

        let idx = self.next_id / 64;
        let bit_idx = self.next_id % 64;
        let bit_mask = 1u64 << (63 - bit_idx);

        let ref mut bitfield = self.nonce_buf[idx as usize];

        if nonce {
            *bitfield |= bit_mask;
        } else {
            *bitfield &= !bit_mask;
        }

        nonce
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

    fn update_nonce_history(&mut self, ack_delta: u32, base_id_new: u32) {
        // We will extract at most 32 bits [p0, p1) from nonce_buf
        let new_bits = ack_delta.min(32);

        let p1 = base_id_new;
        let p0 = base_id_new.wrapping_sub(new_bits);

        // Indexes into nonce_buf
        let p0_idx = p0 / 64;

        let p1_bits = p1 % 64;
        let p1_idx = p1 / 64;

        // Shift out room for bits from nonce_buf
        let mut x_new = if new_bits == 32 {
            0
        } else {
            self.nonce_history << new_bits
        };

        if p0_idx == p1_idx {
            // New bits come from bitfield B only

            // A               B
            // ################%%%%%%%%%%%%%%%%
            //            history xx------
            //                      ^     ^
            //                      p0    p1

            let bitfield = self.nonce_buf[(p1_idx & self.nonce_buf_mask) as usize];

            let shift = 64 - p1_bits;
            let mask = (1u64 << new_bits) - 1;

            x_new |= ((bitfield >> shift) & mask) as u32;
        } else {
            if p1_bits == 0 {
                // New bits come from bitfield A only

                // A               B
                // ################%%%%%%%%%%%%%%%%
                // history xx------
                //           ^     ^
                //           p0    p1

                let bitfield = self.nonce_buf[(p0_idx & self.nonce_buf_mask) as usize];

                let mask = (1u64 << new_bits) - 1;

                x_new |= (bitfield & mask) as u32;
            } else {
                // New bits come from bitfields A and B

                // A               B
                // ################%%%%%%%%%%%%%%%%
                //    history xx------
                //              ^     ^
                //              p0    p1

                let bitfield_a = self.nonce_buf[(p0_idx & self.nonce_buf_mask) as usize];
                let bitfield_b = self.nonce_buf[(p1_idx & self.nonce_buf_mask) as usize];

                let mask_a = (1u64 << (new_bits - p1_bits)) - 1;
                let mask_b = (1u64 << p1_bits) - 1;

                let shift_a = p1_bits;
                let shift_b = 64 - p1_bits;

                x_new |= ((bitfield_b >> shift_b) & mask_b) as u32;
                x_new |= ((bitfield_a & mask_a) << shift_a) as u32;
            }
        }

        self.nonce_history = x_new;
    }

    fn expected_nonce(nonce_history: u32, rx_history: u32) -> bool {
        let mut x = nonce_history & rx_history;
        x ^= x >> 16;
        x ^= x >> 8;
        x ^= x >> 4;
        x ^= x >> 2;
        x ^= x >> 1;
        x & 0b1 == 0b1
    }

    fn update_ack_history(
        &mut self,
        ack_delta: u32,
        rx_history: u32,
        rx_history_nonce: bool,
    ) -> bool {
        // This history queue incrementally tracks acknowledged segments from the receiver
        // until a drop is detected. A drop is detected when three segments have been received
        // after an unacknowledged segment, or any unacknowledged segment is behind the base ID
        // by ≥32 IDs. When a drop is detected, the queue resets to the default state where all
        // segments have been acknowledged in order to detect the next drop.

        //                     base (prev)    base' (latest ack)
        //                     v              v
        // xxxxxxxx xxxxxxxx xx000000 00000000   self.ack_history
        //                     000000 000yyyyy   rx_history

        if ack_delta > self.ack_history.leading_ones() {
            // Shifting would cause a zero to be >32 bits behind, that's a drop
            self.ack_history = u32::MAX;
            return true;
        }

        // Shift in zeros corresponding to this delta
        if ack_delta == 32 {
            self.ack_history = 0;
        } else {
            self.ack_history = self.ack_history.wrapping_shl(ack_delta);
        }

        if Self::expected_nonce(self.nonce_history, rx_history) == rx_history_nonce {
            // Nonce checks out, add received history bits to history buffer
            self.ack_history |= rx_history;
        } else {
            println!("bad nonce");
        }

        if self.ack_history == u32::MAX {
            // All segments accounted for, not a drop
            return false;
        }

        if self.ack_history.wrapping_shr(31) == 0 {
            // Oldest unacknowledged segment is 32 IDs old, that's a drop
            self.ack_history = u32::MAX;
            return true;
        }

        let leading_ones = self.ack_history.leading_ones();
        debug_assert!(leading_ones < 32);

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

    /// Acknowledges previously sent frames according to acknowledgement data. Returns true if a
    /// drop has been detected, false otherwise.
    pub fn acknowledge(
        &mut self,
        rx_base_id: u32,
        rx_history: u32,
        rx_history_nonce: bool,
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

                self.update_nonce_history(ack_delta, rx_base_id);

                // Advance the segment tx window
                self.base_id = rx_base_id;
            }

            // An ack delta of zero can still update previously received packets, so update
            // ack history regardless
            return self.update_ack_history(ack_delta, rx_history, rx_history_nonce);
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
