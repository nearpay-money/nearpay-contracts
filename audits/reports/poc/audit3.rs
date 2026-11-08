//! Audit 3 (re-audit of 44637ed..42cff4f) PoCs. Scratch copy only.

use super::*;

const REG: u128 = 2 * MILLINEAR;
const MCOST: u128 = 10 * MILLINEAR;
const DAY_S: u64 = 86_400;
const BRIDGE: &str = "bridge.test"; // any relaying contract other than intents_account

/// The v1.4 fix refuses only `sender_id == intents_account`. Any other relaying contract
/// (a bridge finalising a transfer with msg, a router, a smart wallet that cannot call `cancel`)
/// still becomes the payer: the human cannot cancel, and refunds go to the relayer.
#[test]
fn poc_other_relayer_still_becomes_payer() {
    let mut s = Sim::new();
    s.call(MALLORY, REG + MCOST, vec![], |v| v.storage_deposit(Some(acc(BRIDGE)), None)).unwrap();
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string();
    let o = s.ft_on_transfer(USDC, BRIDGE, 3 * AMOUNT, &msg);
    assert_eq!(o.result, Ok(0), "accepted");
    let id = o.events()[0]["data"][0]["mandate_id"].as_u64().unwrap();
    assert_eq!(s.mandate(id).unwrap().payer.as_str(), BRIDGE);
    assert_eq!(s.cancel(ALICE, id).err(), ERR_UNAUTHORIZED);
    let o = s.cancel(MERCHANT, id);
    assert!(matches!(&acts(&o.receipts)[0], Act::Call { args, .. } if args["receiver_id"] == BRIDGE));
}

/// Billing allows prices of `interval = year, interval_count 1..12` (billing.md §3). The v1.4
/// bound (`period_secs ≤ 366 d`) refuses every multi-year mandate, although 12 × 366 days still
/// fits int4.
#[test]
fn poc_multi_year_price_is_refused_although_int4_safe() {
    let mut s = Sim::new();
    s.register(ALICE, REG + MCOST);
    let two_years = with(create_msg(MERCHANT, "intents_account", AMOUNT, 100), "period_secs", json!(730 * DAY_S));
    let o = s.ft_on_transfer(USDC, ALICE, AMOUNT, &two_years.to_string());
    assert_eq!((o.result, o.logs), (Ok(AMOUNT), vec![ERR_BAD_MSG.to_string()]));
    assert!(12 * 366 * DAY_S < i32::MAX as u64);
}
