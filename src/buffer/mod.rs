use std::ops::Range;
use std::rc::Rc;

#[derive(Debug)]
pub struct Window {
    pub base_id: u32,
    pub size: u32,
}

impl Window {
    pub fn new(base_id: u32, size: u32) -> Self {
        Self { base_id, size }
    }

    pub fn contains(&self, id: u32) -> bool {
        let delta = id.wrapping_sub(self.base_id);
        return delta < self.size;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FragmentRc {
    pub id: u32,
    pub first: bool,
    pub last: bool,
    pub data: Rc<Box<[u8]>>,
    pub data_range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FragmentRef<'a> {
    pub id: u32,
    pub first: bool,
    pub last: bool,
    pub data: &'a [u8],
}

impl<'a> From<&'a FragmentRc> for FragmentRef<'a> {
    fn from(rc: &'a FragmentRc) -> Self {
        Self {
            id: rc.id,
            first: rc.first,
            last: rc.last,
            data: &rc.data[rc.data_range.clone()],
        }
    }
}

mod reliable;
mod segment;
mod unreliable;

pub use segment::RxBuffer as SegmentRxBuffer;

pub use unreliable::RxBuffer as UnreliableRxBuffer;
pub use unreliable::TxBuffer as UnreliableTxBuffer;

pub use reliable::RxBuffer as ReliableRxBuffer;
pub use reliable::TxBuffer as ReliableTxBuffer;

struct FragmentGen {
    fragment_size: usize,

    fragment_index: usize,

    source: Option<Rc<Box<[u8]>>>,
    source_index_bytes: usize,
    next_fragment_id: u32,
}

impl FragmentGen {
    pub fn new(packet: Box<[u8]>, fragment_size: usize, first_fragment_id: u32) -> Self {
        assert!(fragment_size > 0);

        Self {
            fragment_size,
            fragment_index: 0,
            source: Some(Rc::new(packet)),
            source_index_bytes: 0,
            next_fragment_id: first_fragment_id,
        }
    }

    pub fn next_fragment_id(&self) -> u32 {
        self.next_fragment_id
    }
}

impl Iterator for FragmentGen {
    type Item = FragmentRc;

    fn next(&mut self) -> Option<FragmentRc> {
        if let Some(source) = &self.source {
            let source_len_bytes = source.len();
            let bytes_remaining = source_len_bytes - self.source_index_bytes;

            let next_fragment = if bytes_remaining > self.fragment_size {
                let data_range =
                    self.source_index_bytes..self.source_index_bytes + self.fragment_size;

                FragmentRc {
                    id: self.next_fragment_id,
                    first: self.fragment_index == 0,
                    last: false,
                    data: Rc::clone(&source),
                    data_range,
                }
            } else {
                let data_range = self.source_index_bytes..source_len_bytes;

                FragmentRc {
                    id: self.next_fragment_id,
                    first: self.fragment_index == 0,
                    last: true,
                    data: self.source.take().unwrap(),
                    data_range,
                }
            };

            self.next_fragment_id = self.next_fragment_id.wrapping_add(1);
            self.fragment_index += 1;

            self.source_index_bytes += next_fragment.data_range.len();

            return Some(next_fragment);
        }

        return None;
    }
}
