use super::Window;

struct Slot {
    id: u32,
    data: Box<[u8]>,
    len: usize,
}

pub struct RxBuffer {
    window: Window,

    slot_0: Slot,
    slot_1: Slot,
    len: u8,
}

impl RxBuffer {
    pub fn new(window_base_id: u32, window_size: u32, segment_size_max: usize) -> Self {
        Self {
            window: Window::new(window_base_id, window_size),
            slot_0: Slot {
                id: 0,
                data: (0..segment_size_max).map(|_| 0).collect::<Vec<_>>().into(),
                len: 0,
            },
            slot_1: Slot {
                id: 0,
                data: (0..segment_size_max).map(|_| 0).collect::<Vec<_>>().into(),
                len: 0,
            },
            len: 0,
        }
    }

    fn swap_slots_if_backward(&mut self) {
        debug_assert_eq!(self.len, 2);

        if self.slot_0.id.wrapping_sub(self.window.base_id)
            > self.slot_1.id.wrapping_sub(self.window.base_id)
        {
            std::mem::swap(&mut self.slot_0, &mut self.slot_1);
        }
    }

    pub fn receive<F>(&mut self, new_id: u32, new_data: &[u8], mut cb: F) -> u8
    where
        F: FnMut(&[u8]),
    {
        // Only consider segments in the current window
        if self.window.contains(new_id) {
            if new_id == self.window.base_id {
                // This segment is expected next, deliver
                cb(new_data);
                self.window.base_id = self.window.base_id.wrapping_add(1);
            } else {
                if self.len == 0 {
                    // Add segment to empty buffer
                    self.slot_0.id = new_id;
                    self.slot_0.data[..new_data.len()].copy_from_slice(new_data);
                    self.slot_0.len = new_data.len();

                    self.len = 1;
                } else if self.len == 1 {
                    // Add segment to buffer if unique
                    if self.slot_0.id != new_id {
                        self.slot_1.id = new_id;
                        self.slot_1.data[..new_data.len()].copy_from_slice(new_data);
                        self.slot_1.len = new_data.len();

                        self.len = 2;

                        self.swap_slots_if_backward();
                    }
                } else if self.len == 2 {
                    // Push & pop segments if unique
                    if self.slot_0.id != new_id && self.slot_1.id != new_id {
                        if new_id.wrapping_sub(self.window.base_id)
                            < self.slot_0.id.wrapping_sub(self.window.base_id)
                        {
                            // The latest segment is the newest of the three, deliver
                            // (This skips unreceived segments)
                            cb(new_data);
                            self.window.base_id = new_id.wrapping_add(1);
                        } else {
                            // The first segment in the buffer is the newest of the three, deliver
                            // (This skips unreceived segments)
                            cb(&self.slot_0.data[..self.slot_0.len]);
                            self.window.base_id = self.slot_0.id.wrapping_add(1);

                            self.slot_0.id = new_id;
                            self.slot_0.data[..new_data.len()].copy_from_slice(new_data);
                            self.slot_0.len = new_data.len();

                            self.swap_slots_if_backward();
                        }
                    }
                }
            }

            // Deliver all segments in the buffer which match the next expected ID
            while self.len > 0 && self.slot_0.id == self.window.base_id {
                cb(&self.slot_0.data[..self.slot_0.len]);
                self.window.base_id = self.window.base_id.wrapping_add(1);

                if self.len == 2 {
                    std::mem::swap(&mut self.slot_0, &mut self.slot_1);
                }

                self.len -= 1;
            }
        }

        return self.len;
    }

    pub fn next_expected_id(&self) -> u32 {
        self.window.base_id
    }
}
