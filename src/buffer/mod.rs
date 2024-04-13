use std::ops::Range;
use std::sync::Arc;

mod reliable;
mod unreliable;

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
    pub first: bool,
    pub last: bool,
    pub data: Arc<Box<[u8]>>,
    pub data_range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FragmentRef<'a> {
    pub first: bool,
    pub last: bool,
    pub data: &'a [u8],
}

impl<'a> From<&'a FragmentRc> for FragmentRef<'a> {
    fn from(rc: &'a FragmentRc) -> Self {
        Self {
            first: rc.first,
            last: rc.last,
            data: &rc.data[rc.data_range.clone()],
        }
    }
}

pub use unreliable::RxBuffer as UnreliableRxBuffer;
pub use unreliable::TxBuffer as UnreliableTxBuffer;

pub use reliable::RxBuffer as ReliableRxBuffer;
pub use reliable::TxBuffer as ReliableTxBuffer;
