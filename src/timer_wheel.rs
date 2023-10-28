use std::fmt;

// See https://lwn.net/Articles/646950/

// For array sizes of 8, bin sizes of 4, 16, and 64 ms:
//
//  t (4ms) ->
//
//  xxxxxxxx
//  x---x---x---x---x---x---x---x---
//  x---------------x---------------x---------------x--------------- ...

// mask0: 0b00000111 shift0: 2
// mask1: 0b00000111 shift1: 4
// mask2: 0b00000111 shift2: 6

type TimerIndex = u32;

#[derive(Clone, Debug)]
pub struct TimerId {
    idx: TimerIndex,
}

struct TimerEntry {
    next_idx: TimerIndex,
    prev_idx: TimerIndex,
    timeout_time_ms: u64,
}

struct Array {
    timer_indices: std::ops::Range<TimerIndex>,
    bin_time_mask: u64,
    bin_time_shift: u32,
    size: u64,
    next_abs_bin_idx: u64,
}

pub struct ArrayConfig {
    pub size: u32,
    pub ms_per_bin: u32,
}

pub struct TimerWheel<T> {
    arrays: Box<[Array]>,
    timer_data: Vec<Option<T>>,
    timer_entries: Vec<TimerEntry>,
    free_list: Vec<TimerIndex>,
    last_step_time_ms: u64,
}

impl TimerEntry {
    fn new(next_idx: TimerIndex, prev_idx: TimerIndex, timeout_time_ms: u64) -> Self {
        Self {
            next_idx,
            prev_idx,
            timeout_time_ms,
        }
    }
}

impl Array {
    fn new(
        timer_indices: std::ops::Range<TimerIndex>,
        bin_time_mask: u64,
        bin_time_shift: u32,
    ) -> Self {
        let size = timer_indices.len() as u64;

        Self {
            timer_indices,
            bin_time_mask,
            bin_time_shift,
            size,
            next_abs_bin_idx: 0,
        }
    }
}

fn is_power_of_two(x: u32) -> bool {
    x != 0 && (x - 1) & x == 0
}

fn compute_array_params(size: u32, ms_per_bin: u32) -> (u64, u32) {
    assert!(is_power_of_two(size), "array size must be a power of two");
    assert!(
        is_power_of_two(ms_per_bin),
        "bin interval must be a power of two"
    );

    let mask = (size - 1) as u64;
    let mut shift = 0;

    for i in 0..32 {
        if ms_per_bin & (1 << i) != 0 {
            shift = i;
            break;
        }
    }

    (mask, shift)
}

impl<T> TimerWheel<T> {
    pub fn new(array_config: &[ArrayConfig], now_ms: u64) -> Self {
        assert!(array_config.len() > 0);

        let mut total_bin_count = 0;
        let mut arrays = Vec::new();

        for config in array_config.iter() {
            let timer_indices = total_bin_count..total_bin_count + config.size;
            total_bin_count += config.size;

            let (mask, shift) = compute_array_params(config.size, config.ms_per_bin);

            arrays.push(Array::new(timer_indices, mask, shift));
        }

        let timer_data = (0..total_bin_count).map(|_| None).collect();

        let timer_entries = (0..total_bin_count)
            .map(|idx| TimerEntry::new(idx as TimerIndex, idx as TimerIndex, 0))
            .collect();

        let free_list = Vec::new();

        Self {
            arrays: arrays.into_boxed_slice(),
            timer_data,
            timer_entries,
            free_list,
            last_step_time_ms: now_ms,
        }
    }

    fn find_timer_bin(timeout_time_ms: u64, arrays: &[Array]) -> TimerIndex {
        debug_assert!(arrays.len() > 0);

        for array_idx in 0..arrays.len() - 1 {
            let ref array = arrays[array_idx];

            let abs_bin_idx = timeout_time_ms >> array.bin_time_shift;

            let abs_idx_diff = abs_bin_idx - array.next_abs_bin_idx;

            // Bins in the lower arrays must not alias, so ensure that the absolute bin index is
            // not further ahead than the array size
            if abs_idx_diff < array.size {
                return array.timer_indices.start + (abs_bin_idx & array.bin_time_mask) as u32;
            }
        }

        let ref array = arrays[arrays.len() - 1];

        let abs_bin_idx = timeout_time_ms >> array.bin_time_shift;

        return array.timer_indices.start + (abs_bin_idx & array.bin_time_mask) as u32;
    }

    pub fn set_timer(&mut self, timeout_time_ms: u64, handler_data: T) -> TimerId {
        assert!(
            timeout_time_ms >= self.last_step_time_ms,
            "attempt to set timer in the past"
        );

        let timer_idx = if let Some(timer_idx) = self.free_list.pop() {
            // Update existing entry in the timer table

            self.timer_data[timer_idx as usize] = Some(handler_data);

            self.timer_entries[timer_idx as usize].timeout_time_ms = timeout_time_ms;

            timer_idx
        } else {
            let timer_idx = self
                .timer_data
                .len()
                .try_into()
                .expect("timer limit reached");

            // Create new entries in the timer table
            self.timer_data.push(Some(handler_data));

            self.timer_entries
                .push(TimerEntry::new(timer_idx, timer_idx, timeout_time_ms));

            timer_idx
        };

        let sentinel_idx = Self::find_timer_bin(timeout_time_ms, &self.arrays);
        let sentinel_prev_idx = self.timer_entries[sentinel_idx as usize].prev_idx;

        // sentinel.prev.next := timer
        self.timer_entries[sentinel_prev_idx as usize].next_idx = timer_idx;
        // timer.prev := sentinel.prev
        self.timer_entries[timer_idx as usize].prev_idx = sentinel_prev_idx;
        // sentinel.prev := timer
        self.timer_entries[sentinel_idx as usize].prev_idx = timer_idx;
        // timer.next := sentinel
        self.timer_entries[timer_idx as usize].next_idx = sentinel_idx;

        TimerId { idx: timer_idx }
    }

    pub fn unset_timer(&mut self, timer_id: TimerId) {
        let timer_idx = timer_id.idx;

        assert!(
            timer_idx >= self.arrays[self.arrays.len() - 1].timer_indices.end,
            "invalid timer handle passed to unset"
        );

        assert!(
            (timer_idx as usize) < self.timer_entries.len(),
            "invalid timer handle passed to unset"
        );

        let timer_next_idx = self.timer_entries[timer_idx as usize].next_idx;
        let timer_prev_idx = self.timer_entries[timer_idx as usize].prev_idx;

        // timer.prev.next := timer.next
        self.timer_entries[timer_prev_idx as usize].next_idx = timer_next_idx;
        // timer.next.prev := timer.prev
        self.timer_entries[timer_next_idx as usize].prev_idx = timer_prev_idx;
        // timer.next := timer
        self.timer_entries[timer_idx as usize].next_idx = timer_idx;
        // timer.prev := timer
        self.timer_entries[timer_idx as usize].prev_idx = timer_idx;

        self.timer_data[timer_idx as usize] = None;
        self.free_list.push(timer_idx);
    }

    pub fn next_expiration_time_ms(&self) -> Option<u64> {
        let mut min_time_ms = u64::max_value();

        for array in self.arrays.iter() {
            for i in 0..array.size {
                let abs_bin_idx = array.next_abs_bin_idx + i;
                let true_bin_idx = abs_bin_idx & array.bin_time_mask;
                let sentinel_timer_idx = array.timer_indices.start + true_bin_idx as TimerIndex;

                if self.timer_entries[sentinel_timer_idx as usize].next_idx != sentinel_timer_idx {
                    // This bin is not empty and expires at the start of the following bin
                    let bin_expire_time =
                        ((self.last_step_time_ms >> array.bin_time_shift) + i + 1)
                            << array.bin_time_shift;
                    min_time_ms = min_time_ms.min(bin_expire_time);
                    break;
                }
            }
        }

        if min_time_ms != u64::max_value() {
            Some(min_time_ms)
        } else {
            None
        }
    }

    fn expire_all(
        timer_entries: &mut Vec<TimerEntry>,
        timer_data: &mut Vec<Option<T>>,
        free_list: &mut Vec<TimerIndex>,
        sentinel_timer_idx: u32,
        expired_data: &mut Vec<T>,
    ) {
        let mut timer_idx = timer_entries[sentinel_timer_idx as usize].next_idx;

        while timer_idx != sentinel_timer_idx {
            let handler = timer_data[timer_idx as usize].take().unwrap();
            expired_data.push(handler);

            let timer_next_idx = timer_entries[timer_idx as usize].next_idx;

            // timer.next := timer
            timer_entries[timer_idx as usize].next_idx = timer_idx;
            // timer.prev := timer
            timer_entries[timer_idx as usize].prev_idx = timer_idx;

            free_list.push(timer_idx);

            timer_idx = timer_next_idx;
        }

        timer_entries[sentinel_timer_idx as usize].next_idx = sentinel_timer_idx;
        timer_entries[sentinel_timer_idx as usize].prev_idx = sentinel_timer_idx;
    }

    fn expire_expired(
        timer_entries: &mut Vec<TimerEntry>,
        timer_data: &mut Vec<Option<T>>,
        free_list: &mut Vec<TimerIndex>,
        sentinel_timer_idx: u32,
        expired_data: &mut Vec<T>,
        time_now_ms: u64,
    ) {
        let mut timer_idx = timer_entries[sentinel_timer_idx as usize].next_idx;

        while timer_idx != sentinel_timer_idx {
            if timer_entries[timer_idx as usize].timeout_time_ms <= time_now_ms {
                let handler = timer_data[timer_idx as usize].take().unwrap();
                expired_data.push(handler);

                let timer_next_idx = timer_entries[timer_idx as usize].next_idx;
                let timer_prev_idx = timer_entries[timer_idx as usize].prev_idx;

                // timer.prev.next := timer.next
                timer_entries[timer_prev_idx as usize].next_idx = timer_next_idx;
                // timer.next.prev := timer.prev
                timer_entries[timer_next_idx as usize].prev_idx = timer_prev_idx;
                // timer.next := timer
                timer_entries[timer_idx as usize].next_idx = timer_idx;
                // timer.prev := timer
                timer_entries[timer_idx as usize].prev_idx = timer_idx;

                free_list.push(timer_idx);

                timer_idx = timer_next_idx;
            } else {
                let timer_next_idx = timer_entries[timer_idx as usize].next_idx;

                timer_idx = timer_next_idx;
            }
        }
    }

    pub fn step(&mut self, time_now_ms: u64, entries_out: &mut Vec<T>) {
        assert!(
            time_now_ms >= self.last_step_time_ms,
            "attempt to step in the past"
        );

        let array_count = self.arrays.len();

        for (array_idx, array) in self.arrays.iter_mut().enumerate() {
            let time_now_idx = time_now_ms >> array.bin_time_shift;

            let end_idx = u64::min(time_now_idx, array.next_abs_bin_idx + array.size);

            for bin_idx in array.next_abs_bin_idx..end_idx {
                let true_bin_idx = bin_idx & array.bin_time_mask;

                let sentinel_timer_idx = array.timer_indices.start + true_bin_idx as u32;

                if array_idx < array_count - 1 {
                    Self::expire_all(
                        &mut self.timer_entries,
                        &mut self.timer_data,
                        &mut self.free_list,
                        sentinel_timer_idx,
                        entries_out,
                    );
                } else {
                    Self::expire_expired(
                        &mut self.timer_entries,
                        &mut self.timer_data,
                        &mut self.free_list,
                        sentinel_timer_idx,
                        entries_out,
                        time_now_ms,
                    );
                }
            }

            array.next_abs_bin_idx = time_now_idx;
        }

        self.last_step_time_ms = time_now_ms;
    }
}

fn debug_fmt_row<T>(
    wheel: &TimerWheel<T>,
    timer_idx: u32,
    f: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    let entry = &wheel.timer_entries[timer_idx as usize];
    let data = &wheel.timer_data[timer_idx as usize];

    write!(
        f,
        "    [{:04}] next: {:04} prev: {:04} timeout_time_ms: {}",
        timer_idx, entry.next_idx, entry.prev_idx, entry.timeout_time_ms
    )?;

    if data.is_some() {
        writeln!(f, " data: Some(...)")?;
    } else {
        writeln!(f, "")?;
    }

    fmt::Result::Ok(())
}

impl<T> fmt::Debug for TimerWheel<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "TimerWheel {{")?;

        for (array_idx, array) in self.arrays.iter().enumerate() {
            writeln!(
                f,
                "  Array {} sentinels ({}ms per bin)",
                array_idx,
                1 << array.bin_time_shift
            )?;

            for timer_idx in array.timer_indices.clone() {
                debug_fmt_row(self, timer_idx, f)?;
            }
        }

        writeln!(f, "  Timers")?;

        let last_idx = self.arrays[self.arrays.len() - 1].timer_indices.end;

        for timer_idx in last_idx..self.timer_entries.len() as u32 {
            debug_fmt_row(self, timer_idx, f)?;
        }

        write!(f, "}}")?;

        fmt::Result::Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;
    use std::rc::Rc;

    static TEST_ARRAY_CONFIG: [ArrayConfig; 3] = [
        ArrayConfig {
            size: 8,
            ms_per_bin: 4,
        },
        ArrayConfig {
            size: 8,
            ms_per_bin: 4 * 4,
        },
        ArrayConfig {
            size: 8,
            ms_per_bin: 4 * 4 * 4,
        },
    ];

    struct TestTimerState {
        id: usize,
        timeout_time_ms: u64,
        expired_ids: Rc<RefCell<Vec<usize>>>,
    }

    impl TestTimerState {
        fn new(id: usize, timeout_time_ms: u64, expired_ids: Rc<RefCell<Vec<usize>>>) -> Self {
            Self {
                id,
                timeout_time_ms,
                expired_ids,
            }
        }
    }

    fn compute_expiration_time_ms(last_update_ms: u64, timeout_time_ms: u64) -> u64 {
        assert!(last_update_ms <= timeout_time_ms);
        assert!(TEST_ARRAY_CONFIG.len() == 3);

        // Array 0
        let next_bin_idx = last_update_ms / TEST_ARRAY_CONFIG[0].ms_per_bin as u64;
        let timeout_bin_idx = timeout_time_ms / TEST_ARRAY_CONFIG[0].ms_per_bin as u64;

        if timeout_bin_idx - next_bin_idx < TEST_ARRAY_CONFIG[0].size as u64 {
            return (timeout_bin_idx + 1) * TEST_ARRAY_CONFIG[0].ms_per_bin as u64;
        }

        // Array 1
        let next_bin_idx = last_update_ms / TEST_ARRAY_CONFIG[1].ms_per_bin as u64;
        let timeout_bin_idx = timeout_time_ms / TEST_ARRAY_CONFIG[1].ms_per_bin as u64;

        if timeout_bin_idx - next_bin_idx < TEST_ARRAY_CONFIG[1].size as u64 {
            return (timeout_bin_idx + 1) * TEST_ARRAY_CONFIG[1].ms_per_bin as u64;
        }

        // Array 2
        let timeout_bin_idx = timeout_time_ms / TEST_ARRAY_CONFIG[2].ms_per_bin as u64;
        return (timeout_bin_idx + 1) * TEST_ARRAY_CONFIG[2].ms_per_bin as u64;
    }

    struct TimerState {
        timeout_time_ms: u64,
        expire_time_ms: u64,
        id: Option<TimerId>,
    }

    struct TestHarness {
        wheel: TimerWheel<Rc<RefCell<TimerState>>>,
        expired_data: Vec<Rc<RefCell<TimerState>>>,
        timers: Vec<Rc<RefCell<TimerState>>>,
        last_step_time_ms: u64,
    }

    impl TimerState {
        fn new(timeout_time_ms: u64, expire_time_ms: u64) -> Self {
            Self {
                timeout_time_ms,
                expire_time_ms,
                id: None,
            }
        }
    }

    impl TestHarness {
        fn handler(
            wheel: &mut TimerWheel<Rc<RefCell<TimerState>>>,
            time_now_ms: u64,
            state: Rc<RefCell<TimerState>>,
        ) {
        }

        fn new() -> Self {
            let wheel = TimerWheel::new(&TEST_ARRAY_CONFIG, 0);

            Self {
                wheel,
                expired_data: Vec::new(),
                timers: Vec::new(),
                last_step_time_ms: 0,
            }
        }

        fn validate(&self) {
            for timer in self.timers.iter() {
                if self.last_step_time_ms >= timer.borrow().expire_time_ms {
                    // All timers which have passed their expire time should have an unset id,
                    // whether expired or explicitly unset
                    assert!(timer.borrow().id.is_none());
                }
            }
        }

        fn schedule(&mut self, timers: &[u64]) {
            for &timeout_time_ms in timers.iter() {
                assert!(timeout_time_ms >= self.last_step_time_ms);

                let expire_time_ms =
                    compute_expiration_time_ms(self.last_step_time_ms, timeout_time_ms);

                let timer_data = Rc::new(RefCell::new(TimerState::new(
                    timeout_time_ms,
                    expire_time_ms,
                )));

                let id = self
                    .wheel
                    .set_timer(timeout_time_ms, Rc::clone(&timer_data));

                timer_data.borrow_mut().id = Some(id);

                self.timers.push(timer_data);
            }
        }

        fn unset_random(&mut self) {
            if self.timers.len() > 0 {
                let data_idx = rand::random::<usize>() % self.timers.len();

                let mut timer = self.timers[data_idx].borrow_mut();

                if let Some(id) = timer.id.take() {
                    let timer_idx = id.idx;

                    self.wheel.unset_timer(id);
                }
            }
        }

        fn step(&mut self, interval_ms: u64) {
            let time_now_ms = self.last_step_time_ms + interval_ms;

            self.wheel.step(time_now_ms, &mut self.expired_data);

            for state in self.expired_data.drain(..) {
                let mut state_ref = state.borrow_mut();
                // Only timers which have passed their expire time should be handled
                assert!(time_now_ms >= state_ref.expire_time_ms);
                // Only timers which have not yet been handled (nor unset) should be handled
                assert!(state_ref.id.is_some());
                state_ref.id = None;
            }

            self.validate();

            self.last_step_time_ms = time_now_ms;
        }

        fn last_step_time_ms(&self) -> u64 {
            self.last_step_time_ms
        }
    }

    #[test]
    fn basic() {
        let mut fixture = TestHarness::new();

        fixture.schedule(&[0, 1, 2, 3, 4, 5, 6, 7, 8]);

        while fixture.last_step_time_ms() < 8 {
            fixture.step(1);
        }
    }

    #[test]
    fn basic_unset() {
        let mut wheel = TimerWheel::<()>::new(&TEST_ARRAY_CONFIG, 0);
        let mut expired_data = Vec::new();

        let timer_id = wheel.set_timer(0, ());

        wheel.unset_timer(timer_id);

        wheel.step(8, &mut expired_data);

        assert_eq!(expired_data.len(), 0);
    }

    #[test]
    fn single_fill() {
        const MAX_INTERVAL_MS: u64 = 11;

        const END_TIME_MS: u64 = 512;

        // Place two timers in each possible bin
        const TIMER_TIMES: [u64; 40] = [
            0, 3, 4, 7, 8, 11, 12, 15, 16, 19, 20, 23, 24, 27, 28, 31, 32, 47, 48, 63, 64, 79, 80,
            95, 96, 111, 112, 127, 128, 191, 192, 255, 256, 319, 320, 383, 384, 447, 448, 511,
        ];

        for interval_ms in 1..MAX_INTERVAL_MS + 1 {
            let mut fixture = TestHarness::new();

            fixture.schedule(&TIMER_TIMES);

            while fixture.last_step_time_ms() < END_TIME_MS {
                fixture.step(interval_ms);
            }
        }
    }

    fn random_test(
        intervals_ms: std::ops::Range<u64>,
        round_count: usize,
        round_interval_ms: u64,
        set_count: usize,
        set_range_ms: u64,
        unset_count: usize,
    ) {
        for interval_ms in intervals_ms {
            let mut fixture = TestHarness::new();

            for round_id in 0..round_count {
                let timeout_times = (0..set_count)
                    .map(|_| fixture.last_step_time_ms + rand::random::<u64>() % set_range_ms)
                    .collect::<Vec<_>>();

                fixture.schedule(&timeout_times);

                for _ in 0..unset_count {
                    fixture.unset_random();
                }

                let round_end_ms = (round_id as u64 + 1) * round_interval_ms;

                while fixture.last_step_time_ms() < round_end_ms {
                    fixture.step(interval_ms);
                }
            }
        }
    }

    #[test]
    fn single_random() {
        random_test(1..12, 1, 512, 200, 512, 0);
    }

    #[test]
    fn rolling_random() {
        random_test(1..12, 20, 256, 32, 1024, 0);
    }

    #[test]
    fn rolling_random_unset() {
        random_test(1..12, 20, 256, 32, 1024, 8);
    }

    #[test]
    fn reschedule_min() {
        // Reschedule a timer N times consecutively at the minimum possible interval, verifying
        // that the timer fires when expected.

        static MIN_RESOLUTION_MS: u64 = TEST_ARRAY_CONFIG[0].ms_per_bin as u64;
        static RESCHEDULE_COUNT: u64 = 10;
        static STEP_COUNT: u64 = RESCHEDULE_COUNT + 3;

        type CountRc = Rc<RefCell<u64>>;

        let mut wheel = TimerWheel::<CountRc>::new(&TEST_ARRAY_CONFIG, 0);
        let mut expired_data = Vec::<CountRc>::new();

        let count_rc = Rc::new(RefCell::new(0));

        let timer_id = wheel.set_timer(0, Rc::clone(&count_rc));

        for step_idx in 0..STEP_COUNT {
            let time_now_ms = (step_idx + 1) * MIN_RESOLUTION_MS;

            wheel.step(time_now_ms, &mut expired_data);

            for count_rc in expired_data.drain(..) {
                let mut count_ref = count_rc.borrow_mut();

                assert_eq!(time_now_ms, (*count_ref + 1) * MIN_RESOLUTION_MS);

                if *count_ref < RESCHEDULE_COUNT {
                    *count_ref += 1;

                    std::mem::drop(count_ref);

                    // In order to fire MIN_RESOLUTION_MS from now, schedule for the current bin
                    wheel.set_timer(time_now_ms, count_rc);
                }
            }
        }

        assert_eq!(*count_rc.borrow(), RESCHEDULE_COUNT);
    }

    #[test]
    fn expiration_time_basic() {
        // Set a single timer and verify that computed expiration time is the start of the
        // following bin.

        static BIN_SIZE_MS: u64 = TEST_ARRAY_CONFIG[0].ms_per_bin as u64;

        let mut wheel = TimerWheel::<()>::new(&TEST_ARRAY_CONFIG, 0);
        let mut expired_data = Vec::new();

        assert_eq!(wheel.next_expiration_time_ms(), None);

        wheel.set_timer(BIN_SIZE_MS, ());

        assert_eq!(wheel.next_expiration_time_ms(), Some(2 * BIN_SIZE_MS));

        wheel.step(2 * BIN_SIZE_MS, &mut expired_data);

        assert_eq!(wheel.next_expiration_time_ms(), None);
    }

    #[test]
    fn expiration_time_alias() {
        // Set a single timer such that it aliases in the final array, and verify expected aliasing
        // behavior.

        let mut wheel = TimerWheel::<()>::new(&TEST_ARRAY_CONFIG, 0);
        let mut expired_data = Vec::new();

        static SPAN_MS: u64 =
            ((TEST_ARRAY_CONFIG[2].size) * TEST_ARRAY_CONFIG[2].ms_per_bin) as u64;
        static EXPIRATION_PRE: u64 = TEST_ARRAY_CONFIG[2].ms_per_bin as u64;
        static EXPIRATION_POST: u64 = EXPIRATION_PRE + SPAN_MS;

        wheel.set_timer(SPAN_MS, ());

        assert_eq!(wheel.next_expiration_time_ms(), Some(EXPIRATION_PRE));

        // Verify that the new expiration is correct after advancing past the aliased bin
        wheel.step(EXPIRATION_PRE, &mut expired_data);

        assert_eq!(wheel.next_expiration_time_ms(), Some(EXPIRATION_POST));

        // Veirfy wheel reports no known expiration after expiring all
        wheel.step(EXPIRATION_POST, &mut expired_data);

        assert_eq!(wheel.next_expiration_time_ms(), None);
    }

    #[test]
    fn expiration_time_random() {
        // Set N random timers at a random time, and compare the wheel's computed expiration time
        // against the reference. Timers are set without aliasing, so that the reference time
        // always matches.

        const TRIAL_COUNT: usize = 100;
        const TIMER_COUNT: usize = 5;

        static TIMEOUT_SPAN_MS_NOALIAS: u64 =
            ((TEST_ARRAY_CONFIG[2].size - 1) * TEST_ARRAY_CONFIG[2].ms_per_bin) as u64;

        for _ in 0..TRIAL_COUNT {
            let mut wheel = TimerWheel::<()>::new(&TEST_ARRAY_CONFIG, 0);
            let mut expired_data = Vec::new();

            // Advance to a random time
            let mut step_time_ms = rand::random::<u64>() / 2;

            wheel.step(step_time_ms, &mut expired_data);

            // Set TIMER_COUNT random timers within the test range
            let timeout_times = (0..TIMER_COUNT)
                .map(|_| step_time_ms + rand::random::<u64>() % TIMEOUT_SPAN_MS_NOALIAS)
                .collect::<Vec<_>>();

            for &timeout_time_ms in timeout_times.iter() {
                let expire_time_ms = compute_expiration_time_ms(step_time_ms, timeout_time_ms);

                wheel.set_timer(timeout_time_ms, ());
            }

            // Compute true expiration min/max
            let expire_times = timeout_times
                .iter()
                .map(|&timeout_time_ms| compute_expiration_time_ms(step_time_ms, timeout_time_ms))
                .collect::<Vec<_>>();

            let mut min_expire_time = expire_times[0];
            let mut max_expire_time = expire_times[0];
            for &expire_time in expire_times.iter().skip(1) {
                min_expire_time = min_expire_time.min(expire_time);
                max_expire_time = max_expire_time.max(expire_time);
            }

            // Verify wheel's computed expiration time
            assert_eq!(wheel.next_expiration_time_ms(), Some(min_expire_time));

            // Veirfy wheel reports no known expiration after expiring all
            wheel.step(max_expire_time, &mut expired_data);

            assert_eq!(wheel.next_expiration_time_ms(), None);
        }
    }
}
