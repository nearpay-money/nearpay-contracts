//! `ft_on_transfer` `msg` parsing and validation (LLD §3). Pure functions: no host calls, so they
//! are fuzzed natively (`src/tests/msg_fuzz.rs`, `fuzz/`).
//!
//! Accepted shapes (unknown fields rejected, LLD v1.2 §3.1):
//! ```json
//! {"create_mandate":{"merchant_receiver":"m.near","receiver_kind":"intents_account",
//!   "amount_per_period":"10000000","period_secs":2592000,"start_at":"1731000000000000000",
//!   "max_periods":12,"fee_bps":100,"ext_ref":"sub_123"}}
//! {"top_up":{"mandate_id":7}}
//! ```

use crate::errors::{ERR_BAD_MSG, ERR_FEE_TOO_HIGH};
use crate::math;
use crate::types::ReceiverKind;
use near_sdk::json_types::{U64, U128};
use near_sdk::serde::Deserialize;
use near_sdk::{AccountId, serde_json};

/// Minimum period (LLD §3: `period_secs < 86400` is rejected).
pub const MIN_PERIOD_SECS: u64 = 86_400;
/// Maximum period: 12 × 366 days (LLD v1.5 §3.1): covers every billing price (`year × 12`) and
/// stays below 2^31 − 1, so consumers' int4 columns and JS numbers are safe.
pub const MAX_PERIOD_SECS: u64 = 12 * 366 * 86_400;
/// Maximum `max_periods`: 2^31 − 1 (fits int4; far below 2^53 for JS).
pub const MAX_MAX_PERIODS: u32 = i32::MAX as u32;
/// `start_at` may be at most this far in the future (ns): 366 days.
pub const MAX_START_DELAY_NS: u64 = 366 * 86_400 * 1_000_000_000;
/// Maximum `ext_ref` length in bytes (LLD §2).
pub const MAX_EXT_REF_LEN: usize = 64;
/// Upper bound on the `msg` length we try to parse (bytes). Longer messages are rejected
/// before parsing, bounding gas spent on hostile input.
pub const MAX_MSG_LEN: usize = 2_048;

/// A parsed `ft_on_transfer` message.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(crate = "near_sdk::serde", rename_all = "snake_case", deny_unknown_fields)]
pub enum VaultMsg {
    CreateMandate(CreateMandateMsg),
    TopUp(TopUpMsg),
}

/// `create_mandate` arguments. `token` is the calling token contract and `payer` the
/// `sender_id`; neither is accepted from `msg`.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(crate = "near_sdk::serde", deny_unknown_fields)]
pub struct CreateMandateMsg {
    pub merchant_receiver: AccountId,
    pub receiver_kind: ReceiverKind,
    pub amount_per_period: U128,
    pub period_secs: u64,
    /// Nanoseconds since the Unix epoch; omitted or past values start at the block time.
    #[serde(default)]
    pub start_at: Option<U64>,
    #[serde(default)]
    pub max_periods: Option<u32>,
    pub fee_bps: u16,
    pub ext_ref: String,
}

/// `top_up` arguments.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(crate = "near_sdk::serde", deny_unknown_fields)]
pub struct TopUpMsg {
    pub mandate_id: u64,
}

/// Parses `msg`. Any failure is `ERR_BAD_MSG`.
pub fn parse_msg(msg: &str) -> Result<VaultMsg, &'static str> {
    if msg.len() > MAX_MSG_LEN {
        return Err(ERR_BAD_MSG);
    }
    serde_json::from_str::<VaultMsg>(msg).map_err(|_| ERR_BAD_MSG)
}

/// `true` if `ext_ref` is ≤ 64 bytes of printable ASCII (0x20–0x7E).
pub fn ext_ref_is_valid(ext_ref: &str) -> bool {
    ext_ref.len() <= MAX_EXT_REF_LEN && ext_ref.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// Validates a `create_mandate` message against the contract configuration at block time
/// `now_ns`. Order (first failure wins): shape and range rules → `ERR_BAD_MSG`, then
/// `fee_bps > max_fee_bps` → `ERR_FEE_TOO_HIGH`, then a zero merchant share → `ERR_BAD_MSG`.
pub fn validate_create(
    m: &CreateMandateMsg,
    max_fee_bps: u16,
    vault: &AccountId,
    now_ns: u64,
) -> Result<(), &'static str> {
    let start_too_late = m.start_at.is_some_and(|s| s.0 > now_ns.saturating_add(MAX_START_DELAY_NS));
    if m.amount_per_period.0 == 0
        || m.period_secs < MIN_PERIOD_SECS
        || m.period_secs > MAX_PERIOD_SECS
        || m.max_periods == Some(0)
        || m.max_periods.is_some_and(|n| n > MAX_MAX_PERIODS)
        || start_too_late
        || !ext_ref_is_valid(&m.ext_ref)
        || &m.merchant_receiver == vault
    {
        return Err(ERR_BAD_MSG);
    }
    if m.fee_bps > max_fee_bps {
        return Err(ERR_FEE_TOO_HIGH);
    }
    let (share, _fee) = math::split(m.amount_per_period.0, m.fee_bps);
    if share == 0 {
        return Err(ERR_BAD_MSG);
    }
    Ok(())
}
