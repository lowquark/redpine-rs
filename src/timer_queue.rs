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

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    struct MockTimer {
        should_trigger: bool,
        trigger_count: Arc<Mutex<usize>>,
    }

    impl Timer for MockTimer {
        fn test(&self, _now_ms: u64) -> bool {
            if self.should_trigger {
                let mut count = self.trigger_count.lock().unwrap();
                *count += 1;
                true
            } else {
                false
            }
        }
    }

    #[test]
    fn test_trigger_callback_called() {
        let mut queue = TimerQueue::new();
        let count = Arc::new(Mutex::new(0));

        let timer = Arc::new(MockTimer {
            should_trigger: true,
            trigger_count: count.clone(),
        });

        queue.add_timer(Arc::downgrade(&timer));

        let mut callback_count = 0;
        queue.test(1, 12345, |_| {
            callback_count += 1;
        });

        assert_eq!(callback_count, 1);
        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[test]
    fn test_no_trigger_if_test_returns_false() {
        let mut queue = TimerQueue::new();
        let count = Arc::new(Mutex::new(0));

        let timer = Arc::new(MockTimer {
            should_trigger: false,
            trigger_count: count.clone(),
        });

        queue.add_timer(Arc::downgrade(&timer));

        let mut callback_count = 0;
        queue.test(1, 12345, |_| {
            callback_count += 1;
        });

        assert_eq!(callback_count, 0);
        assert_eq!(*count.lock().unwrap(), 0);
    }

    #[test]
    fn test_expired_timer_removed() {
        let mut queue = TimerQueue::new();

        let timer = Arc::new(MockTimer {
            should_trigger: true,
            trigger_count: Arc::new(Mutex::new(0)),
        });

        queue.add_timer(Arc::downgrade(&timer));

        drop(timer);

        queue.test(1, 12345, |_| panic!("callback should not be called"));

        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn test_limit_respected() {
        let mut queue = TimerQueue::new();
        let count = Arc::new(Mutex::new(0));

        let mut timers = Vec::new();

        for _ in 0..5 {
            let timer = Arc::new(MockTimer {
                should_trigger: true,
                trigger_count: count.clone(),
            });

            queue.add_timer(Arc::downgrade(&timer));

            timers.push(timer);
        }

        let mut callback_count = 0;

        queue.test(3, 12345, |_| {
            callback_count += 1;
        });

        assert_eq!(callback_count, 3);
        assert_eq!(*count.lock().unwrap(), 3);
        assert_eq!(queue.len(), 5);
    }
}
