//! Unit tests: one or more per LLD rule and per error string.

use super::*;

const REG: u128 = 2 * MILLINEAR; // STORAGE_REGISTRATION_COST
const MCOST: u128 = 10 * MILLINEAR; // MANDATE_STORAGE_COST

fn share_fee(amount: u128, bps: u16) -> (u128, u128) {
    math::split(amount, bps)
}

fn event_names(evs: &[Value]) -> Vec<String> {
    evs.iter().map(|e| e["event"].as_str().unwrap().to_string()).collect()
}

// ============================================================== constants & formats

#[test]
fn constants_match_lld() {
    assert_eq!(MANDATE_STORAGE_COST.as_yoctonear(), MCOST); // ≥ 0.01 NEAR per mandate
    assert_eq!(STORAGE_REGISTRATION_COST.as_yoctonear(), REG);
    assert_eq!(msg::MIN_PERIOD_SECS, 86_400);
    assert_eq!(msg::MAX_EXT_REF_LEN, 64);
    assert_eq!(events::STANDARD, "nearpay_vault");
    assert_eq!(events::VERSION, "1.0.0");
    // Error strings are their own names (the keeper parses them verbatim).
    let names = [
        "ERR_NOT_FOUND",
        "ERR_NOT_ACTIVE",
        "ERR_NOT_DUE",
        "ERR_IN_FLIGHT",
        "ERR_INSUFFICIENT_BALANCE",
        "ERR_MAX_PERIODS",
        "ERR_UNAUTHORIZED",
        "ERR_TOKEN_NOT_ALLOWED",
        "ERR_BAD_MSG",
        "ERR_CAP_EXCEEDED",
        "ERR_FEE_TOO_HIGH",
        "ERR_NO_STORAGE",
        "ERR_STILL_ACTIVE",
        "ERR_OPEN_MANDATES",
        "ERR_BAD_CONFIG",
    ];
    assert_eq!(errors::ALL, names);
}

#[test]
fn event_wire_format_is_exact() {
    let a = acc(ALICE);
    let m = acc(MERCHANT);
    let t = acc(USDC);
    let line = events::format(
        "mandate_created",
        events::MandateCreated {
            mandate_id: 0,
            payer: &a,
            merchant_receiver: &m,
            token: &t,
            amount_per_period: U128(10),
            period_secs: 86_400,
            start_at: U64(T0),
            max_periods: None,
            fee_bps: 100,
            ext_ref: "sub_1",
            receiver_kind: ReceiverKind::IntentsAccount,
            balance: U128(30),
        },
    );
    assert_eq!(
        line,
        r#"EVENT_JSON:{"standard":"nearpay_vault","version":"1.0.0","event":"mandate_created","data":[{"mandate_id":0,"payer":"alice.test","merchant_receiver":"merchant.test","token":"usdc.test","amount_per_period":"10","period_secs":86400,"start_at":"1731000000000000000","max_periods":null,"fee_bps":100,"ext_ref":"sub_1","receiver_kind":"intents_account","balance":"30"}]}"#
    );
    assert_eq!(
        events::format("topped_up", events::ToppedUp { mandate_id: 1, amount: U128(5), balance: U128(9) }),
        r#"EVENT_JSON:{"standard":"nearpay_vault","version":"1.0.0","event":"topped_up","data":[{"mandate_id":1,"amount":"5","balance":"9"}]}"#
    );
    assert_eq!(
        events::format(
            "collected",
            events::Collected {
                mandate_id: 1,
                period: 2,
                amount: U128(10),
                merchant_share: U128(9),
                fee: U128(1),
                balance: U128(0)
            }
        ),
        r#"EVENT_JSON:{"standard":"nearpay_vault","version":"1.0.0","event":"collected","data":[{"mandate_id":1,"period":2,"amount":"10","merchant_share":"9","fee":"1","balance":"0"}]}"#
    );
    assert_eq!(
        events::format(
            "collect_failed",
            events::CollectFailed {
                mandate_id: 1,
                period: 2,
                reason: events::REASON_MERCHANT_TRANSFER_FAILED
            }
        ),
        r#"EVENT_JSON:{"standard":"nearpay_vault","version":"1.0.0","event":"collect_failed","data":[{"mandate_id":1,"period":2,"reason":"merchant_transfer_failed"}]}"#
    );
    assert_eq!(
        events::format("canceled", events::Canceled { mandate_id: 1, by: &a, refunded: U128(7) }),
        r#"EVENT_JSON:{"standard":"nearpay_vault","version":"1.0.0","event":"canceled","data":[{"mandate_id":1,"by":"alice.test","refunded":"7"}]}"#
    );
    assert_eq!(
        events::format("withdrawn", events::Withdrawn { mandate_id: 1, amount: U128(7) }),
        r#"EVENT_JSON:{"standard":"nearpay_vault","version":"1.0.0","event":"withdrawn","data":[{"mandate_id":1,"amount":"7"}]}"#
    );
    assert_eq!(
        events::format("exhausted", events::Exhausted { mandate_id: 1 }),
        r#"EVENT_JSON:{"standard":"nearpay_vault","version":"1.0.0","event":"exhausted","data":[{"mandate_id":1}]}"#
    );
}

// ============================================================== constructor

#[test]
fn new_stores_config() {
    let mut s = Sim::new();
    let c = s.view(|v| v.get_config());
    assert_eq!(c.allowed_tokens, vec![acc(USDC), acc(USDT)]);
    assert_eq!(c.fee_receiver, acc(TREASURY));
    assert_eq!(c.max_fee_bps, MAX_FEE_BPS);
    assert_eq!(c.intents_account, acc(INTENTS));
    assert_eq!(c.mandate_storage_cost, U128(MCOST));
    let t = s.view(|v| v.get_token_tvl(acc(USDT))).unwrap();
    assert_eq!((t.tvl, t.cap, t.mandate_cap), (U128(0), U128(CAP), U128(MANDATE_CAP)));
    assert_eq!(c.caps, default_config().tvl_caps);
    assert!(s.view(|v| v.get_token_tvl(acc(EVIL_TOKEN))).is_none());
}

#[test]
fn new_rejects_bad_config() {
    type Mutation = Box<dyn Fn(&mut Config)>;
    let bad: Vec<Mutation> = vec![
        Box::new(|c| c.allowed_tokens.clear()),
        Box::new(|c| c.allowed_tokens.push(acc(USDC))), // duplicate (and cap count mismatch)
        Box::new(|c| c.allowed_tokens = vec![acc(USDC), acc(USDC)]), // duplicate, counts match
        Box::new(|c| {
            c.tvl_caps.pop();
        }),
        Box::new(|c| c.tvl_caps[1].token = acc(EVIL_TOKEN)),
        Box::new(|c| c.tvl_caps[1].token = acc(USDC)), // two caps for one token
        Box::new(|c| c.max_fee_bps = 10_001),
        Box::new(|c| c.tvl_caps[0].mandate_cap = U128(CAP + 1)), // mandate cap above TVL cap
        Box::new(|c| c.fee_receiver = acc(VAULT)),
        Box::new(|c| c.intents_account = acc(VAULT)),
        Box::new(|c| {
            c.allowed_tokens[1] = acc(VAULT);
            c.tvl_caps[1].token = acc(VAULT);
        }),
    ];
    for (i, mutate) in bad.iter().enumerate() {
        let mut cfg = default_config();
        mutate(&mut cfg);
        let mut s = Sim::empty();
        assert_eq!(s.init(cfg), Err(ERR_BAD_CONFIG.to_string()), "case {i}");
    }
}

#[test]
fn new_is_private_and_once() {
    let mut s = Sim::empty();
    let e = s.init_as(ALICE, default_config()).unwrap_err();
    assert!(e.contains("is private"), "{e}");
    s.init(default_config()).unwrap();
    // Re-initialisation is refused by near-sdk's generated init guard (state exists); that
    // guard lives in the wasm entry point and is covered by the integration tests.
}

// ============================================================== NEP-145 storage

#[test]
fn storage_deposit_registers_and_tops_up() {
    let mut s = Sim::new();
    assert_eq!(s.view(|v| v.storage_balance_of(acc(ALICE))), None);
    let b = s.register(ALICE, REG + MCOST);
    assert_eq!(b, StorageBalance { total: U128(REG + MCOST), available: U128(MCOST) });
    let b = s.register(ALICE, MCOST);
    assert_eq!(b.total, U128(REG + 2 * MCOST));
    assert_eq!(s.view(|v| v.storage_balance_bounds()), StorageBalanceBounds { min: U128(REG), max: None });
}

#[test]
fn storage_deposit_below_minimum_fails() {
    let mut s = Sim::new();
    let o = s.call(ALICE, REG - 1, vec![], |v| v.storage_deposit(None, None));
    assert_eq!(o.err(), ERR_NO_STORAGE);
}

#[test]
fn storage_deposit_registration_only_refunds_excess() {
    let mut s = Sim::new();
    let o = s.call(BOB, REG + 5, vec![], |v| v.storage_deposit(Some(acc(ALICE)), Some(true)));
    assert_eq!(o.result.clone().unwrap(), StorageBalance { total: U128(REG), available: U128(0) });
    assert_eq!(acts(&o.receipts), vec![Act::Transfer { receiver: BOB.into(), amount: 5 }]);
    // Already registered + registration_only: everything refunded.
    let o = s.call(BOB, 7, vec![], |v| v.storage_deposit(Some(acc(ALICE)), Some(true)));
    assert_eq!(o.result.clone().unwrap().total, U128(REG));
    assert_eq!(acts(&o.receipts), vec![Act::Transfer { receiver: BOB.into(), amount: 7 }]);
}

#[test]
fn storage_withdraw_rules() {
    let mut s = Sim::new();
    let o = s.call(ALICE, 1, vec![], |v| v.storage_withdraw(None));
    assert_eq!(o.err(), ERR_NO_STORAGE);
    s.register(ALICE, REG + MCOST + 100);
    let o = s.call(ALICE, 0, vec![], |v| v.storage_withdraw(None));
    assert!(o.err().contains("yoctoNEAR"));
    // A mandate locks MCOST.
    s.ft_on_transfer(USDC, ALICE, AMOUNT, &create_msg(MERCHANT, "near_account", AMOUNT, 0).to_string())
        .unwrap();
    let o = s.call(ALICE, 1, vec![], |v| v.storage_withdraw(Some(U128(101))));
    assert_eq!(o.err(), ERR_INSUFFICIENT_BALANCE);
    let o = s.call(ALICE, 1, vec![], |v| v.storage_withdraw(None));
    assert_eq!(o.result.clone().unwrap(), StorageBalance { total: U128(REG + MCOST), available: U128(0) });
    assert_eq!(acts(&o.receipts), vec![Act::Transfer { receiver: ALICE.into(), amount: 100 }]);
}

#[test]
fn storage_unregister_rules() {
    let mut s = Sim::new();
    assert!(!s.call(ALICE, 1, vec![], |v| v.storage_unregister(None)).unwrap());
    let id = s.create_default();
    let o = s.call(ALICE, 1, vec![], |v| v.storage_unregister(Some(true)));
    assert_eq!(o.err(), ERR_OPEN_MANDATES);
    // Close the mandate: cancel + successful refund removes it and releases its storage.
    s.cancel(ALICE, id).unwrap();
    s.on_refunded(id, 3 * AMOUNT, Some(ALICE), ok_empty()).unwrap();
    assert!(s.mandate(id).is_none());
    let o = s.call(ALICE, 1, vec![], |v| v.storage_unregister(None));
    assert!(o.result.clone().unwrap());
    assert_eq!(acts(&o.receipts), vec![Act::Transfer { receiver: ALICE.into(), amount: REG + MCOST }]);
    assert_eq!(s.view(|v| v.storage_balance_of(acc(ALICE))), None);
}

#[test]
fn storage_cost_covers_worst_case_mandate() {
    // Longest account ids (64 chars) and ext_ref: bytes written must be covered by the locks.
    let long = |c: char| -> String { std::iter::repeat_n(c, 64).collect() };
    let payer = long('p');
    let merchant = long('m');
    let token = long('t');
    let mut cfg = default_config();
    cfg.allowed_tokens = vec![acc(&token)];
    cfg.tvl_caps = vec![TokenCap { token: acc(&token), cap: U128(CAP), mandate_cap: U128(MANDATE_CAP) }];
    let mut s = Sim::with_config(cfg);
    let byte_cost = env::storage_byte_cost().as_yoctonear();

    let p = acc(&payer);
    let reg_bytes = s
        .call(&payer, REG + MCOST, vec![], move |v| {
            let before = env::storage_usage();
            v.storage_deposit(Some(p), None);
            v.storage_accounts.flush();
            env::storage_usage() - before
        })
        .unwrap();
    assert!(u128::from(reg_bytes) * byte_cost <= REG, "registration uses {reg_bytes} bytes");

    let msg =
        with(create_msg(&merchant, "intents_account", AMOUNT, 100), "ext_ref", json!(long('x'))).to_string();
    let p = acc(&payer);
    let mandate_bytes = s
        .call(&token, 0, vec![], move |v| {
            let before = env::storage_usage();
            assert_eq!(v.ft_on_transfer(p, U128(AMOUNT), msg).0, 0);
            v.mandates.flush();
            v.storage_accounts.flush();
            v.payer_mandates.flush();
            v.tvl.flush();
            env::storage_usage() - before
        })
        .unwrap();
    assert!(
        u128::from(mandate_bytes) * byte_cost <= MCOST,
        "one mandate uses {mandate_bytes} bytes = {} yocto",
        u128::from(mandate_bytes) * byte_cost
    );
}

// ============================================================== ft_on_transfer: create

#[test]
fn create_mandate_happy_path() {
    let mut s = Sim::new();
    s.register(ALICE, REG + MCOST);
    let msg = with(
        create_msg(MERCHANT, "intents_account", AMOUNT, 100),
        "start_at",
        json!((T0 + DAY_NS).to_string()),
    );
    let o = s.ft_on_transfer(USDC, ALICE, 3 * AMOUNT, &msg.to_string());
    assert_eq!(o.result, Ok(0));
    assert_eq!(o.logs.len(), 1);
    assert_eq!(
        o.logs[0],
        format!(
            r#"EVENT_JSON:{{"standard":"nearpay_vault","version":"1.0.0","event":"mandate_created","data":[{{"mandate_id":0,"payer":"alice.test","merchant_receiver":"merchant.test","token":"usdc.test","amount_per_period":"10000000","period_secs":2592000,"start_at":"{}","max_periods":12,"fee_bps":100,"ext_ref":"sub_test","receiver_kind":"intents_account","balance":"30000000"}}]}}"#,
            T0 + DAY_NS
        )
    );
    let m = s.mandate(0).unwrap();
    assert_eq!(m.payer, acc(ALICE));
    assert_eq!(m.token, acc(USDC));
    assert_eq!(m.balance, U128(3 * AMOUNT));
    assert_eq!(m.status, Status::Active);
    assert_eq!(m.collected_periods, 0);
    assert!(!m.in_flight);
    assert_eq!(m.next_collect_at, Some(U64(T0 + DAY_NS)));
    assert_eq!(s.tvl(USDC), 3 * AMOUNT);
    assert_eq!(s.view(|v| v.storage_balance_of(acc(ALICE))).unwrap().available, U128(0));
    // ids are sequential
    s.register(BOB, REG + MCOST);
    assert_eq!(s.ft_on_transfer(USDT, BOB, AMOUNT, &msg.to_string()).events()[0]["data"][0]["mandate_id"], 1);
    assert_eq!(s.tvl(USDT), AMOUNT);
    assert_eq!(s.mandate(1).unwrap().token, acc(USDT));
}

#[test]
fn create_start_at_defaults_and_clamps_to_now() {
    let mut s = Sim::new();
    s.register(ALICE, REG + 3 * MCOST);
    let base = create_msg(MERCHANT, "near_account", AMOUNT, 0);
    for (msg, expect) in [
        (without(base.clone(), "start_at"), T0),
        (with(base.clone(), "start_at", Value::Null), T0),
        (with(base.clone(), "start_at", json!("5")), T0),
    ] {
        let o = s.ft_on_transfer(USDC, ALICE, AMOUNT, &msg.to_string());
        assert_eq!(o.events()[0]["data"][0]["start_at"], json!(expect.to_string()));
    }
}

#[test]
fn ft_on_transfer_from_unknown_token_panics() {
    let mut s = Sim::new();
    s.register(ALICE, REG + MCOST);
    let o = s.ft_on_transfer(
        EVIL_TOKEN,
        ALICE,
        AMOUNT,
        &create_msg(MERCHANT, "near_account", AMOUNT, 0).to_string(),
    );
    assert_eq!(o.err(), ERR_TOKEN_NOT_ALLOWED);
}

fn assert_rejected(s: &mut Sim, token: &str, sender: &str, amount: u128, msg: &str, code: &str) {
    let next_before = s.view(|v| v.next_id);
    let tvl_before = s.tvl(token);
    let o = s.ft_on_transfer(token, sender, amount, msg);
    assert_eq!(o.result, Ok(amount), "msg {msg}");
    assert_eq!(o.logs, vec![code.to_string()], "msg {msg}");
    assert!(o.receipts.is_empty());
    assert_eq!(s.view(|v| v.next_id), next_before);
    assert_eq!(s.tvl(token), tvl_before);
}

#[test]
fn create_rejects_bad_msg_with_full_refund() {
    let mut s = Sim::new();
    s.register(ALICE, REG + MCOST);
    let base = create_msg(MERCHANT, "intents_account", AMOUNT, 100);
    let long_ref: String = std::iter::repeat_n('x', 65).collect();
    let cases: Vec<String> = vec![
        "".into(),
        "not json".into(),
        "{}".into(),
        "null".into(),
        r#"{"create_mandate":{}}"#.into(),
        r#"{"unknown":{}}"#.into(),
        r#""create_mandate""#.into(),
        json!({"create_mandate": base["create_mandate"], "top_up": {"mandate_id": 0}}).to_string(),
        with(base.clone(), "fee_receiver", json!("evil.test")).to_string(),
        with(base.clone(), "token", json!(USDC)).to_string(),
        with(base.clone(), "payer", json!(BOB)).to_string(),
        without(base.clone(), "merchant_receiver").to_string(),
        without(base.clone(), "receiver_kind").to_string(),
        without(base.clone(), "amount_per_period").to_string(),
        without(base.clone(), "period_secs").to_string(),
        without(base.clone(), "fee_bps").to_string(),
        without(base.clone(), "ext_ref").to_string(),
        with(base.clone(), "receiver_kind", json!("IntentsAccount")).to_string(),
        with(base.clone(), "receiver_kind", json!("chain_address")).to_string(),
        with(base.clone(), "merchant_receiver", json!("Not A Valid Id")).to_string(),
        with(base.clone(), "amount_per_period", json!(10_000_000)).to_string(), // number, not string
        with(base.clone(), "amount_per_period", json!("-1")).to_string(),
        with(base.clone(), "amount_per_period", json!("340282366920938463463374607431768211456")).to_string(),
        with(base.clone(), "amount_per_period", json!("0")).to_string(),
        with(base.clone(), "period_secs", json!(86_399)).to_string(),
        with(base.clone(), "period_secs", json!(-1)).to_string(),
        with(base.clone(), "period_secs", json!("2592000")).to_string(),
        with(base.clone(), "start_at", json!(T0)).to_string(), // number, not string
        with(base.clone(), "max_periods", json!(0)).to_string(),
        with(base.clone(), "max_periods", json!(-1)).to_string(),
        with(base.clone(), "fee_bps", json!(70_000)).to_string(),
        with(base.clone(), "ext_ref", json!(long_ref)).to_string(),
        with(base.clone(), "ext_ref", json!("tab\there")).to_string(),
        with(base.clone(), "ext_ref", json!("ünïcode")).to_string(),
        with(base.clone(), "merchant_receiver", json!(VAULT)).to_string(),
        // share rounds to zero: 1 base unit with any fee
        with(with(base.clone(), "amount_per_period", json!("1")), "fee_bps", json!(1)).to_string(),
        format!("{} ", base) + &" ".repeat(msg::MAX_MSG_LEN), // too long
    ];
    for c in cases {
        assert_rejected(&mut s, USDC, ALICE, AMOUNT, &c, ERR_BAD_MSG);
    }
}

#[test]
fn create_boundaries_accepted() {
    let mut s = Sim::new();
    s.register(ALICE, REG + 4 * MCOST);
    let base = create_msg(MERCHANT, "near_account", AMOUNT, 0);
    let ref64: String = std::iter::repeat_n('~', 64).collect();
    for msg in [
        with(base.clone(), "period_secs", json!(86_400)),
        with(base.clone(), "fee_bps", json!(MAX_FEE_BPS)),
        with(with(base.clone(), "ext_ref", json!(ref64)), "max_periods", Value::Null),
        with(base.clone(), "ext_ref", json!("")),
    ] {
        assert_eq!(s.ft_on_transfer(USDC, ALICE, AMOUNT, &msg.to_string()).result, Ok(0), "{msg}");
    }
}

#[test]
fn create_fee_above_max_is_rejected() {
    let mut s = Sim::new();
    s.register(ALICE, REG + MCOST);
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, MAX_FEE_BPS + 1).to_string();
    assert_rejected(&mut s, USDC, ALICE, AMOUNT, &msg, ERR_FEE_TOO_HIGH);
}

#[test]
fn create_without_storage_is_refunded() {
    let mut s = Sim::new();
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string();
    assert_rejected(&mut s, USDC, ALICE, AMOUNT, &msg, ERR_NO_STORAGE); // unregistered
    s.register(ALICE, REG); // registered, nothing available
    assert_rejected(&mut s, USDC, ALICE, AMOUNT, &msg, ERR_NO_STORAGE);
    s.register(ALICE, MCOST - 1);
    assert_rejected(&mut s, USDC, ALICE, AMOUNT, &msg, ERR_NO_STORAGE);
    s.register(ALICE, 1);
    assert_eq!(s.ft_on_transfer(USDC, ALICE, AMOUNT, &msg).result, Ok(0));
    // The next mandate needs another MCOST.
    assert_rejected(&mut s, USDC, ALICE, AMOUNT, &msg, ERR_NO_STORAGE);
    // Storage of a different account does not count.
    s.register(BOB, REG + MCOST);
    assert_rejected(&mut s, USDC, ALICE, AMOUNT, &msg, ERR_NO_STORAGE);
}

#[test]
fn create_over_tvl_cap_is_refunded() {
    let mut s = Sim::new();
    s.register(ALICE, REG + 12 * MCOST);
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string();
    // Fill the TVL cap with full mandates (each at the per-mandate cap).
    for _ in 0..(CAP / MANDATE_CAP - 1) {
        assert_eq!(s.ft_on_transfer(USDC, ALICE, MANDATE_CAP, &msg).result, Ok(0));
    }
    assert_eq!(s.ft_on_transfer(USDC, ALICE, MANDATE_CAP - 5, &msg).result, Ok(0));
    assert_rejected(&mut s, USDC, ALICE, 6, &msg, LOG_CAP_EXCEEDED_TVL);
    assert_eq!(s.ft_on_transfer(USDC, ALICE, 5, &msg).result, Ok(0)); // exactly the TVL cap
    assert_eq!(s.tvl(USDC), CAP);
    assert_rejected(&mut s, USDC, ALICE, 1, &msg, LOG_CAP_EXCEEDED_TVL);
    // Caps are per token.
    assert_eq!(s.ft_on_transfer(USDT, ALICE, AMOUNT, &msg).result, Ok(0));
}

#[test]
fn create_over_mandate_cap_is_refunded() {
    let mut s = Sim::new();
    s.register(ALICE, REG + 2 * MCOST);
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string();
    assert_rejected(&mut s, USDC, ALICE, MANDATE_CAP + 1, &msg, LOG_CAP_EXCEEDED_MANDATE);
    assert_rejected(&mut s, USDC, ALICE, u128::MAX, &msg, LOG_CAP_EXCEEDED_MANDATE);
    // amount_per_period above the mandate cap is rejected even if the funding fits.
    let big = create_msg(MERCHANT, "intents_account", MANDATE_CAP + 1, 100).to_string();
    assert_rejected(&mut s, USDC, ALICE, AMOUNT, &big, LOG_CAP_EXCEEDED_MANDATE);
    // Exactly the cap is fine, for both.
    let at_cap = create_msg(MERCHANT, "intents_account", MANDATE_CAP, 100).to_string();
    assert_eq!(s.ft_on_transfer(USDC, ALICE, MANDATE_CAP, &at_cap).result, Ok(0));
    assert_eq!(s.mandate(0).unwrap().balance, U128(MANDATE_CAP));
    // The stable code is the text before ':'.
    assert_eq!(errors::code_of(LOG_CAP_EXCEEDED_MANDATE), ERR_CAP_EXCEEDED);
    assert_eq!(errors::code_of(LOG_CAP_EXCEEDED_TVL), ERR_CAP_EXCEEDED);
    assert_eq!(errors::code_of(ERR_BAD_MSG), ERR_BAD_MSG);
}

#[test]
fn top_up_respects_mandate_cap_including_in_flight() {
    let mut s = Sim::new();
    let id = s.create(ALICE, MANDATE_CAP - AMOUNT, &create_msg(MERCHANT, "intents_account", AMOUNT, 100));
    let top = json!({"top_up": {"mandate_id": id}}).to_string();
    assert_rejected(&mut s, USDC, ALICE, AMOUNT + 1, &top, LOG_CAP_EXCEEDED_MANDATE);
    // While a collect is in flight its amount still counts (it may be credited back).
    s.collect(id).unwrap();
    assert_rejected(&mut s, USDC, ALICE, AMOUNT + 1, &top, LOG_CAP_EXCEEDED_MANDATE);
    assert_eq!(s.ft_on_transfer(USDC, ALICE, AMOUNT, &top).result, Ok(0)); // exactly the cap
    let m = s.mandate(id).unwrap();
    assert_eq!(m.balance.0 + m.amount_per_period.0, MANDATE_CAP);
    // A failed merchant leg credits back: still exactly at the cap, never above.
    let (share, fee) = share_fee(AMOUNT, 100);
    s.on_merchant_leg(id, share, fee, failed()).unwrap();
    assert_eq!(s.mandate(id).unwrap().balance, U128(MANDATE_CAP));
    assert_rejected(&mut s, USDC, ALICE, 1, &top, LOG_CAP_EXCEEDED_MANDATE);
}

#[test]
fn zero_amount_transfer_is_rejected() {
    let mut s = Sim::new();
    s.register(ALICE, REG + MCOST);
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string();
    assert_rejected(&mut s, USDC, ALICE, 0, &msg, ERR_BAD_MSG);
}

// ============================================================== ft_on_transfer: top_up

#[test]
fn top_up_rules() {
    let mut s = Sim::new();
    let id = s.create_default();
    let top = json!({"top_up": {"mandate_id": id}}).to_string();
    let o = s.ft_on_transfer(USDC, ALICE, 5, &top);
    assert_eq!(o.result, Ok(0));
    assert_eq!(
        o.logs,
        vec![r#"EVENT_JSON:{"standard":"nearpay_vault","version":"1.0.0","event":"topped_up","data":[{"mandate_id":0,"amount":"5","balance":"30000005"}]}"#.to_string()]
    );
    assert_eq!(s.mandate(id).unwrap().balance, U128(3 * AMOUNT + 5));
    assert_eq!(s.tvl(USDC), 3 * AMOUNT + 5);

    assert_rejected(
        &mut s,
        USDC,
        ALICE,
        5,
        &json!({"top_up": {"mandate_id": 99}}).to_string(),
        ERR_NOT_FOUND,
    );
    assert_rejected(&mut s, USDC, BOB, 5, &top, ERR_UNAUTHORIZED);
    assert_rejected(&mut s, USDC, MERCHANT, 5, &top, ERR_UNAUTHORIZED);
    assert_rejected(&mut s, USDT, ALICE, 5, &top, ERR_TOKEN_NOT_ALLOWED);
    assert_rejected(&mut s, USDC, ALICE, MANDATE_CAP, &top, LOG_CAP_EXCEEDED_MANDATE);
    assert_rejected(&mut s, USDC, ALICE, 5, r#"{"top_up":{"mandate_id":0,"x":1}}"#, ERR_BAD_MSG);
    assert_rejected(&mut s, USDC, ALICE, 5, r#"{"top_up":{"mandate_id":"0"}}"#, ERR_BAD_MSG);
    s.cancel(ALICE, id).unwrap();
    assert_rejected(&mut s, USDC, ALICE, 5, &top, ERR_NOT_ACTIVE);
}

// ============================================================== collect

#[test]
fn collect_preconditions() {
    let mut s = Sim::new();
    assert_eq!(s.collect(7).err(), ERR_NOT_FOUND);

    // Not due before start_at.
    s.register(ALICE, REG + 2 * MCOST);
    let msg = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "start_at", json!((T0 + 10).to_string()));
    s.ft_on_transfer(USDC, ALICE, AMOUNT, &msg.to_string()).unwrap();
    assert_eq!(s.collect(0).err(), ERR_NOT_DUE);
    s.now = T0 + 10; // due exactly at start_at
    s.collect(0).unwrap();
    // In flight.
    assert_eq!(s.collect(0).err(), ERR_IN_FLIGHT);

    // Insufficient balance (funded below one period).
    s.ft_on_transfer(USDC, ALICE, AMOUNT - 1, &create_msg(MERCHANT, "near_account", AMOUNT, 0).to_string())
        .unwrap();
    assert_eq!(s.collect(1).err(), ERR_INSUFFICIENT_BALANCE);
    // Not due wins over insufficient balance when both apply.
    s.now = T0;
    s.register(ALICE, MCOST);
    let msg = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "start_at", json!((T0 + 99).to_string()));
    s.ft_on_transfer(USDC, ALICE, 1, &msg.to_string()).unwrap();
    assert_eq!(s.collect(2).err(), ERR_NOT_DUE);
}

#[test]
fn collect_intents_receipts() {
    let mut s = Sim::new();
    let id = s.create_default();
    let (share, fee) = share_fee(AMOUNT, 100);
    let o = s.collect(id);
    assert!(o.logs.is_empty());
    let a = acts(&o.receipts);
    assert_eq!(a.len(), 2, "{a:?}");
    match &a[0] {
        Act::Call { receiver, method, args, deposit, gas, weight } => {
            assert_eq!(receiver, USDC);
            assert_eq!(method, "ft_transfer_call");
            assert_eq!(
                args,
                &json!({"receiver_id": INTENTS, "amount": share.to_string(), "memo": null, "msg": MERCHANT})
            );
            assert_eq!(*deposit, 1);
            assert_eq!(*gas, GAS_FT_TRANSFER_CALL);
            assert_eq!(*weight, 1);
        }
        other => panic!("{other:?}"),
    }
    match &a[1] {
        Act::Call { receiver, method, args, deposit, gas, .. } => {
            assert_eq!(receiver, VAULT);
            assert_eq!(method, "on_merchant_leg");
            assert_eq!(args, &json!({"mandate_id": id, "share": share.to_string(), "fee": fee.to_string()}));
            assert_eq!(*deposit, 0);
            assert_eq!(*gas, GAS_ON_MERCHANT_LEG);
        }
        other => panic!("{other:?}"),
    }
    let m = s.mandate(id).unwrap();
    assert!(m.in_flight);
    assert_eq!(m.balance, U128(2 * AMOUNT));
    assert_eq!(s.tvl(USDC), 3 * AMOUNT); // still held while in flight
}

#[test]
fn collect_near_account_receipts() {
    let mut s = Sim::new();
    let id = s.create(ALICE, AMOUNT, &create_msg(MERCHANT, "near_account", AMOUNT, 250));
    let (share, _) = share_fee(AMOUNT, 250);
    let a = acts(&s.collect(id).receipts);
    assert_eq!(
        a[0],
        Act::Call {
            receiver: USDC.into(),
            method: "ft_transfer".into(),
            args: json!({"receiver_id": MERCHANT, "amount": share.to_string(), "memo": null}),
            deposit: 1,
            gas: GAS_FT_TRANSFER,
            weight: 1,
        }
    );
}

#[test]
fn collect_success_with_fee() {
    let mut s = Sim::new();
    let id = s.create_default();
    let (share, fee) = share_fee(AMOUNT, 100);
    s.collect(id).unwrap();
    let o = s.on_merchant_leg(id, share, fee, ok_u128(share));
    assert!(o.logs.is_empty());
    let a = acts(&o.receipts);
    assert_eq!(
        a[0],
        Act::Call {
            receiver: USDC.into(),
            method: "ft_transfer".into(),
            args: json!({"receiver_id": TREASURY, "amount": fee.to_string(), "memo": null}),
            deposit: 1,
            gas: GAS_FT_TRANSFER,
            weight: 1,
        }
    );
    assert!(matches!(&a[1], Act::Call { receiver, method, gas, .. }
        if receiver == VAULT && method == "on_collected" && *gas == GAS_ON_COLLECTED));
    assert_eq!(o.unwrap(), None);
    let o = s.on_collected(id, share, fee, ok_empty());
    assert_eq!(
        o.logs,
        vec![format!(
            r#"EVENT_JSON:{{"standard":"nearpay_vault","version":"1.0.0","event":"collected","data":[{{"mandate_id":0,"period":1,"amount":"{AMOUNT}","merchant_share":"{share}","fee":"{fee}","balance":"{}"}}]}}"#,
            2 * AMOUNT
        )]
    );
    assert!(o.unwrap());
    let m = s.mandate(id).unwrap();
    assert_eq!((m.collected_periods, m.balance, m.in_flight), (1, U128(2 * AMOUNT), false));
    assert_eq!(m.next_collect_at, Some(U64(T0 + 30 * DAY_NS)));
    assert_eq!(s.tvl(USDC), 2 * AMOUNT);
    // Period 2 is not due until start + 1 period.
    assert_eq!(s.collect(id).err(), ERR_NOT_DUE);
    s.now = T0 + 30 * DAY_NS - 1;
    assert_eq!(s.collect(id).err(), ERR_NOT_DUE);
    s.now = T0 + 30 * DAY_NS;
    let evs = s.collect_ok(id);
    assert_eq!(evs[0]["data"][0]["period"], 2);
}

#[test]
fn collect_zero_fee_skips_fee_leg() {
    let mut s = Sim::new();
    let id = s.create(ALICE, AMOUNT, &create_msg(MERCHANT, "near_account", AMOUNT, 0));
    s.collect(id).unwrap();
    let o = s.on_merchant_leg(id, AMOUNT, 0, ok_empty());
    assert!(o.receipts.is_empty());
    assert_eq!(event_names(&o.events()), vec!["collected"]);
    assert_eq!(o.events()[0]["data"][0]["fee"], "0");
    assert_eq!(o.unwrap(), Some(true));
}

#[test]
fn collect_merchant_leg_failure_restores_balance() {
    for (kind, result) in [
        ("intents_account", failed()),
        ("intents_account", ok_u128(0)), // receiver refunded everything
        ("near_account", failed()),
    ] {
        let mut s = Sim::new();
        let id = s.create(ALICE, 3 * AMOUNT, &create_msg(MERCHANT, kind, AMOUNT, 100));
        let (share, fee) = share_fee(AMOUNT, 100);
        s.collect(id).unwrap();
        let o = s.on_merchant_leg(id, share, fee, result);
        assert!(o.receipts.is_empty(), "fee must not be sent when the merchant leg fails");
        assert_eq!(
            o.logs,
            vec![r#"EVENT_JSON:{"standard":"nearpay_vault","version":"1.0.0","event":"collect_failed","data":[{"mandate_id":0,"period":1,"reason":"merchant_transfer_failed"}]}"#.to_string()]
        );
        assert_eq!(o.unwrap(), Some(false));
        let m = s.mandate(id).unwrap();
        assert_eq!((m.collected_periods, m.balance, m.in_flight), (0, U128(3 * AMOUNT), false));
        assert_eq!(s.tvl(USDC), 3 * AMOUNT);
        // Retry is possible immediately.
        s.collect(id).unwrap();
    }
}

#[test]
fn collect_partial_delivery_counts_period_without_fee() {
    let mut s = Sim::new();
    let id = s.create_default();
    let (share, fee) = share_fee(AMOUNT, 100);
    s.collect(id).unwrap();
    let o = s.on_merchant_leg(id, share, fee, ok_u128(share - 7));
    assert!(o.receipts.is_empty());
    let e = &o.events()[0];
    assert_eq!(e["event"], "collected");
    assert_eq!(e["data"][0]["merchant_share"], (share - 7).to_string());
    assert_eq!(e["data"][0]["fee"], "0");
    assert_eq!(e["data"][0]["amount"], (share - 7).to_string());
    let m = s.mandate(id).unwrap();
    assert_eq!(m.collected_periods, 1);
    assert_eq!(m.balance, U128(3 * AMOUNT - (share - 7)));
    assert_eq!(s.tvl(USDC), 3 * AMOUNT - (share - 7));
}

#[test]
fn collect_over_reported_or_garbage_result_counts_as_full_share() {
    for r in [
        ok_u128(u128::MAX),
        PromiseResult::Successful(b"not json".to_vec()),
        PromiseResult::Successful(vec![b'1'; 200]), // over-long
    ] {
        let mut s = Sim::new();
        let id = s.create_default();
        let (share, fee) = share_fee(AMOUNT, 100);
        s.collect(id).unwrap();
        // Full share assumed delivered → fee leg is sent.
        assert_eq!(s.on_merchant_leg(id, share, fee, r).unwrap(), None);
        let o = s.on_collected(id, share, fee, ok_empty());
        assert_eq!(o.events()[0]["data"][0]["merchant_share"], share.to_string());
        assert_eq!(s.tvl(USDC), 2 * AMOUNT);
    }
}

#[test]
fn collect_fee_leg_failure_keeps_fee_with_payer() {
    let mut s = Sim::new();
    let id = s.create_default();
    let (share, fee) = share_fee(AMOUNT, 100);
    s.collect(id).unwrap();
    s.on_merchant_leg(id, share, fee, ok_u128(share)).unwrap();
    let o = s.on_collected(id, share, fee, failed());
    let e = &o.events()[0];
    assert_eq!(e["event"], "collected");
    assert_eq!(e["data"][0]["amount"], share.to_string());
    assert_eq!(e["data"][0]["fee"], "0");
    assert!(o.unwrap());
    let m = s.mandate(id).unwrap();
    assert_eq!(m.collected_periods, 1);
    assert_eq!(m.balance, U128(2 * AMOUNT + fee));
    assert_eq!(s.tvl(USDC), 2 * AMOUNT + fee);
}

#[test]
fn collect_until_exhausted() {
    let mut s = Sim::new();
    let msg = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "max_periods", json!(2));
    let id = s.create(ALICE, 2 * AMOUNT + 3, &msg);
    s.collect_ok(id);
    s.now += 30 * DAY_NS;
    let evs = s.collect_ok(id);
    assert_eq!(event_names(&evs), vec!["collected", "exhausted"]);
    assert_eq!(evs[1]["data"][0], json!({"mandate_id": id}));
    let m = s.mandate(id).unwrap();
    assert_eq!((m.status, m.balance, m.next_collect_at), (Status::Exhausted, U128(3), None));
    s.now += 30 * DAY_NS;
    assert_eq!(s.collect(id).err(), ERR_MAX_PERIODS);
    // Leftover goes back to the payer via withdraw; the mandate is then removed.
    s.withdraw(ALICE, id).unwrap();
    let o = s.on_refunded(id, 3, None, ok_empty());
    assert_eq!(event_names(&o.events()), vec!["withdrawn"]);
    assert!(s.mandate(id).is_none());
    assert_eq!(s.collect(id).err(), ERR_NOT_FOUND);
    assert_eq!(s.view(|v| v.storage_balance_of(acc(ALICE))).unwrap().available, U128(MCOST));
}

#[test]
fn exactly_funded_mandate_is_removed_when_exhausted() {
    let mut s = Sim::new();
    let msg = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "max_periods", json!(1));
    let id = s.create(ALICE, AMOUNT, &msg);
    let evs = s.collect_ok(id);
    assert_eq!(event_names(&evs), vec!["collected", "exhausted"]);
    assert!(s.mandate(id).is_none());
    assert_eq!(s.tvl(USDC), 0);
    assert_eq!(s.view(|v| v.storage_balance_of(acc(ALICE))).unwrap().available, U128(MCOST));
}

#[test]
fn unlimited_mandate_collects_while_funded() {
    let mut s = Sim::new();
    let msg = with(create_msg(MERCHANT, "intents_account", AMOUNT, 100), "max_periods", Value::Null);
    let id = s.create(ALICE, 2 * AMOUNT, &msg);
    s.collect_ok(id);
    s.now += 30 * DAY_NS;
    s.collect_ok(id);
    s.now += 30 * DAY_NS;
    assert_eq!(s.collect(id).err(), ERR_INSUFFICIENT_BALANCE);
    let m = s.mandate(id).unwrap();
    assert_eq!((m.status, m.collected_periods, m.balance), (Status::Active, 2, U128(0)));
}

#[test]
fn late_collects_catch_up_one_period_per_call() {
    let mut s = Sim::new();
    let id = s.create(ALICE, 5 * AMOUNT, &create_msg(MERCHANT, "near_account", AMOUNT, 0));
    s.now = T0 + 3 * 30 * DAY_NS; // 4 periods have started
    for p in 1..=4 {
        assert_eq!(s.collect_ok(id)[0]["data"][0]["period"], p);
    }
    assert_eq!(s.collect(id).err(), ERR_NOT_DUE);
}

#[test]
fn callbacks_are_private() {
    let mut s = Sim::new();
    let id = s.create_default();
    s.collect(id).unwrap();
    for e in [
        s.call(MALLORY, 0, vec![ok_u128(1)], |v| {
            let _ = v.on_merchant_leg(id, U128(1), U128(0));
        })
        .result
        .unwrap_err(),
        s.call(MALLORY, 0, vec![ok_empty()], |v| {
            v.on_collected(id, U128(1), U128(0));
        })
        .result
        .unwrap_err(),
        s.call(MALLORY, 0, vec![failed()], |v| {
            v.on_refunded(id, U128(1), None);
        })
        .result
        .unwrap_err(),
    ] {
        assert!(e.contains("is private"), "{e}");
    }
    assert!(s.mandate(id).unwrap().in_flight);
}

#[test]
fn stray_callbacks_change_nothing() {
    // A callback for a mandate that is not in flight (cannot happen on-chain) is a no-op.
    let mut s = Sim::new();
    let id = s.create_default();
    let before = s.mandate(id);
    assert_eq!(s.on_merchant_leg(id, 1, 0, ok_u128(1)).unwrap(), Some(false));
    assert!(!s.on_collected(id, 1, 0, ok_empty()).unwrap());
    assert_eq!(s.on_merchant_leg(99, 1, 0, ok_u128(1)).unwrap(), Some(false));
    assert!(!s.on_refunded(99, 1, None, ok_empty()).unwrap());
    assert_eq!(s.mandate(id), before);
    assert_eq!(s.tvl(USDC), 3 * AMOUNT);
}

#[test]
fn collect_forwards_at_most_one_yocto() {
    // Token calls carry exactly the 1 yoctoNEAR NEP-141 requires; callbacks carry none.
    let mut s = Sim::new();
    let id = s.create_default();
    for act in acts(&s.collect(id).receipts) {
        if let Act::Call { deposit, method, .. } = act {
            assert!(deposit <= 1, "{method} carries {deposit}");
        }
    }
}

// ============================================================== cancel

#[test]
fn cancel_by_payer_refunds_and_closes() {
    let mut s = Sim::new();
    let id = s.create_default();
    let o = s.cancel(ALICE, id);
    assert!(o.logs.is_empty(), "event waits for the refund outcome");
    let a = acts(&o.receipts);
    assert_eq!(
        a[0],
        Act::Call {
            receiver: USDC.into(),
            method: "ft_transfer".into(),
            args: json!({"receiver_id": ALICE, "amount": (3 * AMOUNT).to_string(), "memo": null}),
            deposit: 1,
            gas: GAS_FT_TRANSFER,
            weight: 1,
        }
    );
    assert!(matches!(&a[1], Act::Call { receiver, method, args, gas, .. }
        if receiver == VAULT && method == "on_refunded" && *gas == GAS_ON_REFUNDED
        && args == &json!({"mandate_id": id, "amount": (3 * AMOUNT).to_string(), "canceled_by": ALICE})));
    let m = s.mandate(id).unwrap();
    assert_eq!((m.status, m.balance, m.refund_in_flight), (Status::Canceled, U128(0), U128(3 * AMOUNT)));
    assert_eq!(s.tvl(USDC), 3 * AMOUNT);
    let o = s.on_refunded(id, 3 * AMOUNT, Some(ALICE), ok_empty());
    assert_eq!(
        o.logs,
        vec![format!(
            r#"EVENT_JSON:{{"standard":"nearpay_vault","version":"1.0.0","event":"canceled","data":[{{"mandate_id":0,"by":"alice.test","refunded":"{}"}}]}}"#,
            3 * AMOUNT
        )]
    );
    assert!(o.unwrap());
    assert!(s.mandate(id).is_none());
    assert_eq!(s.tvl(USDC), 0);
    assert!(s.view(|v| v.get_mandates_by_payer(acc(ALICE), None, None)).is_empty());
}

#[test]
fn cancel_by_merchant_refunds_payer() {
    let mut s = Sim::new();
    let id = s.create_default();
    let o = s.cancel(MERCHANT, id);
    assert!(matches!(&acts(&o.receipts)[0], Act::Call { args, .. } if args["receiver_id"] == ALICE));
    let o = s.on_refunded(id, 3 * AMOUNT, Some(MERCHANT), ok_empty());
    assert_eq!(o.events()[0]["data"][0]["by"], MERCHANT);
}

#[test]
fn cancel_errors() {
    let mut s = Sim::new();
    assert_eq!(s.cancel(ALICE, 3).err(), ERR_NOT_FOUND);
    let id = s.create_default();
    assert_eq!(s.cancel(MALLORY, id).err(), ERR_UNAUTHORIZED);
    assert_eq!(s.cancel(TREASURY, id).err(), ERR_UNAUTHORIZED);
    assert_eq!(s.cancel(INTENTS, id).err(), ERR_UNAUTHORIZED);
    let o = s.call(ALICE, 0, vec![], |v| {
        let _ = v.cancel(0);
    });
    assert!(o.err().contains("yoctoNEAR"));
    let o = s.call(ALICE, 2, vec![], |v| {
        let _ = v.cancel(0);
    });
    assert!(o.err().contains("yoctoNEAR"));
    s.cancel(ALICE, id).unwrap();
    assert_eq!(s.cancel(ALICE, id).err(), ERR_NOT_ACTIVE);
    assert_eq!(s.collect(id).err(), ERR_NOT_ACTIVE);
}

#[test]
fn cancel_with_zero_balance_closes_immediately() {
    let mut s = Sim::new();
    let msg = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "max_periods", Value::Null);
    let id = s.create(ALICE, AMOUNT, &msg);
    s.collect_ok(id);
    assert_eq!(s.mandate(id).unwrap().balance, U128(0));
    let o = s.cancel(MERCHANT, id);
    assert!(o.receipts.is_empty());
    assert_eq!(o.events()[0]["data"][0], json!({"mandate_id": id, "by": MERCHANT, "refunded": "0"}));
    assert_eq!(o.unwrap(), Some(true));
    assert!(s.mandate(id).is_none());
}

#[test]
fn cancel_refund_failure_leaves_balance_for_withdraw() {
    let mut s = Sim::new();
    let id = s.create_default();
    s.cancel(ALICE, id).unwrap();
    let o = s.on_refunded(id, 3 * AMOUNT, Some(ALICE), failed());
    assert_eq!(o.events()[0]["data"][0], json!({"mandate_id": id, "by": ALICE, "refunded": "0"}));
    assert!(!o.unwrap());
    let m = s.mandate(id).unwrap();
    assert_eq!((m.status, m.balance, m.refund_in_flight), (Status::Canceled, U128(3 * AMOUNT), U128(0)));
    assert_eq!(s.tvl(USDC), 3 * AMOUNT);
    s.withdraw(ALICE, id).unwrap();
    let o = s.on_refunded(id, 3 * AMOUNT, None, ok_empty());
    assert_eq!(
        o.logs,
        vec![format!(
            r#"EVENT_JSON:{{"standard":"nearpay_vault","version":"1.0.0","event":"withdrawn","data":[{{"mandate_id":0,"amount":"{}"}}]}}"#,
            3 * AMOUNT
        )]
    );
    assert!(s.mandate(id).is_none());
    assert_eq!(s.tvl(USDC), 0);
}

#[test]
fn cancel_during_collect_never_refunds_in_flight_amount() {
    for merchant_ok in [true, false] {
        let mut s = Sim::new();
        let id = s.create_default();
        let (share, fee) = share_fee(AMOUNT, 100);
        s.collect(id).unwrap();
        // Refund covers only the spendable balance.
        let o = s.cancel(ALICE, id);
        assert!(
            matches!(&acts(&o.receipts)[0], Act::Call { args, .. } if args["amount"] == (2 * AMOUNT).to_string())
        );
        s.on_refunded(id, 2 * AMOUNT, Some(ALICE), ok_empty()).unwrap();
        // Mandate stays (collect still in flight).
        assert!(s.mandate(id).unwrap().in_flight);
        assert_eq!(s.withdraw(ALICE, id).err(), ERR_INSUFFICIENT_BALANCE);
        if merchant_ok {
            s.on_merchant_leg(id, share, fee, ok_u128(share)).unwrap();
            let o = s.on_collected(id, share, fee, ok_empty());
            assert_eq!(event_names(&o.events()), vec!["collected"]);
            // Closed: balance 0, nothing in flight.
            assert!(s.mandate(id).is_none());
        } else {
            let o = s.on_merchant_leg(id, share, fee, failed());
            assert_eq!(event_names(&o.events()), vec!["collect_failed"]);
            let m = s.mandate(id).unwrap();
            assert_eq!((m.status, m.balance), (Status::Canceled, U128(AMOUNT)));
            s.withdraw(ALICE, id).unwrap();
            s.on_refunded(id, AMOUNT, None, ok_empty()).unwrap();
            assert!(s.mandate(id).is_none());
        }
        assert_eq!(s.tvl(USDC), 0);
    }
}

#[test]
fn canceled_mandate_is_not_exhausted_by_late_final_collect() {
    let mut s = Sim::new();
    let msg = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "max_periods", json!(1));
    let id = s.create(ALICE, 2 * AMOUNT, &msg);
    s.collect(id).unwrap();
    s.cancel(ALICE, id).unwrap();
    s.on_refunded(id, AMOUNT, Some(ALICE), ok_empty()).unwrap();
    let o = s.on_merchant_leg(id, AMOUNT, 0, ok_empty());
    assert_eq!(event_names(&o.events()), vec!["collected"]); // no `exhausted`: already canceled
    assert!(s.mandate(id).is_none());
}

// ============================================================== withdraw

#[test]
fn withdraw_errors() {
    let mut s = Sim::new();
    assert_eq!(s.withdraw(ALICE, 0).err(), ERR_NOT_FOUND);
    let id = s.create_default();
    assert_eq!(s.withdraw(MERCHANT, id).err(), ERR_UNAUTHORIZED);
    assert_eq!(s.withdraw(ALICE, id).err(), ERR_STILL_ACTIVE);
    s.cancel(MERCHANT, id).unwrap();
    assert_eq!(s.withdraw(MERCHANT, id).err(), ERR_UNAUTHORIZED);
    assert_eq!(s.withdraw(ALICE, id).err(), ERR_IN_FLIGHT); // cancel refund pending
    let o = s.call(ALICE, 0, vec![], |v| {
        let _ = v.withdraw(0);
    });
    assert!(o.err().contains("yoctoNEAR"));
    s.on_refunded(id, 3 * AMOUNT, Some(MERCHANT), failed()).unwrap();
    s.withdraw(ALICE, id).unwrap();
    assert_eq!(s.withdraw(ALICE, id).err(), ERR_IN_FLIGHT);
}

#[test]
fn withdraw_failure_restores_balance_silently() {
    let mut s = Sim::new();
    let id = s.create_default();
    s.cancel(ALICE, id).unwrap();
    s.on_refunded(id, 3 * AMOUNT, Some(ALICE), failed()).unwrap();
    let o = s.withdraw(ALICE, id);
    assert!(matches!(&acts(&o.receipts)[0], Act::Call { method, args, .. }
        if method == "ft_transfer" && args["receiver_id"] == ALICE && args["amount"] == (3 * AMOUNT).to_string()));
    let o = s.on_refunded(id, 3 * AMOUNT, None, failed());
    assert!(o.logs.is_empty());
    assert_eq!(s.mandate(id).unwrap().balance, U128(3 * AMOUNT));
    assert_eq!(s.tvl(USDC), 3 * AMOUNT);
}

// ============================================================== views

#[test]
fn views_by_payer_and_next_collect() {
    let mut s = Sim::new();
    for _ in 0..5 {
        s.create_default();
    }
    s.create(BOB, AMOUNT, &create_msg(MERCHANT, "near_account", AMOUNT, 0));
    let all = s.view(|v| v.get_mandates_by_payer(acc(ALICE), None, None));
    assert_eq!(all.len(), 5);
    let mut ids: Vec<u64> = all.iter().map(|m| m.mandate_id).collect();
    ids.sort();
    assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    let page = s.view(|v| v.get_mandates_by_payer(acc(ALICE), Some(3), Some(10)));
    assert_eq!(page.len(), 2);
    assert_eq!(s.view(|v| v.get_mandates_by_payer(acc(ALICE), Some(0), Some(2))).len(), 2);
    assert_eq!(s.view(|v| v.get_mandates_by_payer(acc(BOB), None, None))[0].mandate_id, 5);
    assert!(s.view(|v| v.get_mandates_by_payer(acc(MALLORY), None, None)).is_empty());

    assert_eq!(s.view(|v| v.next_collect_at(0)), Some(U64(T0)));
    assert_eq!(s.view(|v| v.next_collect_at(42)), None);
    s.cancel(ALICE, 0).unwrap();
    assert_eq!(s.view(|v| v.next_collect_at(0)), None);
    assert!(s.view(|v| v.get_mandate(42)).is_none());
}

// ============================================================== error-string coverage

/// Every stable error string is produced by at least one reachable path (table-driven).
#[test]
fn every_error_string_is_reachable() {
    let mut seen: Vec<&str> = vec![];
    let mut s = Sim::new();
    let base = create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string();

    // via ft_on_transfer logs
    s.register(ALICE, REG + 2 * MCOST);
    let log = |s: &mut Sim, tok: &str, who: &str, amt: u128, m: &str| {
        s.ft_on_transfer(tok, who, amt, m).logs[0].clone()
    };
    for (code, got) in [
        (ERR_BAD_MSG, log(&mut s, USDC, ALICE, 1, "x")),
        (
            ERR_FEE_TOO_HIGH,
            log(&mut s, USDC, ALICE, 1, &create_msg(MERCHANT, "intents_account", AMOUNT, 501).to_string()),
        ),
        (ERR_NO_STORAGE, log(&mut s, USDC, BOB, 1, &base)),
        (LOG_CAP_EXCEEDED_MANDATE, log(&mut s, USDC, ALICE, MANDATE_CAP + 1, &base)),
        (ERR_NOT_FOUND, log(&mut s, USDC, ALICE, 1, r#"{"top_up":{"mandate_id":9}}"#)),
    ] {
        assert_eq!(got, code);
        seen.push(errors::code_of(code));
    }
    let id = s.create_default();
    let top = json!({"top_up": {"mandate_id": id}}).to_string();
    assert_eq!(log(&mut s, USDC, BOB, 1, &top), ERR_UNAUTHORIZED);
    seen.push(ERR_UNAUTHORIZED);
    assert_eq!(s.ft_on_transfer(EVIL_TOKEN, ALICE, 1, &base).err(), ERR_TOKEN_NOT_ALLOWED);
    seen.push(ERR_TOKEN_NOT_ALLOWED);

    // via panics
    s.collect(id).unwrap();
    assert_eq!(s.collect(id).err(), ERR_IN_FLIGHT);
    seen.push(ERR_IN_FLIGHT);
    let msg = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "start_at", json!((T0 + 1).to_string()));
    let id2 = s.create(ALICE, 1, &msg);
    assert_eq!(s.collect(id2).err(), ERR_NOT_DUE);
    seen.push(ERR_NOT_DUE);
    s.now += 1;
    assert_eq!(s.collect(id2).err(), ERR_INSUFFICIENT_BALANCE);
    seen.push(ERR_INSUFFICIENT_BALANCE);
    assert_eq!(s.withdraw(ALICE, id2).err(), ERR_STILL_ACTIVE);
    seen.push(ERR_STILL_ACTIVE);
    assert_eq!(s.call(ALICE, 1, vec![], |v| v.storage_unregister(None)).err(), ERR_OPEN_MANDATES);
    seen.push(ERR_OPEN_MANDATES);
    s.cancel(ALICE, id2).unwrap();
    s.on_refunded(id2, 1, Some(ALICE), failed()).unwrap();
    assert_eq!(s.collect(id2).err(), ERR_NOT_ACTIVE);
    seen.push(ERR_NOT_ACTIVE);
    let msg = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "max_periods", json!(1));
    let id3 = s.create(BOB, 2 * AMOUNT, &msg);
    s.collect_ok(id3);
    assert_eq!(s.collect(id3).err(), ERR_MAX_PERIODS);
    seen.push(ERR_MAX_PERIODS);
    let mut e = Sim::empty();
    let mut cfg = default_config();
    cfg.allowed_tokens.clear();
    assert_eq!(e.init(cfg), Err(ERR_BAD_CONFIG.into()));
    seen.push(ERR_BAD_CONFIG);

    seen.sort();
    let mut all = errors::ALL.to_vec();
    all.sort();
    assert_eq!(seen, all);
}
