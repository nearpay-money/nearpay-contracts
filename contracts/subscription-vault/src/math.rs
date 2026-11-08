//! Pure integer arithmetic (INV-5). No floats, no unchecked overflow.

/// Basis-point denominator.
pub const BPS_DENOM: u128 = 10_000;
/// Nanoseconds per second.
pub const NS_PER_SEC: u128 = 1_000_000_000;

/// Splits one period's amount into `(merchant_share, fee)`:
/// `merchant_share = floor(amount × (10000 − fee_bps) / 10000)`, `fee = amount − merchant_share`.
///
/// Computed without the `amount × k` intermediate so it is exact for every `u128` amount:
/// with `amount = q·10000 + r`, `floor(amount·k/10000) = q·k + floor(r·k/10000)`.
/// Requires `fee_bps ≤ 10000` (callers validate; larger values saturate to a zero share).
pub fn split(amount: u128, fee_bps: u16) -> (u128, u128) {
    let k = BPS_DENOM.saturating_sub(u128::from(fee_bps));
    let q = amount / BPS_DENOM;
    let r = amount % BPS_DENOM;
    // q ≤ u128::MAX / 10000 and k ≤ 10000, so q·k cannot overflow; r·k < 10^8.
    let share = q * k + (r * k) / BPS_DENOM;
    (share, amount - share)
}

/// Timestamp (ns) at which period `collected_periods + 1` becomes collectable:
/// `start_at_ns + collected_periods × period_secs × 10^9`. `None` if it does not fit in `u64`
/// (that period can never become due).
pub fn due_at(start_at_ns: u64, collected_periods: u32, period_secs: u64) -> Option<u64> {
    let offset =
        u128::from(collected_periods).checked_mul(u128::from(period_secs))?.checked_mul(NS_PER_SEC)?;
    let due = u128::from(start_at_ns).checked_add(offset)?;
    u64::try_from(due).ok()
}

/// Number of periods that have started by `now_ns` (period 1 starts at `start_at_ns`).
/// Invariant V3: `collected_periods ≤ elapsed_periods(now)` at all times.
pub fn elapsed_periods(start_at_ns: u64, period_secs: u64, now_ns: u64) -> u128 {
    if now_ns < start_at_ns || period_secs == 0 {
        return 0;
    }
    let period_ns = u128::from(period_secs) * NS_PER_SEC;
    u128::from(now_ns - start_at_ns) / period_ns + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn split_examples() {
        assert_eq!(split(10_000_000, 100), (9_900_000, 100_000));
        assert_eq!(split(1, 0), (1, 0));
        assert_eq!(split(1, 1), (0, 1));
        assert_eq!(split(9_999, 1), (9_998, 1)); // floor(9999 × 9999 / 10000) = 9998
        assert_eq!(split(0, 500), (0, 0));
        assert_eq!(split(u128::MAX, 0), (u128::MAX, 0));
        assert_eq!(split(100, 10_000), (0, 100));
    }

    #[test]
    fn due_at_examples() {
        assert_eq!(due_at(5, 0, 86_400), Some(5));
        assert_eq!(due_at(0, 2, 86_400), Some(2 * 86_400 * 1_000_000_000));
        assert_eq!(due_at(u64::MAX, 1, 86_400), None);
        assert_eq!(due_at(0, u32::MAX, u64::MAX), None);
    }

    #[test]
    fn elapsed_examples() {
        let p = 86_400u64;
        let ns = 1_000_000_000u64;
        assert_eq!(elapsed_periods(100, p, 99), 0);
        assert_eq!(elapsed_periods(100, p, 100), 1);
        assert_eq!(elapsed_periods(100, p, 100 + p * ns - 1), 1);
        assert_eq!(elapsed_periods(100, p, 100 + p * ns), 2);
    }

    proptest! {
        #[test]
        fn split_is_exact_floor(amount in any::<u128>(), fee_bps in 0u16..=10_000) {
            let (share, fee) = split(amount, fee_bps);
            prop_assert_eq!(share + fee, amount);
            // Compare with the wide formula where it cannot overflow.
            if amount <= u128::MAX / BPS_DENOM {
                prop_assert_eq!(share, amount * (BPS_DENOM - u128::from(fee_bps)) / BPS_DENOM);
            }
            prop_assert!(share <= amount);
        }

        #[test]
        fn due_at_is_monotonic(start in any::<u64>(), n in 0u32..10_000, p in 86_400u64..(10 * 366 * 86_400)) {
            if let (Some(a), Some(b)) = (due_at(start, n, p), due_at(start, n + 1, p)) {
                prop_assert!(b > a);
            }
        }

        #[test]
        fn collected_never_exceeds_elapsed_when_due(start in 0u64..(1u64 << 62), n in 0u32..1_000, p in 86_400u64..(400 * 86_400)) {
            // If period n+1 is due at time t, then n+1 periods have elapsed at t.
            if let Some(t) = due_at(start, n, p) {
                prop_assert!(elapsed_periods(start, p, t) > u128::from(n));
                if t > start {
                    prop_assert!(elapsed_periods(start, p, t - 1) <= u128::from(n));
                }
            }
        }
    }
}
