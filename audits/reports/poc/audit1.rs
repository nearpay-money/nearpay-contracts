//! Audit-1 PoC tests (scratch copy only).
use super::*;

const REG: u128 = 2 * MILLINEAR;
const MCOST: u128 = 10 * MILLINEAR;

fn tg(g: Gas) -> f64 { g.as_gas() as f64 / 1e12 }

/// Host-side gas (receipt fees, prepaid static gas of children, storage/log host calls) used by
/// each callback in the mocked VM. Wasm instruction cost is NOT metered natively.
#[test]
fn audit_callback_host_gas() {
    let mut s = Sim::new();
    // max_periods 1, exactly funded -> on_collected exhausts AND removes the mandate (worst path).
    let msg = with(create_msg(MERCHANT, "intents_account", AMOUNT, 100), "max_periods", json!(1));
    let id = s.create(ALICE, AMOUNT, &msg);
    let (share, fee) = math::split(AMOUNT, 100);
    let g = s.call(MALLORY, 0, vec![], move |v| { let p = v.collect(id); drop(p); env::used_gas() }).unwrap();
    println!("AUDIT collect host gas (incl. 70 Tgas static children): {:.3} Tgas", tg(g));
    let g = s.call(VAULT, 0, vec![ok_u128(share)], move |v| {
        let r = v.on_merchant_leg(id, U128(share), U128(fee)); drop(r);
        env::state_write(&*v);
        env::used_gas()
    }).unwrap();
    println!("AUDIT on_merchant_leg host gas (incl. 18 Tgas static children): {:.3} Tgas of {:.0} static", tg(g), tg(GAS_ON_MERCHANT_LEG));
    let g = s.call(VAULT, 0, vec![ok_empty()], move |v| {
        let r = v.on_collected(id, U128(share), U128(fee));
        assert!(r);
        v.mandates.flush(); v.payer_mandates.flush(); v.storage_accounts.flush(); v.tvl.flush();
        env::state_write(&*v);
        env::used_gas()
    }).unwrap();
    println!("AUDIT on_collected (exhaust+close) host gas: {:.3} Tgas of {:.0} static", tg(g), tg(GAS_ON_COLLECTED));
    assert!(s.mandate(id).is_none());

    let id = s.create_default();
    s.cancel(ALICE, id).unwrap();
    let g = s.call(VAULT, 0, vec![ok_empty()], move |v| {
        let r = v.on_refunded(id, U128(3 * AMOUNT), Some(acc(ALICE)));
        assert!(r);
        v.mandates.flush(); v.payer_mandates.flush(); v.storage_accounts.flush(); v.tvl.flush();
        env::state_write(&*v);
        env::used_gas()
    }).unwrap();
    println!("AUDIT on_refunded (close) host gas: {:.3} Tgas of {:.0} static", tg(g), tg(GAS_ON_REFUNDED));
}

/// F: a mandate funded through NEAR Intents (`ft_withdraw` with `msg`, or a 1Click
/// `customRecipientMsg` deposit) arrives with `sender_id = intents_account`. The vault makes
/// intents.near the payer: the real payer can never cancel, and any refund is a plain
/// `ft_transfer` to intents.near, which credits nobody.
#[test]
fn audit_mandate_funded_via_intents_has_intents_as_payer() {
    let mut s = Sim::new();
    s.register(INTENTS, REG + MCOST); // anyone may register any account (NEP-145)
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string();
    let o = s.ft_on_transfer(USDC, INTENTS, 3 * AMOUNT, &msg);
    assert_eq!(o.result, Ok(0), "accepted");
    let id = o.events()[0]["data"][0]["mandate_id"].as_u64().unwrap();
    let m = s.mandate(id).unwrap();
    assert_eq!(m.payer.as_str(), INTENTS);
    // The human payer cannot cancel.
    assert_eq!(s.cancel(ALICE, id).err(), ERR_UNAUTHORIZED);
    // Merchant cancel refunds to intents.near by plain ft_transfer (no msg => nobody credited).
    let o = s.cancel(MERCHANT, id);
    match &acts(&o.receipts)[0] {
        Act::Call { method, args, .. } => {
            assert_eq!(method, "ft_transfer");
            assert_eq!(args["receiver_id"], INTENTS);
        }
        a => panic!("{a:?}"),
    }
}

/// Impact of ANY failed collect callback (e.g. out of gas): `in_flight` stays set forever.
/// Simulated by never delivering `on_merchant_leg` (a reverted callback leaves exactly this
/// state). There is no recovery path: the in-flight amount is locked, the mandate never closes,
/// the storage lock is never released, and the token TVL is permanently over-counted.
#[test]
fn audit_failed_collect_callback_locks_period_forever() {
    let mut s = Sim::new();
    let id = s.create_default();
    s.collect(id).unwrap(); // merchant leg delivered on-chain; on_merchant_leg reverted
    s.cancel(ALICE, id).unwrap();
    s.on_refunded(id, 2 * AMOUNT, Some(ALICE), ok_empty()).unwrap();
    let m = s.mandate(id).unwrap();
    assert!(m.in_flight);
    assert_eq!(s.withdraw(ALICE, id).err(), ERR_INSUFFICIENT_BALANCE);
    assert_eq!(s.collect(id).err(), ERR_NOT_ACTIVE);
    let o = s.call(ALICE, 1, vec![], |v| v.storage_unregister(None));
    assert_eq!(o.err(), ERR_OPEN_MANDATES);
    assert_eq!(s.tvl(USDC), AMOUNT, "TVL keeps counting the stuck period");
    // Before cancel the keeper would see ERR_IN_FLIGHT forever.
}

/// M-1 PoC: `collect` accepts any prepaid gas that covers its own work plus the 70 Tgas of static
/// children. At the minimum, unused gas ≈ 0, so the merchant `ft_transfer_call` gets exactly its
/// 35 Tgas static gas; a near-contract-standards 4.x token forwards `prepaid − 30 Tgas` = 5 Tgas
/// to `intents.near::ft_on_transfer`. (Run with AUDIT_MAINNET_FEES=1 for mainnet fee params.)
#[test]
fn audit_collect_accepts_minimal_gas() {
    let mut s = Sim::new();
    let id = s.create_default();
    let mut lo = 1u64; // Tgas
    let mut hi = 300u64;
    // binary search the minimal prepaid gas for which collect does not panic (state restored each try)
    while lo < hi {
        let mid = (lo + hi) / 2;
        let storage = Sim::take_storage();
        let snapshot = storage.clone();
        let mut ctx = s.context(MALLORY, 0);
        ctx.prepaid_gas = Gas::from_tgas(mid);
        let (vmc, fees) = if std::env::var("AUDIT_MAINNET_FEES").is_ok() {
            let c = near_parameters::RuntimeConfigStore::new(None).get_config(u32::MAX).clone();
            ((*c.wasm_config).clone(), (*c.fees).clone())
        } else {
            (test_vm_config(), RuntimeFeesConfig::test())
        };
        env::set_blockchain_interface(MockedBlockchain::new(ctx, vmc, fees, vec![], storage, HashMap::new(), None));
        let mut v: Vault = env::state_read().unwrap();
        let r = catch_unwind(AssertUnwindSafe(|| { let p = v.collect(id); drop(p); env::used_gas() }));
        std::mem::forget(v);
        s.install(VAULT, 0, vec![], snapshot);
        match r { Ok(_) => hi = mid, Err(_) => lo = mid + 1 }
    }
    println!("AUDIT collect succeeds natively with prepaid = {lo} Tgas (static children 70 Tgas); \
              merchant leg then gets ~{} Tgas -> 4.x token forwards ~{} Tgas to intents.near",
             35 + (lo.saturating_sub(73)) / 2, 5 + (lo.saturating_sub(73)) / 2);
    assert!(lo < 100, "collect has no minimum-gas guard");
}

/// L-3 PoC: one actor fills the per-token TVL cap with self-funded mandates (refundable at any
/// time) and blocks every new mandate and top-up for that token.
#[test]
fn audit_tvl_cap_can_be_squatted_and_released() {
    let mut s = Sim::new();
    let n = (CAP / MANDATE_CAP) as usize;
    let mut ids = vec![];
    for _ in 0..n {
        ids.push(s.create(MALLORY, MANDATE_CAP, &create_msg(MALLORY, "near_account", AMOUNT, 0)));
    }
    assert_eq!(s.tvl(USDC), CAP);
    s.register(ALICE, REG + MCOST);
    let o = s.ft_on_transfer(USDC, ALICE, AMOUNT, &create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string());
    assert_eq!(o.result, Ok(AMOUNT));
    assert_eq!(o.logs, vec![errors::LOG_CAP_EXCEEDED_TVL.to_string()]);
    // The squatter exits whenever it likes, losing nothing but gas.
    for id in ids {
        s.cancel(MALLORY, id).unwrap();
        s.on_refunded(id, MANDATE_CAP, Some(MALLORY), ok_empty()).unwrap();
    }
    assert_eq!(s.tvl(USDC), 0);
}
