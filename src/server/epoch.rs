use std::time::Instant;

pub struct Epoch {
    time_base: Instant,
}

impl Epoch {
    pub fn new() -> Self {
        Self {
            time_base: Instant::now(),
        }
    }

    pub fn time_now_ms(&self) -> u64 {
        self.time_base.elapsed().as_millis() as u64
    }
}
