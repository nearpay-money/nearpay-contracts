//! State and view types (LLD §2). Amounts are `u128` base units in state (borsh) and decimal
//! strings (`U128`) in JSON; nanosecond timestamps are decimal strings (`U64`) in JSON.

use near_sdk::json_types::{U64, U128};
use near_sdk::{AccountId, near};

/// How the merchant share is delivered.
#[near(serializers = [borsh, json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverKind {
    /// `ft_transfer_call(intents_account, share, msg = merchant_receiver)`: credits the merchant's
    /// NEAR Intents balance.
    IntentsAccount,
    /// `ft_transfer(merchant_receiver, share)`: plain NEP-141 transfer.
    NearAccount,
}

/// Mandate lifecycle. `Active → Canceled` (payer or merchant), `Active → Exhausted`
/// (`max_periods` collected). Closed mandates are removed once their balance is zero and nothing
/// is in flight (LLD v1.2 §2).
#[near(serializers = [borsh, json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Active,
    Canceled,
    Exhausted,
}

/// One mandate (LLD §2, plus `refund_in_flight` from v1.2).
#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mandate {
    pub payer: AccountId,
    pub merchant_receiver: AccountId,
    pub receiver_kind: ReceiverKind,
    pub token: AccountId,
    pub amount_per_period: u128,
    pub period_secs: u64,
    pub start_at_ns: u64,
    pub max_periods: Option<u32>,
    pub fee_bps: u16,
    pub collected_periods: u32,
    /// Tokens held for this mandate and available to the next collect / refund. Excludes the
    /// amount of an in-flight collect (`amount_per_period` while `in_flight`) and of an in-flight
    /// refund (`refund_in_flight`).
    pub balance: u128,
    pub status: Status,
    /// A collect is awaiting its callbacks; `amount_per_period` has been moved out of `balance`.
    pub in_flight: bool,
    /// Amount of a refund (`cancel` / `withdraw`) awaiting its callback; 0 when none.
    pub refund_in_flight: u128,
    pub ext_ref: String,
}

impl Mandate {
    /// Tokens of this mandate the vault must still hold: `balance` + in-flight collect +
    /// in-flight refund (invariant V1 is the sum of this over all mandates of a token).
    pub fn held(&self) -> u128 {
        let collect = if self.in_flight { self.amount_per_period } else { 0 };
        self.balance + collect + self.refund_in_flight
    }

    /// `true` once the max number of periods has been collected.
    pub fn max_reached(&self) -> bool {
        self.max_periods.is_some_and(|max| self.collected_periods >= max)
    }
}

/// Per-account NEP-145 storage balance. `locked = registration + mandates × mandate cost`.
#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageAccount {
    /// Total yoctoNEAR deposited and not withdrawn.
    pub total: u128,
    /// Open (not yet removed) mandates whose storage this account pays for.
    pub mandates: u32,
}

/// Per-token caps, a constructor argument: `cap` bounds the token's TVL (Σ held for all its
/// mandates), `mandate_cap` bounds one mandate (its balance after any deposit, and its
/// `amount_per_period`). `mandate_cap ≤ cap`.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenCap {
    pub token: AccountId,
    pub cap: U128,
    pub mandate_cap: U128,
}

/// NEP-145 `StorageBalance`.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageBalance {
    pub total: U128,
    pub available: U128,
}

/// NEP-145 `StorageBalanceBounds`.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageBalanceBounds {
    pub min: U128,
    pub max: Option<U128>,
}

/// `get_mandate` / `get_mandates_by_payer` result.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MandateView {
    pub mandate_id: u64,
    pub payer: AccountId,
    pub merchant_receiver: AccountId,
    pub receiver_kind: ReceiverKind,
    pub token: AccountId,
    pub amount_per_period: U128,
    pub period_secs: u64,
    pub start_at: U64,
    pub max_periods: Option<u32>,
    pub fee_bps: u16,
    pub collected_periods: u32,
    pub balance: U128,
    pub status: Status,
    pub in_flight: bool,
    pub refund_in_flight: U128,
    pub ext_ref: String,
    pub next_collect_at: Option<U64>,
}

/// `get_config` result: the constructor parameters (immutable) and fixed constants.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigView {
    pub allowed_tokens: Vec<AccountId>,
    pub fee_receiver: AccountId,
    pub max_fee_bps: u16,
    pub intents_account: AccountId,
    pub min_period_secs: u64,
    /// Per-token TVL and per-mandate caps, in `allowed_tokens` order.
    pub caps: Vec<TokenCap>,
    pub mandate_storage_cost: U128,
    pub storage_registration_cost: U128,
}

/// `get_token_tvl` result.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenTvlView {
    pub token: AccountId,
    pub tvl: U128,
    pub cap: U128,
    pub mandate_cap: U128,
}
