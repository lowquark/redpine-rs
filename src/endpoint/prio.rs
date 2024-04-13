pub const CHANNEL_COUNT: usize = 4;
pub const DATA_SIZE_MAX: usize = u16::MAX as usize;
pub const BURST_SIZE_MAX: usize = DATA_SIZE_MAX * 255;
pub const WEIGHT_MAX: u32 = 256;

pub struct Config {
    weights: [u32; CHANNEL_COUNT],
    counter_max: u32,
}

impl Config {
    pub fn new(weights: &[u32; CHANNEL_COUNT], burst_size: usize) -> Self {
        for w in weights.iter() {
            assert!(*w != 0, "weights must not be zero (found: {w})");
            assert!(
                *w <= WEIGHT_MAX,
                "weights must not exceed {WEIGHT_MAX} (found: {w})"
            );
        }

        assert!(
            // A > floor(C / B) ==> A * B > C
            weights[0] * weights[1] * weights[2] <= WEIGHT_MAX / weights[3],
            "product of weights must not exceed {WEIGHT_MAX}"
        );

        assert!(burst_size != 0, "burst size must not be zero");

        assert!(
            burst_size <= BURST_SIZE_MAX,
            "burst size must not exceed {BURST_SIZE_MAX}"
        );

        let transformed_weights = [
            weights[1] * weights[2] * weights[3],
            weights[0] * weights[2] * weights[3],
            weights[0] * weights[1] * weights[3],
            weights[0] * weights[1] * weights[2],
        ];

        let total_product = weights[0] * weights[1] * weights[2] * weights[3];

        let counter_max = burst_size as u32 * total_product;

        Self {
            weights: transformed_weights,
            counter_max,
        }
    }
}

pub struct Prio {
    counters: [u32; CHANNEL_COUNT],
    weights: [u32; CHANNEL_COUNT],
    counter_max: u32,
}

impl Prio {
    pub fn new(config: Config) -> Self {
        Self {
            counters: [0, 0, 0, 0],
            weights: config.weights,
            counter_max: config.counter_max,
        }
    }

    pub fn next(&self, ch0: bool, ch1: bool, ch2: bool, ch3: bool) -> Option<u8> {
        let mut min_chan = None;
        let mut min_val = 0;
        let mut set = false;

        if ch0 && (!set || self.counters[0] < min_val) {
            min_chan = Some(0);
            min_val = self.counters[0];
            set = true;
        }

        if ch1 && (!set || self.counters[1] < min_val) {
            min_chan = Some(1);
            min_val = self.counters[1];
            set = true;
        }

        if ch2 && (!set || self.counters[2] < min_val) {
            min_chan = Some(2);
            min_val = self.counters[2];
            set = true;
        }

        if ch3 && (!set || self.counters[3] < min_val) {
            min_chan = Some(3);
        }

        return min_chan;
    }

    pub fn mark_sent(&mut self, channel_idx: u8, data_size: usize) {
        debug_assert!(channel_idx < CHANNEL_COUNT as u8);
        debug_assert!(data_size <= DATA_SIZE_MAX);

        let weight = self.weights[channel_idx as usize];
        let ref mut counter = self.counters[channel_idx as usize];
        let counter_max = self.counter_max;

        let weighted_size = data_size as u32 * weight;

        *counter = counter.wrapping_add(weighted_size);

        if *counter > counter_max {
            let excess = *counter - counter_max;

            for c in self.counters.iter_mut() {
                *c = c.saturating_sub(excess);
            }
        }

        /*
        println!(
            "sent {data_size} on ch{channel_idx:?} | {:5} : {:5} : {:5} : {:5} | max: {}",
            self.counters[0],
            self.counters[1],
            self.counters[2],
            self.counters[3],
            self.counter_max
        );
        */
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_channels() {
        let config = Config::new(&[1, 1, 2, 8], 10);
        let prio = Prio::new(config);

        let result = prio.next(true, false, false, false);
        assert_eq!(result, Some(0));

        let result = prio.next(false, true, false, false);
        assert_eq!(result, Some(1));

        let result = prio.next(false, false, true, false);
        assert_eq!(result, Some(2));

        let result = prio.next(false, false, false, true);
        assert_eq!(result, Some(3));

        let result = prio.next(false, false, false, false);
        assert_eq!(result, None);
    }

    #[test]
    fn simple() {
        let config = Config::new(&[1, 1, 2, 8], 10);
        let mut prio = Prio::new(config);

        for i in 0..20 {
            let next = prio.next(true, true, false, false).unwrap();

            if i % 2 == 0 {
                assert_eq!(next, 0);
            } else {
                assert_eq!(next, 1);
            }

            prio.mark_sent(next, 1);
        }
    }

    #[test]
    fn maxed_counters() {
        let weight = 4;
        let burst_size = BURST_SIZE_MAX;
        let counter_max = weight * weight * weight * weight * burst_size as u32;

        let config = Config::new(&[weight, weight, weight, weight], burst_size);
        let mut prio = Prio::new(config);

        assert_eq!(prio.counter_max, counter_max);

        // Send enough data to saturate each channel. Channels will send their weight * burst size
        // before saturating.

        assert_eq!(burst_size, DATA_SIZE_MAX * 255);

        let send_count = weight * (CHANNEL_COUNT as u32) * 255;

        for _ in 0..send_count {
            prio.mark_sent(prio.next(true, true, true, true).unwrap(), DATA_SIZE_MAX);
        }

        assert_eq!(prio.counters[0], counter_max);

        // Steady state should be reached now

        for _ in 0..CHANNEL_COUNT {
            prio.mark_sent(prio.next(true, true, true, true).unwrap(), DATA_SIZE_MAX);
        }

        assert_eq!(prio.counters[0], counter_max);
    }
}
