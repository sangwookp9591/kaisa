//! Fresh identifiers: a prefix and a base-36 suffix from the clock and a counter.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Distinguishes identifiers made within the same clock tick.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Base-36 digits, lowercase, as identifier suffixes require.
const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn base36(mut value: u128) -> String {
    let mut text: Vec<u8> = Vec::new();
    loop {
        text.push(DIGITS[usize::try_from(value % 36).unwrap_or(0)]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    text.reverse();
    String::from_utf8(text).unwrap_or_default()
}

/// A new suffix: nanoseconds since the epoch, then a per-process counter.
pub(crate) fn fresh_suffix() -> String {
    let nanos: u128 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!(
        "{}{}",
        base36(nanos),
        base36(u128::from(COUNTER.fetch_add(1, Ordering::Relaxed)))
    )
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
