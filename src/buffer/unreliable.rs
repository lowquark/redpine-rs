use super::FragmentGen;
use super::FragmentRc;
use super::FragmentRef;
use super::Window;

use std::collections::VecDeque;

// No sendable fragments sent:
// v---------------
// ooooooooooooooooooooooo^
//
// Some sendable fragments sent:
// v---------------
//        oooooooooooooooo^
//
// All sendable fragments sent:
// v---------------
//                 ooooooo^
//
// All fragments sent I:
// v---------------
//        ^
//
// All fragments sent II:
// v---------------
//                 ^

pub struct TxBuffer {
    fragments: VecDeque<FragmentRc>,
    next_fragment_id: u32,
    fragment_size: usize,
    send_window: Window,
}

impl TxBuffer {
    /// Returns the ID of the next fragment to be sent, whether or not it exists yet
    fn next_send_id(&self) -> u32 {
        if let Some(fragment) = self.fragments.front() {
            fragment.id
        } else {
            self.next_fragment_id
        }
    }

    pub fn new(base_id: u32, window_size: u32, fragment_size: usize) -> Self {
        assert!(fragment_size > 0);

        Self {
            fragments: VecDeque::new(),
            next_fragment_id: base_id,
            fragment_size,
            send_window: Window::new(base_id, window_size),
        }
    }

    pub fn push(&mut self, packet: Box<[u8]>) {
        let mut fragment_iter = FragmentGen::new(packet, self.fragment_size, self.next_fragment_id);

        self.fragments.extend(&mut fragment_iter);

        self.next_fragment_id = fragment_iter.next_fragment_id();
    }

    pub fn peek_sendable(&self) -> Option<&FragmentRc> {
        // Ensure the send window is in sync with the fragment queue
        debug_assert!(
            self.next_send_id().wrapping_sub(self.send_window.base_id) <= self.send_window.size
        );

        if let Some(fragment) = self.fragments.front() {
            if self.send_window.contains(fragment.id) {
                return self.fragments.front();
            }
        }

        return None;
    }

    pub fn pop_sendable(&mut self) -> Option<FragmentRc> {
        // Ensure the send window is in sync with the fragment queue
        debug_assert!(
            self.next_send_id().wrapping_sub(self.send_window.base_id) <= self.send_window.size
        );

        if let Some(fragment) = self.fragments.front() {
            if self.send_window.contains(fragment.id) {
                return self.fragments.pop_front();
            }
        }

        return None;
    }

    pub fn acknowledge(&mut self, new_base_id: u32) -> bool {
        // Ensure the send window is in sync with the fragment queue
        debug_assert!(
            self.next_send_id().wrapping_sub(self.send_window.base_id) <= self.send_window.size
        );

        // For an ack to be valid, the new base ID may not exceed the next ID to be sent
        let ack_delta_max = self.next_send_id().wrapping_sub(self.send_window.base_id);
        let ack_delta = new_base_id.wrapping_sub(self.send_window.base_id);

        if ack_delta <= ack_delta_max {
            // Advance the send window accordingly
            self.send_window.base_id = new_base_id;
            return true;
        } else {
            return false;
        }
    }
}

pub struct PacketBuild {
    buffer: Vec<u8>,
}

pub struct RxBuffer {
    fragment_size: usize,
    receive_window: Window,
    current_build: Option<PacketBuild>,
}

impl RxBuffer {
    pub fn new(base_id: u32, window_size: u32, fragment_size: usize) -> Self {
        assert!(window_size > 0);
        assert!(fragment_size > 0);

        Self {
            fragment_size,
            receive_window: Window::new(base_id, window_size),
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

    pub fn receive(&mut self, fragment: &FragmentRef) -> Option<Box<[u8]>> {
        if self.receive_window.contains(fragment.id)
            && Self::fragment_is_valid(fragment, self.fragment_size)
        {
            if fragment.first {
                // Start a new packet

                if fragment.last {
                    // This is a one-fragment packet, invalidate previous assembly
                    self.current_build = None;

                    // Expect subsequent packet
                    self.receive_window.base_id = fragment.id.wrapping_add(1);

                    // Return clone of input slice (fragments are assumed to be referenced directly
                    // from their containing frame)
                    return Some(fragment.data.into());
                } else {
                    // This is a multi-fragment packet, start new assembly
                    let mut buffer = Vec::new();
                    buffer.extend_from_slice(fragment.data);

                    self.current_build = Some(PacketBuild { buffer });

                    // Expect subsequent packet
                    self.receive_window.base_id = fragment.id.wrapping_add(1);

                    return None;
                }
            } else if fragment.id == self.receive_window.base_id {
                // Extend existing packet
                if let Some(ref mut current_build) = self.current_build {
                    current_build.buffer.extend_from_slice(fragment.data);

                    // Expect subsequent packet
                    self.receive_window.base_id = fragment.id.wrapping_add(1);

                    // Return if finished
                    if fragment.last {
                        let current_build = self.current_build.take().unwrap();

                        return Some(current_build.buffer.into_boxed_slice());
                    }
                } else {
                    // No existing build!? Reject
                }
            }
        }

        return None;
    }

    pub fn next_expected_id(&self) -> u32 {
        self.receive_window.base_id
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;
    use std::rc::Rc;

    use super::*;

    // Tests peek in all cases that we test pop
    fn peek_and_pop_sendable(send_buf: &mut TxBuffer) -> Option<FragmentRc> {
        let fragment_peek = send_buf.peek_sendable().cloned();
        let fragment_pop = send_buf.pop_sendable();

        assert_eq!(fragment_peek, fragment_pop);

        return fragment_pop;
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

    fn expect_pop_basic(send_buf: &mut TxBuffer, id: u32) -> FragmentRc {
        let packet_data = Rc::new(
            vec![
                (id >> 24) as u8,
                (id >> 16) as u8,
                (id >> 8) as u8,
                id as u8,
            ]
            .into_boxed_slice(),
        );

        let fragment = peek_and_pop_sendable(send_buf);

        assert_eq!(
            fragment,
            Some(FragmentRc {
                id,
                first: true,
                last: true,
                data: packet_data,
                data_range: 0..4,
            })
        );

        return fragment.unwrap();
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
            let fragment = peek_and_pop_sendable(&mut send_buf);

            assert_eq!(
                fragment,
                Some(FragmentRc {
                    id: initial_base_id.wrapping_add(idx as u32),
                    first: idx == 0,
                    last: idx == (ranges.len() - 1),
                    data: packet_data_rc.clone(),
                    data_range: range.clone(),
                })
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
}
