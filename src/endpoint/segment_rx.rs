use crate::buffer::Window;

struct Slot {
    id: u32,
    nonce: bool,
    data: Box<[u8]>,
    len: usize,
}

pub struct SegmentRx {
    window: Window,

    slot_0: Slot,
    slot_1: Slot,
    len: u8,

    rx_history: u32,
    nonce_history: u32,
}

impl SegmentRx {
    pub fn new(window_base_id: u32, window_size: u32, segment_size_max: usize) -> Self {
        Self {
            window: Window::new(window_base_id, window_size),
            slot_0: Slot {
                id: 0,
                nonce: false,
                data: (0..segment_size_max).map(|_| 0).collect::<Vec<_>>().into(),
                len: 0,
            },
            slot_1: Slot {
                id: 0,
                nonce: false,
                data: (0..segment_size_max).map(|_| 0).collect::<Vec<_>>().into(),
                len: 0,
            },
            len: 0,

            rx_history: 0,
            nonce_history: 0,
        }
    }

    fn save_segment(slot: &mut Slot, id: u32, nonce: bool, data: &[u8]) {
        slot.id = id;
        slot.nonce = nonce;
        slot.data[..data.len()].copy_from_slice(data);
        slot.len = data.len();
    }

    fn swap_slots(&mut self) {
        debug_assert_eq!(self.len, 2);

        std::mem::swap(&mut self.slot_0, &mut self.slot_1);
    }

    fn swap_slots_if_backward(&mut self) {
        debug_assert_eq!(self.len, 2);

        let slot_0_delta = self.slot_0.id.wrapping_sub(self.window.base_id);
        let slot_1_delta = self.slot_1.id.wrapping_sub(self.window.base_id);

        if slot_0_delta > slot_1_delta {
            self.swap_slots();
        }
    }

    fn push_history_ack(&mut self, nonce: bool) {
        self.rx_history <<= 1;
        self.nonce_history <<= 1;

        self.rx_history |= 0b1;
        self.nonce_history |= nonce as u32;
    }

    fn push_history_skips(&mut self, count: u32) {
        if count < 32 {
            self.rx_history <<= count;
            self.nonce_history <<= count;
        } else {
            self.rx_history = 0;
            self.nonce_history = 0;
        }
    }

    fn advance_window_past(&mut self, new_base_id: u32) {
        self.window.base_id = new_base_id.wrapping_add(1);
    }

    pub fn receive<F>(&mut self, new_id: u32, new_nonce: bool, new_data: &[u8], mut cb: F) -> u8
    where
        F: FnMut(&[u8]),
    {
        // Only consider segments in the current window
        if self.window.contains(new_id) {
            if new_id == self.window.base_id {
                // This segment is expected next, deliver
                cb(new_data);
                self.push_history_ack(new_nonce);
                self.advance_window_past(new_id);
            } else if self.len == 0 {
                // Add segment to empty buffer
                Self::save_segment(&mut self.slot_0, new_id, new_nonce, new_data);

                self.len = 1;
                return self.len;
            } else if self.len == 1 {
                // Add segment to buffer if unique
                if self.slot_0.id != new_id {
                    Self::save_segment(&mut self.slot_1, new_id, new_nonce, new_data);

                    self.len = 2;

                    self.swap_slots_if_backward();
                }

                return self.len;
            } else if self.len == 2 {
                // Push & pop segments if unique
                if self.slot_0.id != new_id && self.slot_1.id != new_id {
                    let new_delta = new_id.wrapping_sub(self.window.base_id);
                    let slot_0_delta = self.slot_0.id.wrapping_sub(self.window.base_id);

                    if new_delta < slot_0_delta {
                        // The latest segment is the newest of the three
                        // (This skips unreceived segments)
                        cb(new_data);

                        self.push_history_skips(new_delta);
                        self.push_history_ack(new_nonce);
                        self.advance_window_past(new_id);
                    } else {
                        // The first segment in the buffer is the newest of the three
                        // (This skips unreceived segments)

                        let slot_0_data = &self.slot_0.data[..self.slot_0.len];
                        cb(slot_0_data);

                        self.push_history_skips(slot_0_delta);
                        self.push_history_ack(self.slot_0.nonce);
                        self.advance_window_past(self.slot_0.id);

                        Self::save_segment(&mut self.slot_0, new_id, new_nonce, new_data);

                        self.swap_slots_if_backward();
                    }
                }
            } else {
                panic!("invalid buffer length")
            }

            // Deliver all segments in the buffer which match the next expected ID
            while self.len > 0 && self.slot_0.id == self.window.base_id {
                let slot_0_data = &self.slot_0.data[..self.slot_0.len];
                cb(slot_0_data);

                self.push_history_ack(self.slot_0.nonce);
                self.advance_window_past(self.slot_0.id);

                if self.len == 2 {
                    self.swap_slots();
                }

                self.len -= 1;
            }
        }

        self.len
    }

    pub fn sync<F>(&mut self, next_segment_id: u32, mut cb: F)
    where
        F: FnMut(&[u8]),
    {
        // Only accept syncs which exceed the last outstanding segment (if any)
        let delta = next_segment_id.wrapping_sub(self.window.base_id);

        let min_delta = match self.len {
            1 => self.slot_0.id.wrapping_sub(self.window.base_id),
            2 => self.slot_1.id.wrapping_sub(self.window.base_id),
            _ => self.window.base_id,
        };

        if delta > min_delta && delta <= self.window.size {
            // Deliver outstanding segments
            if self.len > 0 {
                let slot_0_data = &self.slot_0.data[..self.slot_0.len];
                let slot_0_delta = self.slot_0.id.wrapping_sub(self.window.base_id);
                cb(slot_0_data);

                self.push_history_skips(slot_0_delta);
                self.push_history_ack(self.slot_0.nonce);
                self.advance_window_past(self.slot_0.id);

                if self.len == 2 {
                    let slot_1_data = &self.slot_1.data[..self.slot_1.len];
                    let slot_1_delta = self.slot_1.id.wrapping_sub(self.window.base_id);
                    cb(slot_1_data);

                    self.push_history_skips(slot_1_delta);
                    self.push_history_ack(self.slot_1.nonce);
                    self.advance_window_past(self.slot_1.id);
                }
            }

            self.len = 0;

            // Advance window to complete sync
            let remaining_delta = next_segment_id.wrapping_sub(self.window.base_id);
            self.push_history_skips(remaining_delta);
            self.window.base_id = next_segment_id;
        }
    }

    pub fn next_expected_id(&self) -> u32 {
        self.window.base_id
    }

    fn compute_checksum(rx_history: u8, nonce_history: u8) -> bool {
        let mut x = rx_history & nonce_history;
        x ^= x >> 4;
        x ^= x >> 2;
        x ^= x >> 1;
        x & 0b1 == 0b1
    }

    pub fn next_ack_info(&self) -> (u8, bool) {
        const ACK_MASK: u32 = 0x1F;

        let rx_history = (self.rx_history & ACK_MASK) as u8;
        let nonce_history = self.nonce_history as u8;

        (
            rx_history,
            Self::compute_checksum(rx_history, nonce_history),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    fn test(rounds: &[(u8, Box<[u8]>, u8, u8)]) {
        let mut nonces = HashMap::new();

        let mut rx = SegmentRx::new(0, 128, 1500);

        for round in rounds.into_iter() {
            let id = round.0 as u32;

            // Generate a random nonce for this segment, save for later
            let nonce = rand::random::<bool>();
            if !nonces.contains_key(&id) {
                nonces.insert(id, nonce);
            }

            // A u8 ID is used so that it fits into a single byte
            let ref data = [id as u8];

            // Track all received packets
            let mut recv_buffer = Vec::new();

            let mut handler = |data: &[u8]| {
                recv_buffer.push(data[0]);
            };

            // Actually call receive
            rx.receive(id, nonce, data, &mut handler);

            let next_id = rx.next_expected_id();
            let (rx_history, rx_checksum) = rx.next_ack_info();

            // Test handled segments
            assert_eq!(recv_buffer, round.1.clone().into_vec());
            recv_buffer.clear();

            // Reported receive window base ID should match
            assert_eq!(next_id, round.2 as u32);

            // Reported ack history should match
            assert_eq!(rx_history, round.3);

            // Reported nonce checksum should match manually computed value
            let mut checksum_ref = false;
            for i in 0..8 {
                if rx_history & (1 << i) != 0 {
                    let acked_id = next_id.wrapping_sub(1).wrapping_sub(i) as u32;
                    checksum_ref ^= nonces[&acked_id];
                }
            }
            assert_eq!(rx_checksum, checksum_ref);
        }
    }

    #[test]
    fn sequential_receive() {
        let ref rounds = [
            (0, vec![0].into(), 1, 0b00001),
            (1, vec![1].into(), 2, 0b00011),
            (2, vec![2].into(), 3, 0b00111),
            (3, vec![3].into(), 4, 0b01111),
            (4, vec![4].into(), 5, 0b11111),
        ];

        test(rounds);
    }

    #[test]
    fn nonsequential_receive() {
        let ref rounds = [
            (0, vec![0].into(), 1, 0b00001),
            (2, vec![].into(), 1, 0b00001),
            (1, vec![1, 2].into(), 3, 0b00111),
            (3, vec![3].into(), 4, 0b01111),
            (4, vec![4].into(), 5, 0b11111),
        ];

        test(rounds);

        let ref rounds = [
            (0, vec![0].into(), 1, 0b00001),
            (3, vec![].into(), 1, 0b00001),
            (2, vec![].into(), 1, 0b00001),
            (1, vec![1, 2, 3].into(), 4, 0b01111),
            (4, vec![4].into(), 5, 0b11111),
        ];

        test(rounds);

        let ref rounds = [
            (0, vec![0].into(), 1, 0b00001),
            (2, vec![].into(), 1, 0b00001),
            (3, vec![].into(), 1, 0b00001),
            (1, vec![1, 2, 3].into(), 4, 0b01111),
            (4, vec![4].into(), 5, 0b11111),
        ];

        test(rounds);

        let ref rounds = [
            (0, vec![0].into(), 1, 0b00001),
            (2, vec![].into(), 1, 0b00001),
            (4, vec![].into(), 1, 0b00001),
            (1, vec![1, 2].into(), 3, 0b00111),
            (3, vec![3, 4].into(), 5, 0b11111),
        ];

        test(rounds);
    }

    #[test]
    fn skips() {
        let ref rounds = [
            (0, vec![0].into(), 1, 0b00001),
            (2, vec![].into(), 1, 0b00001),
            (3, vec![].into(), 1, 0b00001),
            (4, vec![2, 3, 4].into(), 5, 0b10111),
        ];

        test(rounds);

        let ref rounds = [
            (0, vec![0].into(), 1, 0b00001),
            (2, vec![].into(), 1, 0b00001),
            (4, vec![].into(), 1, 0b00001),
            (6, vec![2].into(), 3, 0b00101),
            (5, vec![4, 5, 6].into(), 7, 0b10111),
        ];

        test(rounds);

        let ref rounds = [
            (20, vec![].into(), 0, 0b00000),
            (50, vec![].into(), 0, 0b00000),
            (60, vec![20].into(), 21, 0b00001),
            (70, vec![50].into(), 51, 0b00001),
        ];

        test(rounds);
    }

    #[test]
    fn beyond_window() {
        let ref rounds = [
            (127, vec![].into(), 0, 0b00000),
            (128, vec![].into(), 0, 0b00000),
            (129, vec![].into(), 0, 0b00000),
        ];

        test(rounds);
    }

    #[test]
    fn dup_rejection() {
        let ref rounds = [
            (1, vec![].into(), 0, 0b00000),
            (1, vec![].into(), 0, 0b00000),
            (1, vec![].into(), 0, 0b00000),
            (2, vec![].into(), 0, 0b00000),
            (1, vec![].into(), 0, 0b00000),
            (2, vec![].into(), 0, 0b00000),
            (0, vec![0, 1, 2].into(), 3, 0b00111),
        ];

        test(rounds);
    }
}
