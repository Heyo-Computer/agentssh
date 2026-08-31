use std::time::Duration;

pub const MAX_ATTEMPTS: u32 = 10;

/// Exponential backoff: 500ms base, doubling, ±20% jitter, capped at 15s.
pub fn delay(attempt: u32) -> Duration {
    use rand::RngExt;
    let base = 0.5_f64 * 2_f64.powi(attempt.saturating_sub(1) as i32);
    let capped = base.min(15.0);
    let jitter = rand::rng().random_range(0.8..1.2);
    Duration::from_secs_f64(capped * jitter)
}
