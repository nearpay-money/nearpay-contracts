//! Audit 2 PoC tests (access control / spec / DoS). Scratch copy only.

use super::*;

const REG: u128 = 2 * MILLINEAR;
const MCOST: u128 = 10 * MILLINEAR;

fn create_ok(s: &mut Sim, token: &str, payer: &str, amount: u128, msg: &Value) -> u64 {
    let o = s.ft_on_transfer(token, payer, amount, &msg.to_string());
    assert_eq!(o.result, Ok(0), "rejected: {:?}", o.logs);
    o.events()[0]["data"][0]["mandate_id"].as_u64().unwrap()
}

/// F-M1: a deposit relayed by `intents_account` (NEAR Intents `ft_withdraw` with `msg`, or a
/// 1Click `customRecipientMsg` delivery) makes `intents_account` the payer. The human who funded
/// it can never cancel/withdraw; a merchant cancel refunds via plain `ft_transfer` to
/// `intents_account`, which credits no Intents user.
#[test]
fn poc_intents_account_becomes_payer_and_receives_refund() {
    let mut s = Sim::new();
    // Anyone may register storage for intents.test (NEP-145 allows account_id = other).
    s.call(MALLORY, REG + MCOST, vec![], |v| v.storage_deposit(Some(acc(INTENTS)), None)).unwrap();
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100);
    // The token reports sender_id = intents.test (the relaying contract), not the human payer.
    let id = create_ok(&mut s, USDC, INTENTS, 3 * AMOUNT, &msg);
    assert_eq!(s.mandate(id).unwrap().payer, acc(INTENTS));
    // The human (ALICE) who funded it has no rights.
    assert_eq!(s.cancel(ALICE, id).err(), ERR_UNAUTHORIZED);
    // Merchant cancels: refund goes to intents.test with a plain ft_transfer (no msg → no
    // Intents ledger credit for anybody).
    let o = s.cancel(MERCHANT, id);
    let a = acts(&o.receipts);
    assert!(a.iter().any(|x| matches!(x, Act::Call { receiver, method, args, .. }
        if receiver == USDC && method == "ft_transfer" && args["receiver_id"] == INTENTS)));
}

/// F-M2: a single holder of `tvl_cap` tokens can occupy the whole TVL cap of a token with
/// mandates that never pay anyone (merchant = self, start far in the future), blocking every
/// other payer, and later recover 100% of the tokens. Caps are immutable.
#[test]
fn poc_tvl_cap_squatting_is_free_and_permanent_until_attacker_leaves() {
    let mut cfg = default_config();
    let cap = 1_000_000_000u128; // 1,000 USDC "low global TVL cap"
    let mcap = 100_000_000u128; // 100 USDC per mandate
    cfg.tvl_caps[0] = TokenCap { token: acc(USDC), cap: U128(cap), mandate_cap: U128(mcap) };
    let mut s = Sim::with_config(cfg);
    s.register(MALLORY, REG + 10 * MCOST); // 0.102 NEAR of storage
    let squat = with(
        create_msg(MALLORY, "near_account", mcap, 0),
        "start_at",
        json!(u64::MAX.to_string()),
    );
    let mut ids = vec![];
    for _ in 0..10 {
        ids.push(create_ok(&mut s, USDC, MALLORY, mcap, &squat));
    }
    assert_eq!(s.tvl(USDC), cap);
    // A legitimate payer is now refused.
    s.register(ALICE, REG + MCOST);
    let o = s.ft_on_transfer(USDC, ALICE, AMOUNT, &create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string());
    assert_eq!(o.result, Ok(AMOUNT));
    assert_eq!(o.logs, vec![LOG_CAP_EXCEEDED_TVL.to_string()]);
    // No collect can ever take the attacker's tokens (never due) ...
    assert_eq!(s.collect(ids[0]).err(), ERR_NOT_DUE);
    // ... and the attacker exits with everything whenever it likes.
    let o = s.cancel(MALLORY, ids[0]);
    let a = acts(&o.receipts);
    assert!(a.iter().any(|x| matches!(x, Act::Call { method, args, .. }
        if method == "ft_transfer" && args["receiver_id"] == MALLORY && args["amount"] == mcap.to_string())));
}

/// F-M4 (consumer gap): the payer chooses `fee_bps` (0..=max_fee_bps); 0 is accepted and no fee
/// leg is ever sent. Billing's linkage rule does not compare `fee_bps`.
#[test]
fn poc_payer_chosen_zero_fee_is_accepted() {
    let mut s = Sim::new();
    s.register(ALICE, REG + MCOST);
    let id = create_ok(&mut s, USDC, ALICE, 3 * AMOUNT, &create_msg(MERCHANT, "intents_account", AMOUNT, 0));
    let evs = s.collect_ok(id);
    assert_eq!(evs[0]["data"][0]["fee"], "0");
    assert_eq!(evs[0]["data"][0]["merchant_share"], AMOUNT.to_string());
}

/// Info: number edge cases accepted by the parser.
#[test]
fn poc_number_edge_cases() {
    let mut s = Sim::new();
    s.register(ALICE, REG + 4 * MCOST);
    // '+' sign and leading zeros in U128/U64 strings (Rust FromStr accepts them).
    let m = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "amount_per_period", json!("+0010000000"));
    let id = create_ok(&mut s, USDC, ALICE, AMOUNT, &m);
    assert_eq!(s.mandate(id).unwrap().amount_per_period, U128(AMOUNT));
    // period_secs above 2^53 (not exactly representable as a JS Number) is accepted and emitted as a
    // JSON number.
    let big = (1u64 << 60) + 1;
    let m = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "period_secs", json!(big));
    let o = s.ft_on_transfer(USDC, ALICE, AMOUNT, &m.to_string());
    assert_eq!(o.result, Ok(0));
    assert!(o.logs[0].contains(&format!("\"period_secs\":{big}")));
    // start_at = u64::MAX accepted: the mandate is never due.
    let m = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "start_at", json!(u64::MAX.to_string()));
    let id = create_ok(&mut s, USDC, ALICE, AMOUNT, &m);
    assert_eq!(s.mandate(id).unwrap().next_collect_at, Some(U64(u64::MAX)));
}

/// Info: receivers that are configuration accounts are accepted.
#[test]
fn poc_merchant_receiver_may_be_config_accounts() {
    let mut s = Sim::new();
    s.register(ALICE, REG + 3 * MCOST);
    for r in [INTENTS, TREASURY, USDC] {
        create_ok(&mut s, USDC, ALICE, AMOUNT, &create_msg(r, "near_account", AMOUNT, 0));
    }
}

/// Info: the merchant leg's gas floor is the static 35 Tgas regardless of what the caller of the
/// permissionless `collect` attached; the receiver's share is whatever the token leaves.
#[test]
fn poc_merchant_leg_static_gas_is_fixed() {
    let mut s = Sim::new();
    let id = s.create_default();
    let o = s.collect(id);
    let a = acts(&o.receipts);
    let leg = a
        .iter()
        .find_map(|x| match x {
            Act::Call { method, gas, weight, .. } if method == "ft_transfer_call" => Some((*gas, *weight)),
            _ => None,
        })
        .unwrap();
    assert_eq!(leg, (Gas::from_tgas(35), 1));
}

fn flush_and_gas(v: &mut Vault) -> u64 {
    v.mandates.flush();
    v.tvl.flush();
    v.storage_accounts.flush();
    v.payer_mandates.flush();
    env::state_write(v);
    env::used_gas().as_gas()
}

/// Gas margin probe (host-function gas only; wasm compute and base action costs excluded) of the
/// worst-case closing callbacks, which run on their static gas alone when the caller attaches the
/// minimum.
#[test]
fn probe_callback_host_gas() {
    let mut s = Sim::new();
    s.register(ALICE, REG + 2 * MCOST);
    let long = "a".repeat(56) + ".test"; // 61 chars
    s.register(&long, REG + MCOST);
    let mut m = create_msg(MERCHANT, "intents_account", AMOUNT, 100);
    m = with(m, "max_periods", json!(1));
    m = with(m, "ext_ref", json!("x".repeat(64)));
    let id = create_ok(&mut s, USDC, &long, AMOUNT, &m);
    let (share, fee) = math::split(AMOUNT, 100);
    s.collect(id).unwrap();
    assert!(s.on_merchant_leg(id, share, fee, ok_u128(share)).unwrap().is_none());
    let o = s.call(VAULT, 0, vec![ok_empty()], move |v| {
        v.on_collected(id, U128(share), U128(fee));
        flush_and_gas(v)
    });
    let g = o.unwrap();
    println!("on_collected (exhaust + close) host gas: {:.3} Tgas", g as f64 / 1e12);
    assert!(s.mandate(id).is_none());

    let id = create_ok(&mut s, USDC, &long, 3 * AMOUNT, &with(create_msg(MERCHANT, "intents_account", AMOUNT, 100), "ext_ref", json!("y".repeat(64))));
    s.cancel(&long, id).unwrap();
    let by = acc(&long);
    let o = s.call(VAULT, 0, vec![ok_empty()], move |v| {
        v.on_refunded(id, U128(3 * AMOUNT), Some(by));
        flush_and_gas(v)
    });
    println!("on_refunded (cancel + close) host gas: {:.3} Tgas", o.unwrap() as f64 / 1e12);
}

/// F-M3: values the billing mirror cannot store (`period_secs integer`, `max_periods integer`
/// are Postgres int4) are accepted and emitted.
#[test]
fn poc_values_outside_billing_int4_columns_are_accepted() {
    let mut s = Sim::new();
    s.register(MALLORY, REG + MCOST);
    let mut m = create_msg(MALLORY, "near_account", 1, 0);
    m = with(m, "period_secs", json!(i32::MAX as u64 + 1));
    m = with(m, "max_periods", json!(u32::MAX));
    let o = s.ft_on_transfer(USDC, MALLORY, 1, &m.to_string());
    assert_eq!(o.result, Ok(0));
    assert!(o.logs[0].contains("\"period_secs\":2147483648"));
    assert!(o.logs[0].contains("\"max_periods\":4294967295"));
}
