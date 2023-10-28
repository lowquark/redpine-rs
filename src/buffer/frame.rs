use super::Window;

struct FrameBuffer {
    id: u32,
    data: Box<[u8]>,
    len: usize,
}

pub struct RxBuffer {
    window: Window,

    buffer_0: FrameBuffer,
    buffer_1: FrameBuffer,
    buffer_len: u8,
}

impl RxBuffer {
    pub fn new(window_base_id: u32, window_size: u32, frame_buffer_size: usize) -> Self {
        Self {
            window: Window::new(window_base_id, window_size),
            buffer_0: FrameBuffer {
                id: 0,
                data: (0..frame_buffer_size).map(|_| 0).collect::<Vec<_>>().into(),
                len: 0,
            },
            buffer_1: FrameBuffer {
                id: 0,
                data: (0..frame_buffer_size).map(|_| 0).collect::<Vec<_>>().into(),
                len: 0,
            },
            buffer_len: 0,
        }
    }

    fn swap_buffers_if_backward(&mut self) {
        debug_assert_eq!(self.buffer_len, 2);

        if self.buffer_0.id.wrapping_sub(self.window.base_id)
            > self.buffer_1.id.wrapping_sub(self.window.base_id)
        {
            std::mem::swap(&mut self.buffer_0, &mut self.buffer_1);
        }
    }

    pub fn receive<F>(&mut self, new_id: u32, new_data: &[u8], mut cb: F) -> u8
    where
        F: FnMut(&[u8]),
    {
        // Only consider frames in the current window
        if self.window.contains(new_id) {
            if new_id == self.window.base_id {
                // This frame is expected next, deliver
                cb(new_data);
                self.window.base_id = self.window.base_id.wrapping_add(1);
            } else {
                if self.buffer_len == 0 {
                    // Add frame to empty buffer
                    self.buffer_0.id = new_id;
                    self.buffer_0.data[..new_data.len()].copy_from_slice(new_data);
                    self.buffer_0.len = new_data.len();

                    self.buffer_len = 1;
                } else if self.buffer_len == 1 {
                    // Add frame to buffer if unique
                    if self.buffer_0.id != new_id {
                        self.buffer_1.id = new_id;
                        self.buffer_1.data[..new_data.len()].copy_from_slice(new_data);
                        self.buffer_1.len = new_data.len();

                        self.buffer_len = 2;

                        self.swap_buffers_if_backward();
                    }
                } else if self.buffer_len == 2 {
                    // Push & pop frames if unique
                    if self.buffer_0.id != new_id && self.buffer_1.id != new_id {
                        if new_id.wrapping_sub(self.window.base_id)
                            < self.buffer_0.id.wrapping_sub(self.window.base_id)
                        {
                            // The latest frame is the newest of the three, deliver
                            // (This skips unreceived frames)
                            cb(new_data);
                            self.window.base_id = new_id.wrapping_add(1);
                        } else {
                            // The first frame in the buffer is the newest of the three, deliver
                            // (This skips unreceived frames)
                            cb(&self.buffer_0.data[..self.buffer_0.len]);
                            self.window.base_id = self.buffer_0.id.wrapping_add(1);

                            self.buffer_0.id = new_id;
                            self.buffer_0.data[..new_data.len()].copy_from_slice(new_data);
                            self.buffer_0.len = new_data.len();

                            self.swap_buffers_if_backward();
                        }
                    }
                }
            }

            // Deliver all frames in the buffer which match the next expected ID
            while self.buffer_len > 0 && self.buffer_0.id == self.window.base_id {
                cb(&self.buffer_0.data[..self.buffer_0.len]);
                self.window.base_id = self.window.base_id.wrapping_add(1);

                if self.buffer_len == 2 {
                    std::mem::swap(&mut self.buffer_0, &mut self.buffer_1);
                }

                self.buffer_len -= 1;
            }
        }

        return self.buffer_len;
    }

    pub fn next_expected_id(&self) -> u32 {
        self.window.base_id
    }
}
