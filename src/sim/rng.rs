//! The match's random number generator (`found-009`).
//!
//! Split out of the sim because it is the one piece of the simulation with no
//! state of its own beyond a counter, and because the tasks that consume it --
//! the chaos wave variants (`waves-002`), the economy's hazards (`06-economy`)
//! -- all want to name it without pulling in the sim.

/// SplitMix64: eight lines, no dependency, and identical on every platform,
/// which is the whole point. A generator that draws from the platform's entropy
/// or from the clock cannot be replayed, and a match that cannot be replayed
/// cannot be tested or reproduced from a bug report (`found-009`).
///
/// Quality is not the concern here -- the only consumers are jitter, and later
/// the chaos difficulty and the economy's hazards -- but a generator that
/// repeats itself after a few draws would be a real defect, and this one has a
/// 2^64 period.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// A generator that will produce the same sequence for the same `seed`,
    /// forever, and a different one for a different seed.
    pub fn from_seed(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The next float in `[0.0, 1.0)`, from the top 24 bits -- exactly the
    /// mantissa an `f32` can represent, so every value is equally likely and
    /// none is rounded into `1.0`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / 16_777_216.0
    }

    /// The next float in `[low, high)`. An empty or inverted range yields
    /// `low`, so a misconfigured table cannot panic here.
    pub fn range_f32(&mut self, low: f32, high: f32) -> f32 {
        if !(high > low) {
            return low;
        }
        low + (high - low) * self.next_f32()
    }
}
