//! Property tests: random interleavings of every public method with randomly delayed and
//! randomly failing promise outcomes, checked against a model of the token ledger.
//!
//! Invariants (LLD §5, V6–V9 added in v1.2):
//! * V1  per token: Σ open mandates' (balance + in-flight collect + in-flight refund) == tvl
//!   ≤ tokens the vault actually holds (and holdings − tvl is exactly the tokens deliberately
//!   left unattributed by the "garbage success result" rule).
//! * V2  a payer can always cancel an active mandate and recover its balance: checked on every
//!   cancel/withdraw attempt and by a final drain that must close every mandate and return
//!   every unspent token.
//! * V3  per mandate: tokens actually paid out (merchant + fee) ≤ collected_periods ×
//!   amount_per_period, and collected_periods ≤ periods elapsed.
//! * V4  every token transfer goes to the mandate's payer, its merchant receiver (directly or
//!   via `intents_account` with msg = merchant) or the fee receiver; NEAR only to the storage
//!   account owner.
//! * V5  merchant_receiver, receiver kind, token, payer, amount, period, start, max periods,
//!   fee bps and ext_ref never change.
//! * V6  `in_flight` ⇔ exactly one collect leg pending; `refund_in_flight` == pending refund.
//! * V7  tvl ≤ cap for every token.
//! * V8  storage: each payer's locked mandate count == its open mandates == its index entries.
//! * V9  collect succeeds iff its preconditions hold, and fails with the documented error.
//! * V10 (LLD v1.3) per mandate: balance + in-flight collect + in-flight refund ≤ the token's
//!   per-mandate cap, and amount_per_period ≤ that cap.

use super::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

const PAYERS: [&str; 3] = [ALICE, BOB, "carol.test"];
const MERCHANTS: [&str; 2] = [MERCHANT, "shop.test"];
const TOKENS: [&str; 2] = [USDC, USDT];
const PROP_CAP: u128 = 60 * AMOUNT;
const PROP_MANDATE_CAP: u128 = 6 * AMOUNT;
const REG_Y: u128 = 2 * MILLINEAR;
const MCOST_Y: u128 = 10 * MILLINEAR;

#[derive(Debug, Clone)]
enum Leg {
    Full,
    Fail,
    /// Intents receiver used only part (permille of the share); NearAccount → Full.
    Partial(u16),
    /// Success with an unparseable value; the tokens actually stayed in the vault.
    Garbage,
}

#[derive(Debug, Clone)]
enum Op {
    Create {
        payer: usize,
        merchant: usize,
        token: usize,
        intents: bool,
        amount_idx: usize,
        periods_funded: u8,
        extra: u32,
        fee_bps: u16,
        max_periods: Option<u32>,
        start_days: u8,
        daily: bool,
        register: bool,
    },
    TopUp {
        sender: usize,
        mandate: usize,
        token: usize,
        amount: u128,
    },
    Collect {
        mandate: usize,
    },
    Deliver {
        pending: usize,
        leg: Leg,
        ok: bool,
    },
    Cancel {
        who: usize, // 0 payer, 1 merchant, 2 stranger
        mandate: usize,
    },
    Withdraw {
        payer: bool,
        mandate: usize,
    },
    Advance {
        hours: u32,
    },
    StorageWithdraw {
        payer: usize,
    },
}

// 7 × AMOUNT exceeds the per-mandate cap (amount_per_period rejection).
const AMOUNTS: [u128; 6] = [1, 7, 999_999, AMOUNT, 3_333_333, 7 * AMOUNT];

fn leg() -> impl Strategy<Value = Leg> {
    prop_oneof![
        6 => Just(Leg::Full),
        3 => Just(Leg::Fail),
        1 => (0u16..1000).prop_map(Leg::Partial),
        1 => Just(Leg::Garbage),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (
            (0..PAYERS.len(), 0..MERCHANTS.len(), 0..TOKENS.len(), any::<bool>(), 0..AMOUNTS.len()),
            (0u8..6, 0u32..1000, prop_oneof![0u16..=500, Just(501u16)], prop_oneof![Just(None), (1u32..4).prop_map(Some)]),
            (0u8..3, any::<bool>(), prop::bool::weighted(0.9)),
        )
            .prop_map(|((payer, merchant, token, intents, amount_idx), (periods_funded, extra, fee_bps, max_periods), (start_days, daily, register))| Op::Create {
                payer, merchant, token, intents, amount_idx, periods_funded, extra, fee_bps, max_periods, start_days, daily, register,
            }),
        1 => (0..PAYERS.len() + 1, 0usize..8, prop::bool::weighted(0.9).prop_map(|b| if b { 0 } else { 1 }), 1u128..(3 * AMOUNT))
            .prop_map(|(sender, mandate, token, amount)| Op::TopUp { sender, mandate, token, amount }),
        4 => (0usize..8).prop_map(|mandate| Op::Collect { mandate }),
        5 => (0usize..8, leg(), prop::bool::weighted(0.8)).prop_map(|(pending, leg, ok)| Op::Deliver { pending, leg, ok }),
        1 => (0usize..3, 0usize..8).prop_map(|(who, mandate)| Op::Cancel { who, mandate }),
        1 => (prop::bool::weighted(0.9), 0usize..8).prop_map(|(payer, mandate)| Op::Withdraw { payer, mandate }),
        2 => prop_oneof![1u32..48, 700u32..800].prop_map(|hours| Op::Advance { hours }),
        1 => (0..PAYERS.len()).prop_map(|payer| Op::StorageWithdraw { payer }),
    ]
}

#[derive(Debug, Clone)]
enum Pending {
    MerchantLeg { id: u64, share: u128, fee: u128 },
    FeeLeg { id: u64, share: u128, fee: u128 },
    Refund { id: u64, amount: u128, by: Option<String> },
}

impl Pending {
    fn id(&self) -> u64 {
        match self {
            Pending::MerchantLeg { id, .. } | Pending::FeeLeg { id, .. } | Pending::Refund { id, .. } => *id,
        }
    }
}

/// Immutable terms (V5) and money flows per mandate.
#[derive(Debug, Clone)]
struct Track {
    terms: (AccountId, AccountId, ReceiverKind, AccountId, U128, u64, U64, Option<u32>, u16, String),
    deposited: u128,
    to_payer: u128,
    to_merchant: u128,
    to_fee: u128,
    stranded: u128,
    /// Tokens that left (or were marked stranded) during the current, not yet settled collect.
    inflight_out: u128,
}

fn terms(
    m: &MandateView,
) -> (AccountId, AccountId, ReceiverKind, AccountId, U128, u64, U64, Option<u32>, u16, String) {
    (
        m.payer.clone(),
        m.merchant_receiver.clone(),
        m.receiver_kind,
        m.token.clone(),
        m.amount_per_period,
        m.period_secs,
        m.start_at,
        m.max_periods,
        m.fee_bps,
        m.ext_ref.clone(),
    )
}

struct Model {
    s: Sim,
    holdings: BTreeMap<String, u128>,
    stranded: BTreeMap<String, u128>,
    ids: Vec<u64>,
    track: BTreeMap<u64, Track>,
    pending: Vec<Pending>,
    closed: BTreeSet<u64>,
}

fn config() -> Config {
    let mut c = default_config();
    c.tvl_caps = TOKENS
        .iter()
        .map(|t| TokenCap { token: acc(t), cap: U128(PROP_CAP), mandate_cap: U128(PROP_MANDATE_CAP) })
        .collect();
    c
}

fn call_args(a: &Act) -> (&str, &str, &Value) {
    match a {
        Act::Call { receiver, method, args, .. } => (receiver.as_str(), method.as_str(), args),
        Act::Transfer { .. } => panic!("unexpected NEAR transfer {a:?}"),
    }
}

fn amount_arg(v: &Value, key: &str) -> u128 {
    v[key].as_str().unwrap().parse().unwrap()
}

impl Model {
    fn new() -> Self {
        Model {
            s: Sim::with_config(config()),
            holdings: BTreeMap::new(),
            stranded: BTreeMap::new(),
            ids: vec![],
            track: BTreeMap::new(),
            pending: vec![],
            closed: BTreeSet::new(),
        }
    }

    fn pick(&self, i: usize) -> Option<u64> {
        if self.ids.is_empty() { None } else { Some(self.ids[i % self.ids.len()]) }
    }

    fn hold(&mut self, token: &str, delta: i128) {
        let h = self.holdings.entry(token.to_string()).or_default();
        *h = (*h as i128 + delta) as u128;
    }

    fn apply(&mut self, op: &Op) {
        match op.clone() {
            Op::Create {
                payer,
                merchant,
                token,
                intents,
                amount_idx,
                periods_funded,
                extra,
                fee_bps,
                max_periods,
                start_days,
                daily,
                register,
            } => {
                let payer = PAYERS[payer];
                let token = TOKENS[token];
                let amount = AMOUNTS[amount_idx];
                if register {
                    let need = if self.s.view(|v| v.storage_balance_of(acc(payer))).is_none() {
                        REG_Y + MCOST_Y
                    } else {
                        MCOST_Y
                    };
                    self.s.register(payer, need);
                }
                let mut msg = create_msg(
                    MERCHANTS[merchant],
                    if intents { "intents_account" } else { "near_account" },
                    amount,
                    fee_bps,
                );
                msg = with(msg, "max_periods", max_periods.map_or(Value::Null, |m| json!(m)));
                msg = with(msg, "period_secs", json!(if daily { 86_400 } else { 30 * 86_400 }));
                msg = with(msg, "start_at", json!((self.s.now + u64::from(start_days) * DAY_NS).to_string()));
                let funding = amount * u128::from(periods_funded) + u128::from(extra);
                if funding == 0 {
                    return;
                }
                let o = self.s.ft_on_transfer(token, payer, funding, &msg.to_string());
                let unused = o.result.clone().unwrap();
                assert!(o.receipts.is_empty());
                if unused == 0 {
                    let id = o.events()[0]["data"][0]["mandate_id"].as_u64().unwrap();
                    self.hold(token, funding as i128);
                    let m = self.s.mandate(id).unwrap();
                    self.track.insert(
                        id,
                        Track {
                            terms: terms(&m),
                            deposited: funding,
                            to_payer: 0,
                            to_merchant: 0,
                            to_fee: 0,
                            stranded: 0,
                            inflight_out: 0,
                        },
                    );
                    self.ids.push(id);
                } else {
                    assert_eq!(unused, funding);
                    assert_eq!(o.logs.len(), 1);
                    assert!(errors::ALL.contains(&errors::code_of(&o.logs[0])), "{:?}", o.logs);
                }
            }
            Op::TopUp { sender, mandate, token, amount } => {
                let Some(id) = self.pick(mandate) else { return };
                let sender = if sender < PAYERS.len() { PAYERS[sender] } else { MALLORY };
                let mtoken = self.track[&id].terms.3.to_string();
                let token = if token == 0 {
                    mtoken.clone()
                } else {
                    TOKENS.iter().find(|t| **t != mtoken).unwrap().to_string()
                };
                let o = self.s.ft_on_transfer(
                    &token,
                    sender,
                    amount,
                    &json!({"top_up": {"mandate_id": id}}).to_string(),
                );
                if o.result.clone().unwrap() == 0 {
                    assert_eq!(token, mtoken);
                    assert_eq!(sender, self.track[&id].terms.0.as_str());
                    self.hold(&token, amount as i128);
                    self.track.get_mut(&id).unwrap().deposited += amount;
                }
            }
            Op::Collect { mandate } => {
                let Some(id) = self.pick(mandate) else { return };
                let before = self.s.mandate(id);
                let o = self.s.collect(id);
                self.check_collect_rule(before.as_ref(), &o);
                if o.result.is_ok() {
                    let m = before.unwrap();
                    let a = acts(&o.receipts);
                    assert_eq!(a.len(), 2);
                    let (recv, method, args) = call_args(&a[0]);
                    assert_eq!(recv, m.token.as_str());
                    match m.receiver_kind {
                        ReceiverKind::IntentsAccount => {
                            assert_eq!(method, "ft_transfer_call");
                            assert_eq!(args["receiver_id"], INTENTS);
                            assert_eq!(args["msg"], m.merchant_receiver.as_str());
                        }
                        ReceiverKind::NearAccount => {
                            assert_eq!(method, "ft_transfer");
                            assert_eq!(args["receiver_id"], m.merchant_receiver.as_str());
                        }
                    }
                    let (share, fee) = math::split(m.amount_per_period.0, m.fee_bps);
                    assert_eq!(amount_arg(args, "amount"), share);
                    let (recv, method, _) = call_args(&a[1]);
                    assert_eq!((recv, method), (VAULT, "on_merchant_leg"));
                    self.pending.push(Pending::MerchantLeg { id, share, fee });
                }
            }
            Op::Deliver { pending, leg, ok } => {
                if self.pending.is_empty() {
                    return;
                }
                let p = self.pending.remove(pending % self.pending.len());
                self.deliver(p, leg, ok);
            }
            Op::Cancel { who, mandate } => {
                let Some(id) = self.pick(mandate) else { return };
                let t = self.track[&id].clone();
                let caller = match who {
                    0 => t.terms.0.to_string(),
                    1 => t.terms.1.to_string(),
                    _ => MALLORY.to_string(),
                };
                let before = self.s.mandate(id);
                let o = self.s.cancel(&caller, id);
                match &before {
                    None => assert_eq!(o.err(), ERR_NOT_FOUND),
                    Some(_) if who == 2 => assert_eq!(o.err(), ERR_UNAUTHORIZED),
                    Some(m) if m.status != Status::Active => assert_eq!(o.err(), ERR_NOT_ACTIVE),
                    Some(m) => {
                        // V2: an active mandate can always be canceled by its payer (or merchant).
                        assert!(o.result.is_ok(), "cancel failed: {:?}", o.result);
                        self.after_refund_call(id, &o.receipts, m.balance.0, Some(caller));
                    }
                }
            }
            Op::Withdraw { payer, mandate } => {
                let Some(id) = self.pick(mandate) else { return };
                let caller = if payer { self.track[&id].terms.0.to_string() } else { MALLORY.to_string() };
                let before = self.s.mandate(id);
                let o = self.s.withdraw(&caller, id);
                match &before {
                    None => assert_eq!(o.err(), ERR_NOT_FOUND),
                    Some(_) if !payer => assert_eq!(o.err(), ERR_UNAUTHORIZED),
                    Some(m) if m.status == Status::Active => assert_eq!(o.err(), ERR_STILL_ACTIVE),
                    Some(m) if m.refund_in_flight.0 > 0 => assert_eq!(o.err(), ERR_IN_FLIGHT),
                    Some(m) if m.balance.0 == 0 => assert_eq!(o.err(), ERR_INSUFFICIENT_BALANCE),
                    Some(m) => {
                        assert!(o.result.is_ok(), "withdraw failed: {:?}", o.result);
                        self.after_refund_call(id, &o.receipts, m.balance.0, None);
                    }
                }
            }
            Op::Advance { hours } => self.s.now += u64::from(hours) * 3_600 * 1_000_000_000,
            Op::StorageWithdraw { payer } => {
                let p = PAYERS[payer];
                let before = self.s.view(|v| v.storage_balance_of(acc(p)));
                let o = self.s.call(p, 1, vec![], |v| v.storage_withdraw(None));
                match before {
                    None => assert_eq!(o.err(), ERR_NO_STORAGE),
                    Some(b) => {
                        let a = acts(&o.receipts);
                        if b.available.0 > 0 {
                            assert_eq!(a, vec![Act::Transfer { receiver: p.into(), amount: b.available.0 }]);
                        } else {
                            assert!(a.is_empty());
                        }
                    }
                }
            }
        }
    }

    /// V9: collect outcome matches its documented rule order.
    fn check_collect_rule(&self, before: Option<&MandateView>, o: &Outcome<()>) {
        let Some(m) = before else {
            assert_eq!(o.err(), ERR_NOT_FOUND);
            return;
        };
        let due =
            math::due_at(m.start_at.0, m.collected_periods, m.period_secs).is_some_and(|d| self.s.now >= d);
        let expect = match m.status {
            Status::Exhausted => Some(ERR_MAX_PERIODS),
            Status::Canceled => Some(ERR_NOT_ACTIVE),
            Status::Active if m.in_flight => Some(ERR_IN_FLIGHT),
            Status::Active if m.max_periods.is_some_and(|x| m.collected_periods >= x) => {
                Some(ERR_MAX_PERIODS)
            }
            Status::Active if !due => Some(ERR_NOT_DUE),
            Status::Active if m.balance.0 < m.amount_per_period.0 => Some(ERR_INSUFFICIENT_BALANCE),
            Status::Active => None,
        };
        match expect {
            Some(code) => assert_eq!(o.err(), code),
            None => assert!(o.result.is_ok(), "collect should succeed: {:?}", o.result),
        }
    }

    fn after_refund_call(&mut self, id: u64, receipts: &[Receipt], balance: u128, by: Option<String>) {
        let a = acts(receipts);
        if balance == 0 {
            assert!(a.is_empty());
            return;
        }
        assert_eq!(a.len(), 2);
        let (recv, method, args) = call_args(&a[0]);
        let t = &self.track[&id];
        assert_eq!((recv, method), (t.terms.3.as_str(), "ft_transfer"));
        assert_eq!(args["receiver_id"], t.terms.0.as_str()); // V4: refunds only to the payer
        assert_eq!(amount_arg(args, "amount"), balance);
        assert_eq!(call_args(&a[1]).1, "on_refunded");
        self.pending.push(Pending::Refund { id, amount: balance, by });
    }

    fn deliver(&mut self, p: Pending, leg: Leg, ok: bool) {
        let id = p.id();
        let token = self.track[&id].terms.3.to_string();
        match p {
            Pending::MerchantLeg { id, share, fee } => {
                let kind = self.track[&id].terms.2;
                let (result, moved) = match (leg, kind) {
                    (Leg::Fail, _) => (failed(), 0),
                    (_, ReceiverKind::NearAccount) => (ok_empty(), share),
                    (Leg::Full, ReceiverKind::IntentsAccount) => (ok_u128(share), share),
                    (Leg::Partial(pm), ReceiverKind::IntentsAccount) => {
                        let used = share * u128::from(pm) / 1000;
                        (ok_u128(used), used)
                    }
                    (Leg::Garbage, ReceiverKind::IntentsAccount) => {
                        (PromiseResult::Successful(b"\"garbage\"".to_vec()), 0)
                    }
                };
                let garbage =
                    matches!(&result, PromiseResult::Successful(b) if b.as_slice() == b"\"garbage\"");
                self.hold(&token, -(moved as i128));
                self.track.get_mut(&id).unwrap().to_merchant += moved;
                self.track.get_mut(&id).unwrap().inflight_out += moved;
                if garbage {
                    // The vault assumes the share left; it stays unattributed (V1 slack).
                    *self.stranded.entry(token.clone()).or_default() += share;
                    self.track.get_mut(&id).unwrap().stranded += share;
                    self.track.get_mut(&id).unwrap().inflight_out += share;
                }
                let o = self.s.on_merchant_leg(id, share, fee, result);
                match o.result.clone().unwrap() {
                    None => {
                        let a = acts(&o.receipts);
                        assert_eq!(a.len(), 2);
                        let (recv, method, args) = call_args(&a[0]);
                        assert_eq!((recv, method), (token.as_str(), "ft_transfer"));
                        assert_eq!(args["receiver_id"], TREASURY); // V4
                        assert_eq!(amount_arg(args, "amount"), fee);
                        assert!(moved == share || garbage, "fee leg only after full delivery");
                        self.pending.push(Pending::FeeLeg { id, share, fee });
                    }
                    Some(_) => {
                        assert!(o.receipts.is_empty());
                        self.track.get_mut(&id).unwrap().inflight_out = 0; // settled
                    }
                }
            }
            Pending::FeeLeg { id, share, fee } => {
                if ok {
                    self.hold(&token, -(fee as i128));
                    self.track.get_mut(&id).unwrap().to_fee += fee;
                }
                self.track.get_mut(&id).unwrap().inflight_out = 0; // settled below
                let o = self.s.on_collected(id, share, fee, if ok { ok_empty() } else { failed() });
                assert!(o.result.unwrap());
                assert!(o.receipts.is_empty());
            }
            Pending::Refund { id, amount, by } => {
                if ok {
                    self.hold(&token, -(amount as i128));
                    self.track.get_mut(&id).unwrap().to_payer += amount;
                }
                let o = self.s.on_refunded(id, amount, by.as_deref(), if ok { ok_empty() } else { failed() });
                assert_eq!(o.result.unwrap(), ok);
                assert!(o.receipts.is_empty());
            }
        }
    }

    fn check(&mut self) {
        // One read of everything (each simulated call re-creates the mocked chain).
        let ids = self.ids.clone();
        let snap = self.s.view(move |v| {
            let mandates: BTreeMap<u64, Option<MandateView>> =
                ids.iter().map(|id| (*id, v.get_mandate(*id))).collect();
            let tvl: BTreeMap<String, u128> = TOKENS
                .iter()
                .map(|t| (t.to_string(), v.get_token_tvl(acc(t)).map_or(0, |x| x.tvl.0)))
                .collect();
            let payers: BTreeMap<String, (BTreeSet<u64>, u32)> = PAYERS
                .iter()
                .map(|p| {
                    let idx = v
                        .get_mandates_by_payer(acc(p), None, Some(100))
                        .iter()
                        .map(|m| m.mandate_id)
                        .collect();
                    let locked = v.storage_accounts.get(&acc(p)).map_or(0, |a| a.mandates);
                    (p.to_string(), (idx, locked))
                })
                .collect();
            (mandates, tvl, payers)
        });
        let mut held: BTreeMap<String, u128> = BTreeMap::new();
        let mut inflight_out: BTreeMap<String, u128> = BTreeMap::new();
        let mut open_by_payer: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
        let now = self.s.now;
        for &id in &self.ids.clone() {
            let m = snap.0[&id].clone();
            let t = self.track[&id].clone();
            let pend_collect = self.pending.iter().filter(|p| matches!(p, Pending::MerchantLeg { id: i, .. } | Pending::FeeLeg { id: i, .. } if *i == id)).count();
            let pend_refund: u128 = self
                .pending
                .iter()
                .map(|p| match p {
                    Pending::Refund { id: i, amount, .. } if *i == id => *amount,
                    _ => 0,
                })
                .sum();
            match m {
                Some(m) => {
                    assert!(!self.closed.contains(&id), "closed mandate {id} reappeared");
                    // V5
                    assert_eq!(terms(&m), t.terms, "V5: terms of {id} changed");
                    // V6
                    assert_eq!(m.in_flight, pend_collect == 1, "V6 in_flight {id}");
                    assert!(pend_collect <= 1);
                    assert_eq!(m.refund_in_flight.0, pend_refund, "V6 refund_in_flight {id}");
                    // V3
                    // Tokens leave before the callback counts the period, so the in-flight
                    // period may already be (partly) paid.
                    let paid = t.to_merchant + t.to_fee;
                    let counted = u128::from(m.collected_periods) + u128::from(m.in_flight);
                    assert!(paid <= counted * m.amount_per_period.0, "V3 paid {id}");
                    if !m.in_flight {
                        assert!(
                            paid <= u128::from(m.collected_periods) * m.amount_per_period.0,
                            "V3 paid {id}"
                        );
                    }
                    assert!(
                        u128::from(m.collected_periods)
                            <= math::elapsed_periods(m.start_at.0, m.period_secs, now),
                        "V3 periods {id}"
                    );
                    if let Some(max) = m.max_periods {
                        assert!(m.collected_periods <= max);
                        assert_eq!(
                            m.status == Status::Exhausted,
                            m.collected_periods == max && m.status != Status::Canceled
                        );
                    }
                    let in_flight_amt = if m.in_flight { m.amount_per_period.0 } else { 0 };
                    // V10
                    assert!(
                        m.balance.0 + in_flight_amt + m.refund_in_flight.0 <= PROP_MANDATE_CAP,
                        "V10 held {id}"
                    );
                    assert!(m.amount_per_period.0 <= PROP_MANDATE_CAP, "V10 amount {id}");
                    *held.entry(m.token.to_string()).or_default() +=
                        m.balance.0 + in_flight_amt + m.refund_in_flight.0;
                    // Per-mandate conservation while open.
                    assert_eq!(
                        t.deposited + t.inflight_out,
                        m.balance.0
                            + in_flight_amt
                            + m.refund_in_flight.0
                            + t.to_payer
                            + t.to_merchant
                            + t.to_fee
                            + t.stranded,
                        "conservation {id}"
                    );
                    if !m.in_flight {
                        assert_eq!(t.inflight_out, 0);
                    }
                    *inflight_out.entry(m.token.to_string()).or_default() += t.inflight_out;
                    open_by_payer.entry(m.payer.to_string()).or_default().insert(id);
                }
                None => {
                    self.closed.insert(id);
                    assert_eq!(pend_collect, 0);
                    assert_eq!(pend_refund, 0);
                    assert_eq!(
                        t.deposited,
                        t.to_payer + t.to_merchant + t.to_fee + t.stranded,
                        "closed conservation {id}"
                    );
                }
            }
        }
        for token in TOKENS {
            let tvl = snap.1[token];
            let h = self.holdings.get(token).copied().unwrap_or(0);
            let st = self.stranded.get(token).copied().unwrap_or(0);
            let sum = held.get(token).copied().unwrap_or(0);
            let out = inflight_out.get(token).copied().unwrap_or(0);
            assert_eq!(tvl, sum, "tvl accounting {token}");
            // V1: what the vault owes (minus what already left in unsettled collects) is held.
            assert!(tvl - out <= h, "V1 {token}: tvl {tvl} - in-flight out {out} > holdings {h}");
            if out == 0 {
                assert!(tvl <= h, "V1 {token}");
            }
            assert_eq!(h + out, tvl + st, "unexplained tokens {token}");
            assert!(tvl <= PROP_CAP, "V7 {token}");
        }
        // V8
        for p in PAYERS {
            let open = open_by_payer.get(p).cloned().unwrap_or_default();
            let (idx, locked) = snap.2[p].clone();
            assert_eq!(idx, open, "V8 index {p}");
            assert_eq!(locked as usize, open.len(), "V8 storage lock {p}");
        }
    }

    /// V2 end state: deliver everything successfully, then every payer exits.
    fn drain(&mut self) {
        while let Some(p) = self.pending.pop() {
            self.deliver(p, Leg::Full, true);
        }
        self.check();
        for &id in &self.ids.clone() {
            let Some(m) = self.s.mandate(id) else { continue };
            let payer = m.payer.to_string();
            if m.status == Status::Active {
                let o = self.s.cancel(&payer, id);
                assert!(o.result.is_ok(), "V2 cancel {id}: {:?}", o.result);
                self.after_refund_call(id, &o.receipts, m.balance.0, Some(payer.clone()));
            }
            while let Some(p) = self.pending.pop() {
                self.deliver(p, Leg::Full, true);
            }
            if let Some(m) = self.s.mandate(id) {
                assert!(m.balance.0 > 0, "open zero-balance mandate {id} after drain: {m:?}");
                let o = self.s.withdraw(&payer, id);
                assert!(o.result.is_ok(), "V2 withdraw {id}: {:?}", o.result);
                self.after_refund_call(id, &o.receipts, m.balance.0, None);
                while let Some(p) = self.pending.pop() {
                    self.deliver(p, Leg::Full, true);
                }
            }
            assert!(self.s.mandate(id).is_none(), "V2: mandate {id} not closed");
            self.check();
        }
        for token in TOKENS {
            assert_eq!(self.s.tvl(token), 0);
            assert_eq!(
                self.holdings.get(token).copied().unwrap_or(0),
                self.stranded.get(token).copied().unwrap_or(0)
            );
        }
        for p in PAYERS {
            if self.s.view(|v| v.storage_balance_of(acc(p))).is_some() {
                assert!(
                    self.s.call(p, 1, vec![], |v| v.storage_unregister(None)).unwrap(),
                    "storage stuck for {p}"
                );
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(96),
        max_shrink_iters: 2_000,
        .. ProptestConfig::default()
    })]

    #[test]
    fn invariants_hold_under_random_interleavings(ops in prop::collection::vec(op(), 1..70)) {
        let mut model = Model::new();
        for op in &ops {
            model.apply(op);
            model.check();
        }
        model.drain();
    }
}

/// A fixed adversarial schedule: everything in flight at once, then delivered in reverse.
#[test]
fn everything_in_flight_then_reverse_delivery() {
    let mut model = Model::new();
    let ops = vec![
        Op::Create {
            payer: 0,
            merchant: 0,
            token: 0,
            intents: true,
            amount_idx: 3,
            periods_funded: 3,
            extra: 5,
            fee_bps: 100,
            max_periods: Some(2),
            start_days: 0,
            daily: true,
            register: true,
        },
        Op::Create {
            payer: 1,
            merchant: 1,
            token: 0,
            intents: false,
            amount_idx: 4,
            periods_funded: 2,
            extra: 0,
            fee_bps: 0,
            max_periods: None,
            start_days: 0,
            daily: true,
            register: true,
        },
        Op::Collect { mandate: 0 },
        Op::Collect { mandate: 1 },
        Op::Cancel { who: 1, mandate: 0 },
        Op::Cancel { who: 0, mandate: 1 },
        Op::Deliver { pending: 3, leg: Leg::Full, ok: false },
        Op::Deliver { pending: 2, leg: Leg::Full, ok: true },
        Op::Deliver { pending: 1, leg: Leg::Garbage, ok: true },
        Op::Deliver { pending: 0, leg: Leg::Full, ok: false },
        Op::Deliver { pending: 0, leg: Leg::Full, ok: true },
        Op::Withdraw { payer: true, mandate: 0 },
        Op::Withdraw { payer: true, mandate: 1 },
    ];
    for op in &ops {
        model.apply(op);
        model.check();
    }
    model.drain();
}
