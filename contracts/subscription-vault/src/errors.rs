//! Stable error strings (LLD `contracts-vault.md` §3). The billing keeper parses these from
//! transaction failures (`collect`) and from `ft_on_transfer` refund logs; never rename them.

/// Mandate id unknown (never created, or closed and removed after its balance reached zero).
pub const ERR_NOT_FOUND: &str = "ERR_NOT_FOUND";
/// Mandate is `canceled` (collect, cancel, top_up).
pub const ERR_NOT_ACTIVE: &str = "ERR_NOT_ACTIVE";
/// `collect` before `start_at + collected_periods × period`.
pub const ERR_NOT_DUE: &str = "ERR_NOT_DUE";
/// A collect (or, for `withdraw`, a refund) of this mandate is still awaiting its callback.
pub const ERR_IN_FLIGHT: &str = "ERR_IN_FLIGHT";
/// `balance < amount_per_period` (collect) or nothing to withdraw / storage withdraw too large.
pub const ERR_INSUFFICIENT_BALANCE: &str = "ERR_INSUFFICIENT_BALANCE";
/// `max_periods` reached (mandate `exhausted`).
pub const ERR_MAX_PERIODS: &str = "ERR_MAX_PERIODS";
/// Caller is not allowed to do this to this mandate.
pub const ERR_UNAUTHORIZED: &str = "ERR_UNAUTHORIZED";
/// `ft_on_transfer` from a token outside `allowed_tokens`, or a top-up in another token.
pub const ERR_TOKEN_NOT_ALLOWED: &str = "ERR_TOKEN_NOT_ALLOWED";
/// `msg` does not parse or fails validation.
pub const ERR_BAD_MSG: &str = "ERR_BAD_MSG";
/// Deposit would take the token's TVL above its cap, or a mandate's balance (or its
/// `amount_per_period`) above the token's per-mandate cap. The `ft_on_transfer` log line adds
/// which cap after the code (`LOG_CAP_EXCEEDED_*`).
pub const ERR_CAP_EXCEEDED: &str = "ERR_CAP_EXCEEDED";
/// Log line: per-mandate cap (LLD v1.3).
pub const LOG_CAP_EXCEEDED_MANDATE: &str = "ERR_CAP_EXCEEDED: mandate";
/// Log line: per-token TVL cap (LLD v1.3).
pub const LOG_CAP_EXCEEDED_TVL: &str = "ERR_CAP_EXCEEDED: tvl";
/// `fee_bps > max_fee_bps`.
pub const ERR_FEE_TOO_HIGH: &str = "ERR_FEE_TOO_HIGH";
/// Payer has no NEP-145 storage balance, or not enough available for one more mandate.
pub const ERR_NO_STORAGE: &str = "ERR_NO_STORAGE";

// Added in LLD v1.2 (payer-only / deploy-only paths; the keeper never sees them).

/// `withdraw` on a mandate that is still `active` (cancel it first).
pub const ERR_STILL_ACTIVE: &str = "ERR_STILL_ACTIVE";
/// `storage_unregister` while the account still has open mandates.
pub const ERR_OPEN_MANDATES: &str = "ERR_OPEN_MANDATES";
/// Invalid constructor arguments (deploy time only).
pub const ERR_BAD_CONFIG: &str = "ERR_BAD_CONFIG";

// Added in LLD v1.4 (AI audits 2026-11-08).

/// `ft_on_transfer` whose `sender_id` is not the transaction signer (a deposit relayed by a
/// contract: the human payer would be unknown and refunds unattributable), is `intents_account`,
/// or is the vault itself. Refunded.
pub const ERR_UNSUPPORTED_SENDER: &str = "ERR_UNSUPPORTED_SENDER";
/// `collect` with less than `MIN_COLLECT_GAS`, or `cancel`/`withdraw` with less than
/// `MIN_EXIT_GAS`, attached (the keeper sees the `collect` case).
pub const ERR_GAS: &str = "ERR_GAS";

/// The stable code of a rejection log line: the text before the first `:` (LLD v1.3 §3.1 —
/// consumers match the leading code; anything after `": "` is detail).
pub fn code_of(line: &str) -> &str {
    line.split(':').next().unwrap_or(line)
}

/// Every error string the contract can emit, for tests and the audit index.
pub const ALL: [&str; 17] = [
    ERR_NOT_FOUND,
    ERR_NOT_ACTIVE,
    ERR_NOT_DUE,
    ERR_IN_FLIGHT,
    ERR_INSUFFICIENT_BALANCE,
    ERR_MAX_PERIODS,
    ERR_UNAUTHORIZED,
    ERR_TOKEN_NOT_ALLOWED,
    ERR_BAD_MSG,
    ERR_CAP_EXCEEDED,
    ERR_FEE_TOO_HIGH,
    ERR_NO_STORAGE,
    ERR_STILL_ACTIVE,
    ERR_OPEN_MANDATES,
    ERR_BAD_CONFIG,
    ERR_UNSUPPORTED_SENDER,
    ERR_GAS,
];
