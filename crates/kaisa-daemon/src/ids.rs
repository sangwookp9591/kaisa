//! Fresh identifiers: a prefix and a base-36 suffix from the clock, process id and a counter.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Distinguishes identifiers made within the same clock tick.
static COUNTER: AtomicU32 = AtomicU32::new(0);

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

/// A new suffix: nanoseconds, process id and a per-process counter. Fixed-width pid and
/// counter fields prevent ambiguous concatenations. At most 27 base-36 digits fit inside
/// the domain's 32-character limit (13 for u64, then 7 for each u32).
pub(crate) fn fresh_suffix() -> String {
    let nanos: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    suffix_at(
        nanos,
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    )
}

fn suffix_at(nanos: u64, process: u32, counter: u32) -> String {
    format!(
        "{}{:0>7}{:0>7}",
        base36(u128::from(nanos)),
        base36(u128::from(process)),
        base36(u128::from(counter))
    )
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{AttemptId, IntentId, MissionId, RunId, TaskId};

    use super::{fresh_suffix, suffix_at};

    /// Independent owners starting in exactly the same OS clock tick.
    const SIMULTANEOUS_OWNERS: [u32; 2] = [41, 42];
    /// A fixed tick and initial counter reproduce two freshly started CLI processes.
    const SAME_CLOCK_TICK: u64 = 1_793_448_000_000_000_000;
    const FIRST_COUNTER: u32 = 0;

    #[test]
    fn separate_processes_in_the_same_clock_tick_receive_distinct_suffixes() {
        let suffixes =
            SIMULTANEOUS_OWNERS.map(|pid| suffix_at(SAME_CLOCK_TICK, pid, FIRST_COUNTER));
        assert_ne!(
            suffixes.first(),
            suffixes.last(),
            "process-local counters must not collide across simultaneous owners"
        );
    }

    /// Both pairs used to spell `110` when the variable-width fields were concatenated.
    fn formerly_ambiguous_clock_and_counter_pairs() -> [(u64, u32); 2] {
        [(1, 36), (37, FIRST_COUNTER)]
    }

    #[test]
    fn adjacent_clock_and_counter_fields_cannot_alias_each_other() {
        let [before, after] = formerly_ambiguous_clock_and_counter_pairs()
            .map(|(tick, counter)| suffix_at(tick, SIMULTANEOUS_OWNERS[0], counter));
        assert_ne!(
            before, after,
            "variable-width base36 fields must not concatenate to the same identifier"
        );
    }

    #[test]
    fn fresh_and_largest_suffixes_fit_every_domain_id() {
        for suffix in [fresh_suffix(), suffix_at(u64::MAX, u32::MAX, u32::MAX)] {
            assert!(
                AttemptId::try_new(&format!("att_{suffix}")).is_ok(),
                "attempt suffix must fit domain validation: {suffix}"
            );
            assert!(
                MissionId::try_new(&format!("mis_{suffix}")).is_ok(),
                "mission suffix must fit domain validation: {suffix}"
            );
            assert!(
                RunId::try_new(&format!("run_{suffix}")).is_ok(),
                "run suffix must fit domain validation: {suffix}"
            );
            assert!(
                TaskId::try_new(&format!("task_{suffix}")).is_ok(),
                "task suffix must fit domain validation: {suffix}"
            );
            assert!(
                IntentId::try_new(&format!("intent_{suffix}")).is_ok(),
                "intent suffix must fit domain validation: {suffix}"
            );
        }
    }
}
