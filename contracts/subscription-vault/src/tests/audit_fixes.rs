//! Regression tests for the 2026-11-08 AI audits (`audits/2026-11-08-ai-audits.md`). Each test
//! names the finding(s) it covers; PoCs from the audits are kept, inverted where the code now
//! refuses the attack.

use super::*;

const REG: u128 = 2 * MILLINEAR;
const MCOST: u128 = 10 * MILLINEAR;
const DAY_S: u64 = 86_400;

/// A1-M-2 / A2-M-1: a deposit relayed by `intents_account` (Intents `ft_withdraw` with msg,
/// 1Click `customRecipientMsg`) would have made intents.near the payer. Now refunded.
#[test]
fn relayed_deposit_from_intents_account_is_refunded() {
    let mut s = Sim::new();
    // Anyone may register storage for any account (NEP-145) — the PoC's precondition.
    s.call(MALLORY, REG + MCOST, vec![], |v| v.storage_deposit(Some(acc(INTENTS)), None)).unwrap();
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string();
    let o = s.ft_on_transfer(USDC, INTENTS, 3 * AMOUNT, &msg);
    assert_eq!(o.result, Ok(3 * AMOUNT), "full refund");
    assert_eq!(o.logs, vec![ERR_UNSUPPORTED_SENDER.to_string()]);
    assert!(o.receipts.is_empty());
    assert_eq!(s.view(|v| v.next_id), 0);
    assert_eq!(s.tvl(USDC), 0);

    // Top-ups relayed by intents.near are refused too (even into someone else's mandate).
    let id = s.create_default();
    let top = json!({"top_up": {"mandate_id": id}}).to_string();
    let o = s.ft_on_transfer(USDC, INTENTS, AMOUNT, &top);
    assert_eq!((o.result, o.logs), (Ok(AMOUNT), vec![ERR_UNSUPPORTED_SENDER.to_string()]));
    // The vault itself as sender (impossible on-chain, refused anyway).
    let o = s.ft_on_transfer(USDC, VAULT, AMOUNT, &msg);
    assert_eq!(o.logs, vec![ERR_UNSUPPORTED_SENDER.to_string()]);
    // Checked before parsing: even a malformed msg reports the sender.
    assert_eq!(s.ft_on_transfer(USDC, INTENTS, 1, "x").logs, vec![ERR_UNSUPPORTED_SENDER.to_string()]);
    // A disallowed token still panics first.
    assert_eq!(s.ft_on_transfer(EVIL_TOKEN, INTENTS, 1, &msg).err(), ERR_TOKEN_NOT_ALLOWED);
}

fn rejected(s: &mut Sim, msg: &Value) -> bool {
    let o = s.ft_on_transfer(USDC, ALICE, AMOUNT, &msg.to_string());
    match o.result.clone().unwrap() {
        0 => false,
        r => {
            assert_eq!(r, AMOUNT);
            assert_eq!(o.logs, vec![ERR_BAD_MSG.to_string()], "{msg}");
            true
        }
    }
}

/// A2-M-3: values outside consumers' int4/JS ranges are refused at the source.
#[test]
fn inputs_are_bounded() {
    let mut s = Sim::new();
    s.register(ALICE, REG + 12 * MCOST);
    let base = create_msg(MERCHANT, "near_account", AMOUNT, 0);

    // period_secs ≤ 12 × 366 days (v1.5, re-audit N-3: every billing price up to year × 12)
    assert!(!rejected(&mut s, &with(base.clone(), "period_secs", json!(366 * DAY_S + 1))));
    assert!(!rejected(&mut s, &with(base.clone(), "period_secs", json!(12 * 366 * DAY_S))));
    assert!(rejected(&mut s, &with(base.clone(), "period_secs", json!(12 * 366 * DAY_S + 1))));
    assert!(rejected(&mut s, &with(base.clone(), "period_secs", json!(i32::MAX as u64 + 1)))); // audit PoC
    assert!(rejected(&mut s, &with(base.clone(), "period_secs", json!((1u64 << 60) + 1))));

    // max_periods ≤ 2^31 − 1
    assert!(!rejected(&mut s, &with(base.clone(), "max_periods", json!(i32::MAX))));
    assert!(rejected(&mut s, &with(base.clone(), "max_periods", json!(i32::MAX as u64 + 1))));
    assert!(rejected(&mut s, &with(base.clone(), "max_periods", json!(u32::MAX)))); // audit PoC

    // start_at ≤ now + 366 days (ns)
    let limit = T0 + 366 * DAY_S * 1_000_000_000;
    assert!(!rejected(&mut s, &with(base.clone(), "start_at", json!(limit.to_string()))));
    assert!(rejected(&mut s, &with(base.clone(), "start_at", json!((limit + 1).to_string()))));
    assert!(rejected(&mut s, &with(base.clone(), "start_at", json!(u64::MAX.to_string())))); // audit PoC

    // Every accepted mandate's numbers fit int4 / JS-safe integers.
    for m in s.view(|v| v.get_mandates_by_payer(acc(ALICE), None, Some(100))) {
        assert!(m.period_secs <= i32::MAX as u64);
        assert!(m.max_periods.is_none_or(|n| n <= i32::MAX as u32));
        assert!(m.next_collect_at.is_some_and(|t| t.0 <= limit));
    }
}

/// A1-M-1 / A2-M-5 / A1-L-1 / A2-L-5: permissionless callers cannot starve the merchant leg or
/// the callbacks: `collect` refuses less than `MIN_COLLECT_GAS`, and the static budgets do not
/// depend on the attached gas.
#[test]
fn collect_gas_floor_and_static_budgets() {
    // The floor covers every static child of collect plus collect's own work.
    assert!(
        MIN_COLLECT_GAS.as_gas()
            >= GAS_FT_TRANSFER_CALL.as_gas() + GAS_ON_MERCHANT_LEG.as_gas() + Gas::from_tgas(20).as_gas()
    );
    // 4.x tokens forward prepaid − 30 Tgas to the receiver: ≥ 70 Tgas for intents.near.
    assert!(GAS_FT_TRANSFER_CALL.as_gas() - Gas::from_tgas(30).as_gas() >= Gas::from_tgas(70).as_gas());
    // on_merchant_leg covers its children (fee leg + on_collected) with ≥ 15 Tgas to spare.
    assert!(
        GAS_ON_MERCHANT_LEG.as_gas()
            >= GAS_FT_TRANSFER.as_gas() + GAS_ON_COLLECTED.as_gas() + Gas::from_tgas(15).as_gas()
    );
    assert!(GAS_ON_COLLECTED >= Gas::from_tgas(20) && GAS_ON_REFUNDED >= Gas::from_tgas(20));
    // Keeper gas (LLD §3.3) has margin over the floor and fits even the pre-protocol-83 prepaid-gas
    // limit of 300 Tgas (1 PGas since protocol 83).
    assert!(Gas::from_tgas(250) >= MIN_COLLECT_GAS && Gas::from_tgas(250) <= Gas::from_tgas(300));
    // Exits: refund transfer + on_refunded static gas, with ≥ 15 Tgas for the method itself.
    assert!(
        MIN_EXIT_GAS.as_gas()
            >= GAS_FT_TRANSFER.as_gas() + GAS_ON_REFUNDED.as_gas() + Gas::from_tgas(15).as_gas()
    );

    let mut s = Sim::new();
    let id = s.create_default();
    let before = s.mandate(id);
    s.gas_tgas = MIN_COLLECT_GAS.as_tgas() - 1;
    let o = s.collect(id);
    assert_eq!(o.err(), ERR_GAS);
    s.gas_tgas = 300;
    assert_eq!(s.mandate(id), before, "refused collect changes nothing");
    // ERR_GAS comes first, even for unknown mandates (cheap, deterministic for the keeper).
    s.gas_tgas = 100;
    assert_eq!(s.collect(999).err(), ERR_GAS);

    // At exactly the floor: accepted, and the merchant leg still gets its full static budget.
    s.gas_tgas = MIN_COLLECT_GAS.as_tgas();
    let o = s.collect(id);
    let leg = acts(&o.receipts)
        .into_iter()
        .find_map(|a| match a {
            Act::Call { method, gas, .. } if method == "ft_transfer_call" => Some(gas),
            _ => None,
        })
        .unwrap();
    assert_eq!(leg, GAS_FT_TRANSFER_CALL);
    s.gas_tgas = 300;
    assert!(s.mandate(id).unwrap().in_flight);
}

/// A1-L-1 / A1-I-4.1: a failing callback (e.g. out of gas) cannot be recovered. This documents
/// the resulting state — the reason for the generous static gas (LLD v1.4 §3.10).
#[test]
fn failed_collect_callback_leaves_the_period_stuck() {
    let mut s = Sim::new();
    let id = s.create_default(); // 3 periods funded
    let (share, fee) = math::split(AMOUNT, 100);
    s.collect(id).unwrap();
    // on_merchant_leg runs and then fails: the runtime reverts all its state changes. The
    // merchant transfer before it stands (the tokens left).
    let o = s.call(VAULT, 0, vec![ok_u128(share)], move |v| {
        let _ = v.on_merchant_leg(id, U128(share), U128(fee));
        env::panic_str("ERR_SIMULATED_CALLBACK_OUT_OF_GAS")
    });
    assert_eq!(o.err(), "ERR_SIMULATED_CALLBACK_OUT_OF_GAS");
    let m = s.mandate(id).unwrap();
    assert!(m.in_flight, "in_flight is never cleared");
    assert_eq!(m.collected_periods, 0, "the delivered period is never counted");
    assert_eq!(m.balance, U128(2 * AMOUNT));
    // Further collects are refused forever.
    s.now += 400 * DAY_NS;
    assert_eq!(s.collect(id).err(), ERR_IN_FLIGHT);
    // The payer can still cancel and recover the spendable balance (V2 for `balance`).
    s.cancel(ALICE, id).unwrap();
    s.on_refunded(id, 2 * AMOUNT, Some(ALICE), ok_empty()).unwrap();
    // ...but the in-flight period stays attributed to the mandate: it never closes, its storage
    // lock is never released and the token TVL keeps counting it.
    let m = s.mandate(id).unwrap();
    assert!(m.in_flight);
    assert_eq!(m.balance, U128(0));
    assert_eq!(s.withdraw(ALICE, id).err(), ERR_INSUFFICIENT_BALANCE);
    assert_eq!(s.call(ALICE, 1, vec![], |v| v.storage_unregister(None)).err(), ERR_OPEN_MANDATES);
    assert_eq!(s.tvl(USDC), AMOUNT);
}

#[test]
fn failed_refund_callback_blocks_withdraw_forever() {
    let mut s = Sim::new();
    let id = s.create_default();
    s.cancel(ALICE, id).unwrap();
    // The refund transfer failed (tokens still in the vault) and on_refunded then failed too.
    let o = s.call(VAULT, 0, vec![failed()], move |v| {
        v.on_refunded(id, U128(3 * AMOUNT), Some(acc(ALICE)));
        env::panic_str("ERR_SIMULATED_CALLBACK_OUT_OF_GAS")
    });
    assert!(o.result.is_err());
    let m = s.mandate(id).unwrap();
    assert_eq!((m.status, m.balance, m.refund_in_flight), (Status::Canceled, U128(0), U128(3 * AMOUNT)));
    assert_eq!(s.withdraw(ALICE, id).err(), ERR_IN_FLIGHT);
    assert_eq!(s.tvl(USDC), 3 * AMOUNT);
}

/// A1-L-3 / A2-M-2 (accepted risk, documented): one capital holder can occupy the TVL cap with
/// refundable mandates. The v1.4 `start_at` bound limits how long a mandate can sit undue, but
/// cannot prevent squatting. Mitigation is operational (monitoring, pre-staged v2 account).
#[test]
fn accepted_risk_tvl_cap_squatting() {
    let mut cfg = default_config();
    let (cap, mcap) = (1_000_000_000u128, 100_000_000u128);
    cfg.tvl_caps[0] = TokenCap { token: acc(USDC), cap: U128(cap), mandate_cap: U128(mcap) };
    let mut s = Sim::with_config(cfg);
    s.register(MALLORY, REG + 10 * MCOST);
    let squat = with(
        create_msg(MALLORY, "near_account", mcap, 0),
        "start_at",
        json!((T0 + 366 * DAY_S * 1_000_000_000).to_string()),
    );
    let mut ids = vec![];
    for _ in 0..10 {
        let o = s.ft_on_transfer(USDC, MALLORY, mcap, &squat.to_string());
        assert_eq!(o.result, Ok(0));
        ids.push(o.events()[0]["data"][0]["mandate_id"].as_u64().unwrap());
    }
    assert_eq!(s.tvl(USDC), cap);
    s.register(ALICE, REG + MCOST);
    let o = s.ft_on_transfer(
        USDC,
        ALICE,
        AMOUNT,
        &create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string(),
    );
    assert_eq!(o.logs, vec![LOG_CAP_EXCEEDED_TVL.to_string()]);
    // The squatter leaves whenever they like, at no loss.
    let o = s.cancel(MALLORY, ids[0]);
    assert!(matches!(&acts(&o.receipts)[0], Act::Call { args, .. } if args["receiver_id"] == MALLORY));
}

/// A2-M-6 / A1-L-2 (billing-side fix): the payer signs `fee_bps`; the vault accepts any value
/// ≤ `max_fee_bps`, including 0. Billing's linkage must compare it with the agreed fee.
#[test]
fn payer_signed_fee_is_a_mandate_term() {
    let mut s = Sim::new();
    let id = s.create(ALICE, 3 * AMOUNT, &create_msg(MERCHANT, "intents_account", AMOUNT, 0));
    let evs = s.collect_ok(id);
    assert_eq!(evs[0]["data"][0]["fee"], "0");
    assert_eq!(evs[0]["data"][0]["merchant_share"], AMOUNT.to_string());
}

// ------------------------------------------------------------------ re-audit (audit 3)

const BRIDGE: &str = "bridge.test"; // any relaying contract other than intents_account

/// N-1 (re-audit PoC `poc_other_relayer_still_becomes_payer`, inverted): the payer must be the
/// transaction signer, so no relaying contract (bridge, router, smart wallet) can become payer.
#[test]
fn relayed_deposit_from_any_contract_is_refunded() {
    let mut s = Sim::new();
    s.call(MALLORY, REG + MCOST, vec![], |v| v.storage_deposit(Some(acc(BRIDGE)), None)).unwrap();
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string();
    // Alice signs a transaction in which bridge.test calls the token's ft_transfer_call.
    let o = s.ft_on_transfer_signed(USDC, BRIDGE, ALICE, 3 * AMOUNT, &msg);
    assert_eq!(o.result, Ok(3 * AMOUNT), "full refund");
    assert_eq!(o.logs, vec![ERR_UNSUPPORTED_SENDER.to_string()]);
    assert_eq!(s.view(|v| v.next_id), 0);
    // Same for top-ups of an existing mandate.
    let id = s.create_default();
    let top = json!({"top_up": {"mandate_id": id}}).to_string();
    let o = s.ft_on_transfer_signed(USDC, ALICE, MALLORY, AMOUNT, &top);
    assert_eq!(o.logs, vec![ERR_UNSUPPORTED_SENDER.to_string()]);
    // intents.near relaying a user's Intents withdrawal (signer = the user) is refused too, and so
    // is intents.near signing itself (the explicit check).
    assert_eq!(
        s.ft_on_transfer_signed(USDC, INTENTS, ALICE, AMOUNT, &msg).logs,
        vec![ERR_UNSUPPORTED_SENDER.to_string()]
    );
    assert_eq!(
        s.ft_on_transfer_signed(USDC, INTENTS, INTENTS, AMOUNT, &msg).logs,
        vec![ERR_UNSUPPORTED_SENDER.to_string()]
    );
    // A direct ft_transfer_call (signer = sender) still works.
    assert_eq!(s.ft_on_transfer(USDC, ALICE, AMOUNT, &top).result, Ok(0));
}

/// N-3 (re-audit PoC `poc_multi_year_price_is_refused_although_int4_safe`, inverted): a 2-year
/// price is accepted and collects once per 2 years.
#[test]
fn multi_year_period_is_accepted_and_collects_on_schedule() {
    let mut s = Sim::new();
    let two_years =
        with(create_msg(MERCHANT, "intents_account", AMOUNT, 100), "period_secs", json!(730 * DAY_S));
    let id = s.create(ALICE, 2 * AMOUNT, &two_years);
    assert!(12 * 366 * DAY_S < i32::MAX as u64);
    assert_eq!(s.collect_ok(id)[0]["data"][0]["period"], 1);
    s.now = T0 + 730 * DAY_NS - 1;
    assert_eq!(s.collect(id).err(), ERR_NOT_DUE);
    s.now = T0 + 730 * DAY_NS;
    assert_eq!(s.collect_ok(id)[0]["data"][0]["period"], 2);
    assert_eq!(s.mandate(id).unwrap().next_collect_at, Some(U64(T0 + 2 * 730 * DAY_NS)));
}

/// N-4: exits below `MIN_EXIT_GAS` fail early with `ERR_GAS` (not "Exceeded the prepaid gas"),
/// with no state change. (Success at exactly the floor is asserted in the sandbox,
/// `min_gas.rs`: the native harness uses test fees, far above mainnet's.)
#[test]
fn exits_need_min_exit_gas() {
    let mut s = Sim::new();
    let id = s.create_default();
    s.gas_tgas = 30; // common tooling default
    assert_eq!(s.cancel(ALICE, id).err(), ERR_GAS);
    s.gas_tgas = MIN_EXIT_GAS.as_tgas() - 1;
    assert_eq!(s.cancel(ALICE, id).err(), ERR_GAS);
    assert_eq!(s.mandate(id).unwrap().status, Status::Active);
    s.gas_tgas = 300;
    s.cancel(ALICE, id).unwrap();
    s.on_refunded(id, 3 * AMOUNT, Some(ALICE), failed()).unwrap();
    s.gas_tgas = MIN_EXIT_GAS.as_tgas() - 1;
    assert_eq!(s.withdraw(ALICE, id).err(), ERR_GAS);
    s.gas_tgas = 300;
    assert_eq!(s.mandate(id).unwrap().refund_in_flight, U128(0));
    s.withdraw(ALICE, id).unwrap();
    assert_eq!(s.mandate(id).unwrap().refund_in_flight, U128(3 * AMOUNT));
}
