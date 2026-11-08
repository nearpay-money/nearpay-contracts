//! NEP-297 events (LLD §4): `EVENT_JSON:{"standard":"nearpay_vault","version":"1.0.0",
//! "event":"<name>","data":[{...}]}`. `data` is a one-element array (NEP-141/171 convention).
//! Field order is fixed by the structs below; amounts are decimal strings, `start_at` is a
//! nanosecond decimal string, ids/periods/bps are JSON numbers.

use crate::types::ReceiverKind;
use near_sdk::json_types::{U64, U128};
use near_sdk::serde::Serialize;
use near_sdk::{AccountId, env, serde_json};

pub const STANDARD: &str = "nearpay_vault";
pub const VERSION: &str = "1.0.0";
pub const EVENT_JSON_PREFIX: &str = "EVENT_JSON:";

/// `collect_failed.reason` when the merchant leg delivered nothing.
pub const REASON_MERCHANT_TRANSFER_FAILED: &str = "merchant_transfer_failed";

#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
struct Envelope<'a, T: Serialize> {
    standard: &'static str,
    version: &'static str,
    event: &'a str,
    data: [T; 1],
}

#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct MandateCreated<'a> {
    pub mandate_id: u64,
    pub payer: &'a AccountId,
    pub merchant_receiver: &'a AccountId,
    pub token: &'a AccountId,
    pub amount_per_period: U128,
    pub period_secs: u64,
    pub start_at: U64,
    pub max_periods: Option<u32>,
    pub fee_bps: u16,
    pub ext_ref: &'a str,
    pub receiver_kind: ReceiverKind,
    pub balance: U128,
}

#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct ToppedUp {
    pub mandate_id: u64,
    pub amount: U128,
    pub balance: U128,
}

#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct Collected {
    pub mandate_id: u64,
    pub period: u32,
    pub amount: U128,
    pub merchant_share: U128,
    pub fee: U128,
    pub balance: U128,
}

#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct CollectFailed<'a> {
    pub mandate_id: u64,
    pub period: u32,
    pub reason: &'a str,
}

#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct Canceled<'a> {
    pub mandate_id: u64,
    pub by: &'a AccountId,
    pub refunded: U128,
}

#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct Withdrawn {
    pub mandate_id: u64,
    pub amount: U128,
}

#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct Exhausted {
    pub mandate_id: u64,
}

/// Formats one event line (without logging it).
pub fn format<T: Serialize>(event: &str, data: T) -> String {
    let envelope = Envelope { standard: STANDARD, version: VERSION, event, data: [data] };
    // Serialising these plain structs cannot fail.
    let json = serde_json::to_string(&envelope).unwrap_or_else(|_| env::abort());
    format!("{EVENT_JSON_PREFIX}{json}")
}

fn emit<T: Serialize>(event: &str, data: T) {
    env::log_str(&format(event, data));
}

pub fn mandate_created(e: MandateCreated<'_>) {
    emit("mandate_created", e);
}
pub fn topped_up(e: ToppedUp) {
    emit("topped_up", e);
}
pub fn collected(e: Collected) {
    emit("collected", e);
}
pub fn collect_failed(e: CollectFailed<'_>) {
    emit("collect_failed", e);
}
pub fn canceled(e: Canceled<'_>) {
    emit("canceled", e);
}
pub fn withdrawn(e: Withdrawn) {
    emit("withdrawn", e);
}
pub fn exhausted(e: Exhausted) {
    emit("exhausted", e);
}
