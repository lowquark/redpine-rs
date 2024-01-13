// This implementation of TCP-Reno-like congestion control has been informed by:
// https://intronetworks.cs.luc.edu/current/html/reno.html

// An initial TCP cwnd value of 10 * MSS is common on the post-2013 internet [19.2.5]
const INITIAL_CWND_MSS: u64 = 10;

enum Mode {
    UnboundedSlowStart,
    ThresholdSlowStart(u64),
    CongestionAvoidance,
}

pub struct AimdReno {
    mode: Mode,
    mss_q16: u64,
    cwnd_q16: u64,
    dropcnt_q16: u64,
}

impl AimdReno {
    pub fn new(mss: usize) -> Self {
        // (mss * mss) << 32 must be a valid u64
        assert!(mss <= u16::max_value().into());

        Self {
            mode: Mode::UnboundedSlowStart,
            mss_q16: (mss as u64) << 16,
            cwnd_q16: (INITIAL_CWND_MSS * mss as u64) << 16,
            dropcnt_q16: 0,
        }
    }

    pub fn handle_ack(&mut self) {
        // Window has advanced, subtract from drop counter
        if self.dropcnt_q16 > self.mss_q16 {
            self.dropcnt_q16 -= self.mss_q16;
        } else {
            self.dropcnt_q16 = 0;
        }

        match self.mode {
            Mode::UnboundedSlowStart => {
                // Double cwnd each RTT [19.2]
                self.cwnd_q16 = self.cwnd_q16.saturating_add(self.mss_q16);
            }
            Mode::ThresholdSlowStart(ssthresh) => {
                // Double cwnd each RTT, unless we would advance beyond ssthresh [19.2.2]
                let new_cwnd_q16 = self.cwnd_q16.saturating_add(self.mss_q16);
                if new_cwnd_q16 >= ssthresh {
                    // Enter congestion avoidance, beginning with ssthresh
                    self.cwnd_q16 = ssthresh;
                    self.mode = Mode::CongestionAvoidance;
                } else {
                    // Handle as if we were in unbounded slow start
                    self.cwnd_q16 = new_cwnd_q16;
                }
            }
            Mode::CongestionAvoidance => {
                // Compute mss*mss/cwnd and add to existing cwnd [19.2.1]
                let mss_q16 = self.mss_q16;
                let cwnd_q16 = self.cwnd_q16;
                let cwnd_inv_q16 = (mss_q16 * mss_q16 + cwnd_q16 / 2) / cwnd_q16;
                self.cwnd_q16 = cwnd_q16.saturating_add(cwnd_inv_q16);
            }
        }
    }

    pub fn handle_timeout(&mut self) {
        let cwnd_q16 = self.cwnd_q16;
        let cwnd_min_q16 = INITIAL_CWND_MSS * self.mss_q16;

        // The pipe has drained, reassess connection capacity in threshold slow start [19.2.2]
        self.cwnd_q16 = cwnd_min_q16;
        self.dropcnt_q16 = 0;
        self.mode = Mode::ThresholdSlowStart((cwnd_q16 / 2).max(cwnd_min_q16));
    }

    pub fn handle_drop(&mut self) {
        let cwnd_q16 = self.cwnd_q16;
        let cwnd_min_q16 = INITIAL_CWND_MSS * self.mss_q16;

        // Since no segments are retransmitted, we don't need to worry about window pinning. Also,
        // since the newest received segment ID is always sent back to the sender, the segment tx
        // queue has a reasonably accurate estimate of the number of bytes in transit, and we need
        // not worry about estimating the flight size here in the same way that TCP Reno does. It
        // suffices to halve cwnd and continue in congestion avoidance in all three modes. [19.4]

        // Halve cwnd and enter congestion avoidance, provided we haven't seen a drop recently
        if self.dropcnt_q16 == 0 {
            let new_cwnd_q16 = (cwnd_q16 / 2).max(cwnd_min_q16);
            self.cwnd_q16 = new_cwnd_q16;
            self.dropcnt_q16 = new_cwnd_q16;
            self.mode = Mode::CongestionAvoidance;
        }
    }

    pub fn cwnd(&self) -> usize {
        // Convert out of Q16 and into usize
        let cwnd_int = self.cwnd_q16 >> 16;
        if let Ok(cwnd_int) = cwnd_int.try_into() {
            cwnd_int
        } else {
            usize::max_value()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_cwnd_near(cwnd: usize, expected: f64) {
        let cwnd_f64 = cwnd as f64;

        if (cwnd_f64 - expected).abs() > 1.0 {
            panic!("expected cwnd near {}, found {}", expected, cwnd);
        }
    }

    #[test]
    fn basic() {
        let mss = 100;
        let mss_f64 = mss as f64;

        let mut cc = AimdReno::new(mss);
        let mut expected_cwnd = 1000.0;
        assert_cwnd_near(cc.cwnd(), expected_cwnd);

        for _ in 0..30 {
            cc.handle_ack();
            expected_cwnd += 100.0;
            assert_cwnd_near(cc.cwnd(), expected_cwnd);
        }

        cc.handle_drop();
        expected_cwnd /= 2.0;
        assert_cwnd_near(cc.cwnd(), expected_cwnd);

        cc.handle_drop();
        assert_cwnd_near(cc.cwnd(), expected_cwnd);

        for _ in 0..30 {
            cc.handle_ack();
            expected_cwnd += mss_f64 * mss_f64 / expected_cwnd;
            assert_cwnd_near(cc.cwnd(), expected_cwnd);
        }

        cc.handle_drop();
        expected_cwnd /= 2.0;
        assert_cwnd_near(cc.cwnd(), expected_cwnd);
    }
}
