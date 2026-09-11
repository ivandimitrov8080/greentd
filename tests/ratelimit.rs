//! The per-peer command budget (`audit-007`).
//!
//! The bucket is pure, so these tests are the whole of the statement "a burst
//! of `n + burst` commands in one frame produces exactly `burst` executions":
//! they drive a frame at a time with no `App`, no window and no socket.

use greentd::ratelimit::TokenBucket;

/// One frame at the server's 30 Hz tick.
const FRAME: f32 = 1.0 / 30.0;

#[test]
fn a_burst_taller_than_the_bucket_is_cut_down_to_the_bucket() {
    let burst = 4u32;
    let extra = 10;
    let mut bucket = TokenBucket::new(burst, 20.0);

    let executed = (0..burst as usize + extra)
        .filter(|_| bucket.try_consume())
        .count();

    assert_eq!(
        executed, burst as usize,
        "a frame may spend the burst and not one command more"
    );
}

#[test]
fn a_full_bucket_still_admits_the_first_command() {
    let mut bucket = TokenBucket::new(1, 1.0);
    assert!(bucket.try_consume());
    assert!(!bucket.try_consume());
}

#[test]
fn a_zero_burst_is_floored_at_one_command() {
    // A bucket that admits nothing reads to a player as a broken client rather
    // than as a rate limit, so the floor is one.
    let mut bucket = TokenBucket::new(0, 0.0);
    assert_eq!(bucket.capacity(), 1.0);
    assert!(bucket.try_consume());
    assert!(!bucket.try_consume());
}

#[test]
fn a_zero_length_frame_earns_nothing() {
    let mut bucket = TokenBucket::new(2, 1.0);
    assert!(bucket.try_consume());
    assert!(bucket.try_consume());
    assert!(!bucket.try_consume());

    bucket.refill(0.0);
    assert!(!bucket.try_consume(), "a zero-length frame buys no command");
}

#[test]
fn refill_earns_tokens_and_stops_at_the_burst() {
    let mut bucket = TokenBucket::new(5, 30.0);
    for _ in 0..5 {
        assert!(bucket.try_consume());
    }
    assert_eq!(bucket.tokens(), 0.0, "the burst is spent exactly");

    // A frame at 30 commands per second buys more than one command; a long
    // gap buys the burst back and no more.
    bucket.refill(1.0);
    assert_eq!(bucket.tokens(), bucket.capacity());
    bucket.refill(1000.0);
    assert_eq!(bucket.tokens(), bucket.capacity());
    assert_eq!(bucket.capacity(), 5.0);
}

#[test]
fn a_slow_rate_still_yields_something_within_a_frame() {
    // The budget is per second, but a well-behaved client sends a command every
    // so often, so the accumulation has to be fractional rather than
    // all-or-nothing.
    let mut bucket = TokenBucket::new(1, 2.0);
    assert!(bucket.try_consume());
    assert!(!bucket.try_consume());

    bucket.refill(0.6);
    assert!(bucket.try_consume(), "0.6 s at 2/s buys the next command");
    assert!(!bucket.try_consume());
}

#[test]
fn a_frame_of_time_earns_about_a_frame_of_commands() {
    // 30 commands per second is one command per frame. The rate is nudged to 31
    // so that what the test measures is the rate, not whether f32 rounding
    // landed on or just under 1.0.
    let mut bucket = TokenBucket::new(4, 31.0);
    for _ in 0..4 {
        assert!(bucket.try_consume());
    }
    bucket.refill(FRAME);
    assert!(
        bucket.tokens() > 1.0,
        "one frame at 31/s earns a bit more than one command"
    );
    assert!(bucket.try_consume(), "so the next command is admitted");
    assert!(!bucket.try_consume(), "and the one after it is not");
}

#[test]
fn a_misconfigured_rate_earns_nothing_rather_than_panicking() {
    for rate in [f32::NAN, f32::INFINITY, -1.0] {
        let mut bucket = TokenBucket::new(2, rate);
        assert!(bucket.try_consume());
        assert!(bucket.try_consume());
        bucket.refill(FRAME);
        assert!(
            !bucket.try_consume(),
            "a rate of {rate} must not mint commands"
        );
    }
}
