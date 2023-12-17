use super::FragmentRc;
use super::FragmentRef;

use std::collections::VecDeque;
use std::rc::Rc;

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

struct TxEntry {
    fragment: FragmentRc,
    expire_time_ms: u64,
}

pub struct TxBuffer {
    fragments: VecDeque<TxEntry>,
    next_fragment_id: u32,
    fragment_size: usize,
}

impl TxBuffer {
    pub fn new(base_id: u32, fragment_size: usize) -> Self {
        assert!(fragment_size > 0);

        Self {
            fragments: VecDeque::new(),
            next_fragment_id: base_id,
            fragment_size,
        }
    }

    pub fn push(&mut self, packet: Box<[u8]>, expire_time_ms: u64) {
        let packet_len = packet.len();

        let mut bytes_remaining = packet.len();
        let mut index = 0;
        let mut first = true;

        let packet_rc = Rc::new(packet);

        while bytes_remaining > self.fragment_size {
            let data_range = index..index + self.fragment_size;

            self.fragments.push_back(TxEntry {
                fragment: FragmentRc {
                    first,
                    last: false,
                    data: Rc::clone(&packet_rc),
                    data_range,
                },
                expire_time_ms,
            });

            first = false;
            index += self.fragment_size;

            bytes_remaining -= self.fragment_size;
        }

        let data_range = index..packet_len;

        self.fragments.push_back(TxEntry {
            fragment: FragmentRc {
                first,
                last: true,
                data: packet_rc,
                data_range,
            },
            expire_time_ms,
        });
    }

    /// Pops all fragments from the front of the queue which have expired
    pub fn pop_expired(&mut self, now_ms: u64) {
        while let Some(entry) = self.fragments.front() {
            if entry.expire_time_ms <= now_ms {
                self.fragments.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn peek_sendable(&self) -> Option<(u32, &FragmentRc)> {
        if let Some(entry) = self.fragments.front() {
            let fragment_id = self.next_fragment_id;

            return Some((fragment_id, &entry.fragment));
        }

        return None;
    }

    pub fn pop_sendable(&mut self) -> Option<(u32, FragmentRc)> {
        if let Some(entry) = self.fragments.pop_front() {
            let fragment_id = self.next_fragment_id;
            self.next_fragment_id = self.next_fragment_id.wrapping_add(1);

            return Some((fragment_id, entry.fragment));
        }

        return None;
    }
}

pub struct PacketBuild {
    buffer: Vec<u8>,
}

pub struct RxBuffer {
    fragment_size: usize,
    base_id: u32,
    current_build: Option<PacketBuild>,
}

impl RxBuffer {
    pub fn new(base_id: u32, fragment_size: usize) -> Self {
        assert!(fragment_size > 0);

        Self {
            fragment_size,
            base_id,
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
        if Self::fragment_is_valid(fragment, self.fragment_size) {
            if fragment.first {
                // Start a new packet

                if fragment.last {
                    // This is a one-fragment packet, invalidate previous assembly
                    self.current_build = None;

                    // Expect subsequent fragment
                    self.base_id = fragment_id.wrapping_add(1);

                    // Return clone of input slice (fragment data is referenced directly from its
                    // containing frame)
                    return Some(fragment.data.into());
                } else {
                    // This is a multi-fragment packet, start new assembly
                    let mut buffer = Vec::new();
                    buffer.extend_from_slice(fragment.data);

                    self.current_build = Some(PacketBuild { buffer });

                    // Expect subsequent fragment
                    self.base_id = fragment_id.wrapping_add(1);

                    return None;
                }
            } else if fragment_id == self.base_id {
                // Extend existing packet
                if let Some(ref mut current_build) = self.current_build {
                    current_build.buffer.extend_from_slice(fragment.data);

                    // Expect subsequent fragment
                    self.base_id = fragment_id.wrapping_add(1);

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

    pub fn reset(&mut self) {
        self.current_build = None;
    }

    pub fn next_expected_id(&self) -> u32 {
        self.base_id
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
            Some(result) => Some((result.0, result.1.clone())),
            None => None,
        };

        let pop_result = send_buf.pop_sendable();

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

        send_buf.push(packet_data, u64::max_value());
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

    fn fragmentation_trial(
        initial_base_id: u32,
        fragment_size: usize,
        packet_data: &Box<[u8]>,
        ranges: &[Range<usize>],
    ) {
        let mut send_buf = TxBuffer::new(initial_base_id, fragment_size);

        // Enqueue a single packet
        send_buf.push(packet_data.clone(), u64::max_value());

        let packet_data_rc = Rc::new(packet_data.clone());

        // Test the ranges and IDs of resulting fragments
        for (idx, range) in ranges.iter().enumerate() {
            let (fragment_id, fragment) = peek_and_pop_sendable(&mut send_buf).unwrap();

            assert_eq!(fragment_id, initial_base_id.wrapping_add(idx as u32));

            assert_eq!(
                fragment,
                FragmentRc {
                    first: idx == 0,
                    last: idx == (ranges.len() - 1),
                    data: packet_data_rc.clone(),
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
        const ID_SWEEP_SIZE: u32 = 4;

        let mut initial_base_id = 0_u32.wrapping_sub(ID_SWEEP_SIZE);

        for _ in 0..=ID_SWEEP_SIZE {
            for trial in trials.iter() {
                fragmentation_trial(
                    initial_base_id,
                    FRAGMENT_SIZE,
                    &trial.packet_data,
                    &trial.ranges,
                )
            }

            initial_base_id = initial_base_id.wrapping_add(1);
        }
    }
}
