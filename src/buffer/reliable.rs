use super::FragmentRc;
use super::FragmentRef;
use super::Window;

use std::collections::VecDeque;
use std::rc::Rc;

#[derive(Debug)]
pub struct TxBuffer {
    fragments: VecDeque<FragmentRc>,
    next_send_id: u32,
    min_ack_id: u32,
    fragment_size: usize,
    send_window: Window,
    duplicate_ack_count: u32,
}

impl TxBuffer {
    pub fn new(base_id: u32, window_size: u32, fragment_size: usize) -> Self {
        assert!(window_size > 0);
        assert!(fragment_size > 0);

        Self {
            fragments: VecDeque::new(),
            next_send_id: base_id,
            min_ack_id: base_id,
            fragment_size,
            send_window: Window::new(base_id, window_size),
            duplicate_ack_count: 0,
        }
    }

    pub fn push(&mut self, packet: Box<[u8]>) {
        let packet_len = packet.len();

        let mut bytes_remaining = packet.len();
        let mut index = 0;
        let mut first = true;

        let packet_rc = Rc::new(packet);

        while bytes_remaining > self.fragment_size {
            let data_range = index..index + self.fragment_size;

            self.fragments.push_back(FragmentRc {
                first,
                last: false,
                data: Rc::clone(&packet_rc),
                data_range,
            });

            first = false;
            index += self.fragment_size;

            bytes_remaining -= self.fragment_size;
        }

        let data_range = index..packet_len;

        self.fragments.push_back(FragmentRc {
            first,
            last: true,
            data: packet_rc,
            data_range,
        });
    }

    pub fn peek_sendable(&self) -> Option<(u32, &FragmentRc)> {
        if self.send_window.contains(self.next_send_id) {
            let fragment_id = self.next_send_id;

            let idx = fragment_id.wrapping_sub(self.send_window.base_id) as usize;

            if let Some(fragment) = self.fragments.get(idx) {
                return Some((self.next_send_id, fragment));
            }
        }

        return None;
    }

    pub fn pop_sendable(&mut self) -> Option<(u32, &FragmentRc)> {
        if self.send_window.contains(self.next_send_id) {
            let idx = self.next_send_id.wrapping_sub(self.send_window.base_id) as usize;

            if let Some(fragment) = self.fragments.get(idx) {
                // Return the fragment after advancing the next send ID, also advancing the next
                // ack ID if we are caught up

                let fragment_id = self.next_send_id;

                let equal = self.next_send_id == self.min_ack_id;

                self.next_send_id = self.next_send_id.wrapping_add(1);

                if equal {
                    self.min_ack_id = self.next_send_id;
                }

                return Some((fragment_id, fragment));
            }
        }

        return None;
    }

    pub fn acknowledge(&mut self, new_base_id: u32) -> bool {
        if new_base_id == self.send_window.base_id {
            // Nothing new was acknowledged, this is a duplicate

            self.duplicate_ack_count += 1;

            if self.duplicate_ack_count == 2 {
                self.duplicate_ack_count = 0;

                self.next_send_id = self.send_window.base_id;
            }

            return true;
        } else {
            let new_base_delta = new_base_id.wrapping_sub(self.send_window.base_id);
            let max_ack_delta = self.min_ack_id.wrapping_sub(self.send_window.base_id);

            if new_base_delta <= max_ack_delta {
                // New fragments have been acknowledged, remove those fragments from the queue and
                // advance the window
                self.fragments.drain(0..new_base_delta as usize);
                self.send_window.base_id = new_base_id;

                self.duplicate_ack_count = 0;

                return true;
            } else {
                // The ack would acknowledge fragments which do not exist or are beyond the send
                // window
                return false;
            }
        }
    }
}

pub struct PacketBuild {
    buffer: Vec<u8>,
}

pub struct RxBuffer {
    fragment_size: usize,
    next_fragment_id: u32,
    current_build: Option<PacketBuild>,
}

impl RxBuffer {
    pub fn new(base_id: u32, fragment_size: usize) -> Self {
        assert!(fragment_size > 0);

        Self {
            fragment_size,
            next_fragment_id: base_id,
            current_build: None,
        }
    }

    fn fragment_is_valid(fragment: &FragmentRef, fragment_size: usize) -> bool {
        if fragment.last && fragment.data.len() > fragment_size {
            // This fragment does not agree with our fragment size
            return false;
        }

        if !fragment.last && fragment.data.len() != fragment_size {
            // This fragment does not agree with our fragment size
            return false;
        }

        return true;
    }

    pub fn receive(&mut self, fragment_id: u32, fragment: &FragmentRef) -> Option<Box<[u8]>> {
        if fragment_id == self.next_fragment_id
            && Self::fragment_is_valid(fragment, self.fragment_size)
        {
            // TODO: If there is no packet currently being built, a fragment should have its first
            // flag set in order to be accepted

            let ref mut current_build = self
                .current_build
                .get_or_insert_with(|| PacketBuild { buffer: Vec::new() });

            current_build.buffer.extend_from_slice(fragment.data);

            self.next_fragment_id = self.next_fragment_id.wrapping_add(1);

            if fragment.last {
                // Complete, return current buffer
                let current_build = self.current_build.take().unwrap();

                return Some(current_build.buffer.into());
            }
        }

        return None;
    }

    pub fn next_expected_id(&self) -> u32 {
        self.next_fragment_id
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;
    use std::rc::Rc;

    use super::*;

    // Tests peek in all cases that we test pop
    fn peek_and_pop_sendable(send_buf: &mut TxBuffer) -> Option<(u32, FragmentRc)> {
        let peek_result = match send_buf.peek_sendable() {
            Some(result) => {
                Some((result.0, result.1.clone()))
            },
            None => None,
        };

        let pop_result = match send_buf.pop_sendable() {
            Some(result) => {
                Some((result.0, result.1.clone()))
            },
            None => None,
        };

        assert_eq!(peek_result, pop_result);

        return pop_result;
    }

    fn push_basic(send_buf: &mut TxBuffer, id: u32) {
        let packet_data: Box<[u8]> = vec![
            (id >> 24) as u8,
            (id >> 16) as u8,
            (id >> 8) as u8,
            id as u8,
        ]
        .into();

        send_buf.push(packet_data);
    }

    fn expect_pop_basic(send_buf: &mut TxBuffer, id: u32) {
        let packet_data = Rc::new(
            vec![
                (id >> 24) as u8,
                (id >> 16) as u8,
                (id >> 8) as u8,
                id as u8,
            ]
            .into_boxed_slice(),
        );

        let (fragment_id, fragment) = peek_and_pop_sendable(send_buf).unwrap();

        assert_eq!(fragment_id, id);

        assert_eq!(
            fragment,
            FragmentRc {
                first: true,
                last: true,
                data: packet_data,
                data_range: 0..4,
            }
        );
    }

    fn expect_pop_fail(send_buf: &mut TxBuffer) {
        assert_eq!(peek_and_pop_sendable(send_buf), None);
    }

    fn expect_ack(send_buf: &mut TxBuffer, new_base_id: u32) {
        assert_eq!(send_buf.acknowledge(new_base_id), true);
    }

    fn expect_ack_fail(send_buf: &mut TxBuffer, new_base_id: u32) {
        assert_eq!(send_buf.acknowledge(new_base_id), false);
    }

    fn fragmentation_trial(
        initial_base_id: u32,
        window_size: u32,
        fragment_size: usize,
        packet_data: &Box<[u8]>,
        ranges: &[Range<usize>],
    ) {
        assert!(ranges.len() <= window_size as usize);

        let mut send_buf = TxBuffer::new(initial_base_id, window_size, fragment_size);

        // Enqueue a single packet
        send_buf.push(packet_data.clone());

        let packet_data_rc = Rc::new(packet_data.clone());

        // Test the ranges and IDs of resulting fragments
        for (idx, range) in ranges.iter().enumerate() {
            let (fragment_id, fragment) = peek_and_pop_sendable(&mut send_buf).unwrap();

            assert_eq!(fragment_id, initial_base_id.wrapping_add(idx as u32));

            assert_eq!(
                fragment,
                FragmentRc {
                    first: idx == 0,
                    last: idx == ranges.len() - 1,
                    data: Rc::clone(&packet_data_rc),
                    data_range: range.clone(),
                }
            );
        }
        expect_pop_fail(&mut send_buf);
    }

    #[test]
    fn send_fragmentation() {
        struct Trial {
            packet_data: Box<[u8]>,
            ranges: Vec<Range<usize>>,
        }

        let trials = vec![
            Trial {
                packet_data: vec![].into(),
                ranges: vec![0..0],
            },
            Trial {
                packet_data: vec![0].into(),
                ranges: vec![0..1],
            },
            Trial {
                packet_data: vec![0, 1].into(),
                ranges: vec![0..2],
            },
            Trial {
                packet_data: vec![0, 1, 2].into(),
                ranges: vec![0..3],
            },
            Trial {
                packet_data: vec![0, 1, 2, 3].into(),
                ranges: vec![0..4],
            },
            Trial {
                packet_data: vec![0, 1, 2, 3, 4].into(),
                ranges: vec![0..4, 4..5],
            },
            Trial {
                packet_data: vec![0, 1, 2, 3, 4, 5].into(),
                ranges: vec![0..4, 4..6],
            },
            Trial {
                packet_data: vec![0, 1, 2, 3, 4, 5, 6].into(),
                ranges: vec![0..4, 4..7],
            },
            Trial {
                packet_data: vec![0, 1, 2, 3, 4, 5, 6, 7].into(),
                ranges: vec![0..4, 4..8],
            },
            Trial {
                packet_data: vec![0, 1, 2, 3, 4, 5, 6, 7, 8].into(),
                ranges: vec![0..4, 4..8, 8..9],
            },
        ];

        const FRAGMENT_SIZE: usize = 4;
        const WINDOW_SIZE: u32 = 16;
        const ID_SWEEP_SIZE: u32 = 4;

        let mut initial_base_id = 0_u32.wrapping_sub(ID_SWEEP_SIZE);

        for _ in 0..=ID_SWEEP_SIZE {
            for trial in trials.iter() {
                fragmentation_trial(
                    initial_base_id,
                    WINDOW_SIZE,
                    FRAGMENT_SIZE,
                    &trial.packet_data,
                    &trial.ranges,
                )
            }

            initial_base_id = initial_base_id.wrapping_add(1);
        }
    }

    fn ack_advancement_trial(initial_base_id: u32, block_size: u32, window_size: u32) {
        assert!(block_size > 0);
        assert!(block_size <= window_size);

        const FRAGMENT_SIZE: usize = 8;

        let mut send_buf = TxBuffer::new(initial_base_id, window_size, FRAGMENT_SIZE);

        // Fill send buffer with thrice as many fragments as the send window size
        for i in 0..3 * window_size {
            push_basic(&mut send_buf, initial_base_id.wrapping_add(i));
        }

        let mut next_pop_id = initial_base_id;
        let mut queue_size = 3 * window_size;

        // Pop all currently sendable fragments
        for _ in 0..window_size {
            expect_pop_basic(&mut send_buf, next_pop_id);
            next_pop_id = next_pop_id.wrapping_add(1);
            queue_size -= 1;
        }
        expect_pop_fail(&mut send_buf);

        // Ack & send remaining packets in blocks of block_size
        let mut next_ack_id = initial_base_id.wrapping_add(block_size);

        loop {
            // Permit sending block_size more packets
            expect_ack(&mut send_buf, next_ack_id);
            next_ack_id = next_ack_id.wrapping_add(block_size);

            // block_size fragments are no longer window-limited, assuming the queue is not empty
            for _ in 0..block_size.min(queue_size) {
                expect_pop_basic(&mut send_buf, next_pop_id);
                next_pop_id = next_pop_id.wrapping_add(1);
                queue_size -= 1;
            }
            expect_pop_fail(&mut send_buf);

            if queue_size == 0 {
                return;
            }
        }
    }

    #[test]
    fn ack_advancement() {
        const WINDOW_SIZE: u32 = 16;
        const ID_SWEEP_SIZE: u32 = 3 * WINDOW_SIZE;

        let mut initial_base_id = 0_u32.wrapping_sub(ID_SWEEP_SIZE);

        for _ in 0..=ID_SWEEP_SIZE {
            for block_size in 1..=WINDOW_SIZE {
                ack_advancement_trial(initial_base_id, block_size, WINDOW_SIZE);
            }
            initial_base_id = initial_base_id.wrapping_add(1);
        }
    }

    fn invalid_acks_trial(initial_base_id: u32, window_size: u32) {
        const FRAGMENT_SIZE: usize = 8;
        let test_margin_size: u32 = window_size;

        for send_count in 0..window_size {
            let mut send_buf = TxBuffer::new(initial_base_id, window_size, FRAGMENT_SIZE);

            // Start with a filled send window
            for i in 0..window_size {
                push_basic(&mut send_buf, initial_base_id.wrapping_add(i));
            }

            // Send some fragments
            for i in 0..send_count {
                expect_pop_basic(&mut send_buf, initial_base_id.wrapping_add(i));
            }

            // Acks beyond valid range fail
            for i in 1..test_margin_size {
                expect_ack_fail(&mut send_buf, initial_base_id.wrapping_sub(i));
                expect_ack_fail(
                    &mut send_buf,
                    initial_base_id.wrapping_add(send_count).wrapping_add(i),
                );
            }

            // Sequential intermittent IDs each succeed
            for i in 0..=send_count {
                expect_ack(&mut send_buf, initial_base_id.wrapping_add(i));
            }

            // A second ack succeeds
            expect_ack(&mut send_buf, initial_base_id.wrapping_add(send_count));

            // But its neighbors do not
            expect_ack_fail(
                &mut send_buf,
                initial_base_id.wrapping_add(send_count).wrapping_sub(1),
            );
            expect_ack_fail(
                &mut send_buf,
                initial_base_id.wrapping_add(send_count).wrapping_add(1),
            );
        }
    }

    #[test]
    fn invalid_acks() {
        const WINDOW_SIZE: u32 = 16;
        const ID_SWEEP_SIZE: u32 = 2 * WINDOW_SIZE;

        let mut initial_base_id = 0_u32.wrapping_sub(ID_SWEEP_SIZE);

        for _ in 0..=ID_SWEEP_SIZE {
            invalid_acks_trial(initial_base_id, WINDOW_SIZE);
            initial_base_id = initial_base_id.wrapping_add(1);
        }
    }

    fn resend_trial(initial_base_id: u32, window_size: u32) {
        const FRAGMENT_SIZE: usize = 8;

        for send_count in 0..=window_size {
            let mut send_buf = TxBuffer::new(initial_base_id, window_size, FRAGMENT_SIZE);

            // Start with a filled send window
            for i in 0..window_size {
                push_basic(&mut send_buf, initial_base_id.wrapping_add(i));
            }

            // Tx some fragments
            for i in 0..send_count {
                expect_pop_basic(&mut send_buf, initial_base_id.wrapping_add(i));
            }

            // Triple-acknowledge first sent packet (implicitly acked to begin with)
            send_buf.acknowledge(initial_base_id);
            send_buf.acknowledge(initial_base_id);

            // A resend should have been triggered, test that each sent packet is resent
            for i in 0..send_count {
                expect_pop_basic(&mut send_buf, initial_base_id.wrapping_add(i));
            }
        }
    }

    #[test]
    fn resend() {
        const WINDOW_SIZE: u32 = 16;
        const ID_SWEEP_SIZE: u32 = WINDOW_SIZE;

        let mut initial_base_id = 0_u32.wrapping_sub(ID_SWEEP_SIZE);

        for _ in 0..=ID_SWEEP_SIZE {
            resend_trial(initial_base_id, WINDOW_SIZE);
            initial_base_id = initial_base_id.wrapping_add(1);
        }
    }

    /*
    fn reassembly_trial(initial_base_id: u32, fragment_size: usize) {
        use rand::Rng;
        use rand::SeedableRng;

        let mut rng = rand::rngs::StdRng::from_seed([0; 32]);

        const MAX_PACKET_SIZE: usize = 20;

        for packet_size in 0..=MAX_PACKET_SIZE {
            let mut recv_buf = RxBuffer::new(initial_base_id, fragment_size);

            let mut packet_data: Box<[u8]> = vec![0; packet_size].into();
            rng.fill(&mut packet_data[..]);

            todo!()

            let generator = FragmentGen::new(packet_data.clone(), fragment_size, initial_base_id);

            let fragment_count = if packet_data.len() == 0 {
                1
            } else {
                (packet_data.len() + fragment_size - 1) / fragment_size
            };

            for (idx, ref fragment) in generator.enumerate() {
                let result = recv_buf.receive(&fragment.into());

                if idx == fragment_count - 1 {
                    assert_eq!(result, Some(packet_data));
                    break;
                } else {
                    assert_eq!(result, None);
                }
            }
        }
    }

    #[test]
    fn reassembly() {
        const ID_SWEEP_SIZE: u32 = 16;

        let mut initial_base_id = 0_u32.wrapping_sub(ID_SWEEP_SIZE);

        for fragment_size in [4, 7, 13] {
            for _ in 0..=ID_SWEEP_SIZE {
                reassembly_trial(initial_base_id, fragment_size);
                initial_base_id = initial_base_id.wrapping_add(1);
            }
        }
    }
    */
}
