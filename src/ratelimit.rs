//! Per-peer command budgets (`audit-007`, `net-005`).
//!
//! `handle_cmds` used to execute every command that had arrived by the time it
//! ran, so the only bound on a peer was how many datagrams it could send: a
//! host that accepted a thousand build attempts per tick was a denial of
//! service against every other player in the match (D7).
//!
//! The budget is a token bucket. It holds `burst` tokens, earns `per_second`
//! tokens per second of frame time, and every accepted command costs one. A
//! command that finds the bucket empty is *dropped*, not deferred: a deferred
//! command would be executed a frame later, which is exactly the backlog the
//! bucket exists to prevent.
//!
//! The bucket is deliberately tiny and pure -- it knows nothing about peers,
//! commands or sockets -- so `tests/ratelimit.rs` drives it a frame at a time
//! with no `App`, no window and no socket.
//!
//! `net-005` extends this with a disconnect policy for a peer that keeps
//! abusing the budget across many frames; `audit-007` only drops the command
//! and answers once per frame.

/// A token bucket, sized in commands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenBucket {
    tokens: f32,
    capacity: f32,
    per_second: f32,
}

impl TokenBucket {
    /// A full bucket: `burst` commands may be spent immediately, and spending
    /// is sustained at `per_second` commands per second after that.
    ///
    /// A burst of zero would refuse every command, which reads to a player as a
    /// broken client rather than as a rate limit, so the floor is one. A
    /// non-finite or negative rate earns nothing, which is the safe reading of
    /// a misconfigured table.
    pub fn new(burst: u32, per_second: f32) -> Self {
        let capacity = burst.max(1) as f32;
        Self {
            tokens: capacity,
            capacity,
            per_second: if per_second.is_finite() {
                per_second.max(0.0)
            } else {
                0.0
            },
        }
    }

    /// Earn the tokens `elapsed` seconds bought, never exceeding the burst.
    ///
    /// One call per frame per peer, with that frame's delta, before any command
    /// is considered. `f32::min` returns the non-NaN operand, so a `NaN`
    /// `elapsed` leaves the bucket alone instead of poisoning it.
    pub fn refill(&mut self, elapsed: f32) {
        if elapsed <= 0.0 {
            return;
        }
        self.tokens = (self.tokens + self.per_second * elapsed).min(self.capacity);
    }

    /// Spend one token if there is one. `false` means "refuse this command".
    pub fn try_consume(&mut self) -> bool {
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Tokens left, for tests and diagnostics.
    pub fn tokens(&self) -> f32 {
        self.tokens
    }

    /// The burst the bucket was built with, i.e. the most it can ever hold.
    pub fn capacity(&self) -> f32 {
        self.capacity
    }
}
