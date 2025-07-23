use std::collections::VecDeque;
use std::sync::{Arc, Weak};

pub trait Timer {
    fn test(&self, now_ms: u64) -> bool;
}

#[derive(Default)]
pub struct TimerQueue<T> {
    queue: VecDeque<Weak<T>>,
}

impl<T> TimerQueue<T>
where
    T: Timer,
{
    pub fn new() -> Self {
        Self {
            queue: Default::default(),
        }
    }

    pub fn add_timer(&mut self, timer: Weak<T>) {
        self.queue.push_back(timer);
    }

    pub fn test<F>(&mut self, limit: usize, now_ms: u64, mut cb: F)
    where
        F: FnMut(&Arc<T>),
    {
        let max_iters = limit.min(self.queue.len());

        for _ in 0..max_iters {
            let timer_weak = self
                .queue
                .pop_front()
                .expect("impossible iteration condition");

            if let Some(timer) = Weak::upgrade(&timer_weak) {
                if timer.test(now_ms) {
                    // This timer has expired, signal
                    cb(&timer);
                }

                self.queue.push_back(timer_weak);
            } else {
                // This timer went out of scope, let it fall off the queue
            }
        }

        // Limit break
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }
}
