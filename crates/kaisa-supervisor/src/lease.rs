//! Leases: how long Kaisa keeps a resource for an attempt before it must be renewed
//! (architecture §5.1, ADR-0011).
//!
//! Every resource an attempt holds — its worktree, its process, its terminal — is held under a
//! lease. Renewal moves the expiry forward but never past a hard limit, so nothing is held
//! forever by accident. A lease the user asked to keep is retained and never expires.

/// A time-limited hold on a resource. Times are milliseconds on the supervisor's clock, which
/// callers pass in so that tests control time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    expires_at_ms: u64,
    max_until_ms: u64,
    retained: bool,
}

impl Lease {
    /// A lease granted at `now_ms` for `term_ms`, never extendable past `max_until_ms`.
    #[must_use]
    pub fn granted(now_ms: u64, term_ms: u64, max_until_ms: u64) -> Self {
        Self {
            expires_at_ms: now_ms.saturating_add(term_ms).min(max_until_ms),
            max_until_ms,
            retained: false,
        }
    }

    /// Renews the lease at `now_ms` for another `term_ms`. The expiry only moves forward, and
    /// never past the hard limit.
    pub fn renew(&mut self, now_ms: u64, term_ms: u64) {
        let renewed: u64 = now_ms.saturating_add(term_ms).min(self.max_until_ms);
        self.expires_at_ms = self.expires_at_ms.max(renewed);
    }

    /// Keeps the resource until the user releases it; a retained lease never expires.
    pub fn retain(&mut self) {
        self.retained = true;
    }

    /// When the lease runs out, unless it is retained.
    #[must_use]
    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// True once `now_ms` has reached the expiry of a lease that is not retained.
    #[must_use]
    pub fn is_expired(&self, now_ms: u64) -> bool {
        !self.retained && now_ms >= self.expires_at_ms
    }
}

#[cfg(test)]
mod tests {
    use super::Lease;

    /// One minute, in milliseconds.
    const MINUTE_MS: u64 = 60_000;
    /// When the attempt started: 2026-10-30 09:00 UTC.
    const NINE_O_CLOCK_MS: u64 = 1_793_350_800_000;
    /// How long one grant or renewal lasts.
    const TERM_MS: u64 = 5 * MINUTE_MS;
    /// A full day: far past any hard limit.
    const ONE_DAY_MS: u64 = 24 * 60 * MINUTE_MS;
    /// The hard limit: an hour after the start, whatever happens.
    const HARD_LIMIT_MS: u64 = NINE_O_CLOCK_MS + 60 * MINUTE_MS;

    fn a_worktree_lease_granted_at_nine() -> Lease {
        Lease::granted(NINE_O_CLOCK_MS, TERM_MS, HARD_LIMIT_MS)
    }

    #[test]
    fn renewing_a_lease_moves_its_expiry_forward() {
        let mut lease: Lease = a_worktree_lease_granted_at_nine();
        let renewed_at: u64 = NINE_O_CLOCK_MS + 3 * MINUTE_MS;

        lease.renew(renewed_at, TERM_MS);

        assert_eq!(
            lease.expires_at_ms(),
            renewed_at + TERM_MS,
            "a heartbeat keeps a live attempt's resources"
        );
    }

    #[test]
    fn a_lease_is_never_renewed_past_its_hard_limit() {
        let mut lease: Lease = a_worktree_lease_granted_at_nine();

        lease.renew(HARD_LIMIT_MS - MINUTE_MS, TERM_MS);

        assert_eq!(
            lease.expires_at_ms(),
            HARD_LIMIT_MS,
            "nothing is held forever by a process that keeps renewing"
        );
    }

    #[test]
    fn a_lease_expires_after_its_deadline() {
        let lease: Lease = a_worktree_lease_granted_at_nine();

        assert_eq!(
            (
                lease.is_expired(lease.expires_at_ms() - MINUTE_MS),
                lease.is_expired(lease.expires_at_ms())
            ),
            (false, true),
            "a lease holds until its deadline and not a moment longer"
        );
    }

    #[test]
    fn a_retained_lease_never_expires() {
        let mut lease: Lease = a_worktree_lease_granted_at_nine();

        lease.retain();

        assert!(
            !lease.is_expired(HARD_LIMIT_MS + ONE_DAY_MS),
            "work the user asked to keep stays until the user releases it"
        );
    }
}
