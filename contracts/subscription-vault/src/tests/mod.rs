//! Test harness: runs every contract call the way the chain does.
//!
//! Each [`Sim::call`] installs a fresh mocked blockchain over the persisted storage, reads the
//! contract state, runs the method, and on success writes the state back and drops the object
//! (flushing every collection). On panic the storage snapshot is restored and the half-mutated
//! object is leaked without flushing — i.e. a failed call has no effect, as on-chain. Logs and
//! created receipts of each call are returned for inspection.

mod audit_fixes;
mod invariants;
mod msg_fuzz;
mod unit;

use crate::*;
use near_sdk::mock::{MockAction, Receipt};
use near_sdk::serde_json::{self, Value, json};
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{Gas, MockedBlockchain, NearToken, PromiseResult, RuntimeFeesConfig, env, test_vm_config};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;

pub const VAULT: &str = "vault.test";
pub const USDC: &str = "usdc.test";
pub const USDT: &str = "usdt.test";
pub const EVIL_TOKEN: &str = "evil.test";
pub const TREASURY: &str = "treasury.test";
pub const INTENTS: &str = "intents.test";
pub const ALICE: &str = "alice.test";
pub const BOB: &str = "bob.test";
pub const MERCHANT: &str = "merchant.test";
pub const MALLORY: &str = "mallory.test";

pub const T0: u64 = 1_731_000_000_000_000_000; // ns
pub const DAY_NS: u64 = 86_400 * 1_000_000_000;
pub const CAP: u128 = 1_000_000_000_000; // 1M USDC (6 decimals) TVL cap per token
pub const MANDATE_CAP: u128 = 100_000_000_000; // 100k USDC per mandate
pub const MAX_FEE_BPS: u16 = 500;
pub const AMOUNT: u128 = 10_000_000; // 10 USDC per period
pub const MILLINEAR: u128 = 1_000_000_000_000_000_000_000;

pub fn acc(s: &str) -> AccountId {
    s.parse().unwrap()
}

pub fn yocto(n: u128) -> NearToken {
    NearToken::from_yoctonear(n)
}

/// Constructor arguments.
#[derive(Clone)]
pub struct Config {
    pub allowed_tokens: Vec<AccountId>,
    pub fee_receiver: AccountId,
    pub max_fee_bps: u16,
    pub tvl_caps: Vec<TokenCap>,
    pub intents_account: AccountId,
}

pub fn default_config() -> Config {
    Config {
        allowed_tokens: vec![acc(USDC), acc(USDT)],
        fee_receiver: acc(TREASURY),
        max_fee_bps: MAX_FEE_BPS,
        tvl_caps: vec![
            TokenCap { token: acc(USDC), cap: U128(CAP), mandate_cap: U128(MANDATE_CAP) },
            TokenCap { token: acc(USDT), cap: U128(CAP), mandate_cap: U128(MANDATE_CAP) },
        ],
        intents_account: acc(INTENTS),
    }
}

/// Result of one simulated call.
pub struct Outcome<R> {
    pub result: Result<R, String>,
    pub logs: Vec<String>,
    pub receipts: Vec<Receipt>,
}

impl<R: std::fmt::Debug> Outcome<R> {
    pub fn unwrap(self) -> R {
        match self.result {
            Ok(r) => r,
            Err(e) => panic!("call failed: {e}"),
        }
    }
    pub fn err(&self) -> &str {
        match &self.result {
            Ok(r) => panic!("expected failure, got {r:?}"),
            Err(e) => e,
        }
    }
    pub fn events(&self) -> Vec<Value> {
        events_of(&self.logs)
    }
}

pub fn events_of(logs: &[String]) -> Vec<Value> {
    logs.iter()
        .filter_map(|l| l.strip_prefix(events::EVENT_JSON_PREFIX))
        .map(|j| serde_json::from_str(j).unwrap())
        .collect()
}

/// A decoded action of a created receipt.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Call { receiver: String, method: String, args: Value, deposit: u128, gas: Gas, weight: u64 },
    Transfer { receiver: String, amount: u128 },
}

pub fn acts(receipts: &[Receipt]) -> Vec<Act> {
    let mut out = vec![];
    for r in receipts {
        for a in &r.actions {
            match a {
                MockAction::FunctionCallWeight {
                    method_name,
                    args,
                    attached_deposit,
                    prepaid_gas,
                    gas_weight,
                    ..
                } => out.push(Act::Call {
                    receiver: r.receiver_id.to_string(),
                    method: String::from_utf8(method_name.clone()).unwrap(),
                    args: serde_json::from_slice(args).unwrap_or(Value::Null),
                    deposit: attached_deposit.as_yoctonear(),
                    gas: *prepaid_gas,
                    weight: gas_weight.0,
                }),
                MockAction::Transfer { deposit, .. } => out.push(Act::Transfer {
                    receiver: r.receiver_id.to_string(),
                    amount: deposit.as_yoctonear(),
                }),
                MockAction::CreateReceipt { .. } => {}
                other => panic!("unexpected action {other:?}"),
            }
        }
    }
    out
}

fn quiet_panics() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Contract panics are expected in these tests; only print test-assertion panics.
            let msg = panic_msg(info.payload());
            if !msg.starts_with("ERR_") && !msg.contains("is private") && !msg.contains("yoctoNEAR") {
                default(info);
            }
        }));
    });
}

/// The contract's panic message: the mocked host wraps `env::panic_str(m)` as
/// `... HostError(GuestPanic { panic_msg: "m" })`; unwrap that to `m`.
fn panic_msg(p: &(dyn std::any::Any + Send)) -> String {
    let raw = if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        return "<non-string panic>".to_string();
    };
    const MARK: &str = "GuestPanic { panic_msg: \"";
    match raw.find(MARK) {
        Some(i) => {
            let rest = &raw[i + MARK.len()..];
            rest.rfind("\" }").map_or(rest, |j| &rest[..j]).replace("\\\"", "\"")
        }
        None => raw,
    }
}

pub struct Sim {
    pub now: u64,
    /// Prepaid gas of every call (Tgas).
    pub gas_tgas: u64,
}

impl Sim {
    pub fn new() -> Self {
        Self::with_config(default_config())
    }

    pub fn with_config(cfg: Config) -> Self {
        let mut s = Self::empty();
        s.init(cfg).unwrap();
        s
    }

    /// No contract state yet.
    pub fn empty() -> Self {
        quiet_panics();
        let s = Sim { now: T0, gas_tgas: 300 };
        s.install(VAULT, 0, vec![], HashMap::new());
        s
    }

    fn context(&self, predecessor: &str, deposit: u128) -> near_sdk::VMContext {
        VMContextBuilder::new()
            .current_account_id(acc(VAULT))
            .signer_account_id(acc(predecessor))
            .predecessor_account_id(acc(predecessor))
            .attached_deposit(yocto(deposit))
            .block_timestamp(self.now)
            .prepaid_gas(Gas::from_tgas(self.gas_tgas))
            .account_balance(NearToken::from_near(1_000))
            .build()
    }

    fn install(
        &self,
        predecessor: &str,
        deposit: u128,
        results: Vec<PromiseResult>,
        storage: HashMap<Vec<u8>, Vec<u8>>,
    ) {
        env::set_blockchain_interface(MockedBlockchain::new(
            self.context(predecessor, deposit),
            test_vm_config(),
            RuntimeFeesConfig::test(),
            results,
            storage,
            HashMap::new(),
            None,
        ));
    }

    fn take_storage() -> HashMap<Vec<u8>, Vec<u8>> {
        near_sdk::mock::with_mocked_blockchain(|b| b.take_storage())
    }

    pub fn init(&mut self, cfg: Config) -> Result<(), String> {
        self.init_as(VAULT, cfg)
    }

    pub fn init_as(&mut self, predecessor: &str, cfg: Config) -> Result<(), String> {
        let storage = Self::take_storage();
        let snapshot = storage.clone();
        self.install(predecessor, 0, vec![], storage);
        let r = catch_unwind(AssertUnwindSafe(|| {
            Vault::new(
                cfg.allowed_tokens,
                cfg.fee_receiver,
                cfg.max_fee_bps,
                cfg.tvl_caps,
                cfg.intents_account,
            )
        }));
        match r {
            Ok(v) => {
                env::state_write(&v);
                drop(v);
                Ok(())
            }
            Err(p) => {
                self.install(VAULT, 0, vec![], snapshot);
                Err(panic_msg(&*p))
            }
        }
    }

    /// Runs `f` as a call from `predecessor` with `deposit` yocto and the given promise results.
    pub fn call<R>(
        &mut self,
        predecessor: &str,
        deposit: u128,
        results: Vec<PromiseResult>,
        f: impl FnOnce(&mut Vault) -> R,
    ) -> Outcome<R> {
        let storage = Self::take_storage();
        let snapshot = storage.clone();
        self.install(predecessor, deposit, results, storage);
        let mut v: Vault = env::state_read().expect("contract state");
        match catch_unwind(AssertUnwindSafe(|| f(&mut v))) {
            Ok(r) => {
                env::state_write(&v);
                drop(v); // flush collections
                let logs = near_sdk::test_utils::get_logs();
                let receipts = near_sdk::test_utils::get_created_receipts();
                Outcome { result: Ok(r), logs, receipts }
            }
            Err(p) => {
                std::mem::forget(v); // never flush a reverted call
                self.install(VAULT, 0, vec![], snapshot);
                Outcome { result: Err(panic_msg(&*p)), logs: vec![], receipts: vec![] }
            }
        }
    }

    pub fn view<R>(&mut self, f: impl FnOnce(&Vault) -> R) -> R {
        self.call(MALLORY, 0, vec![], |v| f(v)).result.expect("view panicked")
    }

    // ---------------------------------------------------------------- conveniences

    pub fn register(&mut self, who: &str, amount: u128) -> StorageBalance {
        self.call(who, amount, vec![], |v| v.storage_deposit(None, None)).unwrap()
    }

    /// `ft_on_transfer` as called by `token`; returns (unused, logs).
    pub fn ft_on_transfer(&mut self, token: &str, sender: &str, amount: u128, msg: &str) -> Outcome<u128> {
        let msg = msg.to_string();
        let sender = acc(sender);
        self.call(token, 0, vec![], move |v| v.ft_on_transfer(sender, U128(amount), msg).0)
    }

    /// Registers `payer` for one more mandate and creates it; returns the id.
    pub fn create(&mut self, payer: &str, amount: u128, msg: &Value) -> u64 {
        self.register(payer, STORAGE_REGISTRATION_COST.as_yoctonear() + MANDATE_STORAGE_COST.as_yoctonear());
        let o = self.ft_on_transfer(USDC, payer, amount, &msg.to_string());
        assert_eq!(o.result, Ok(0), "create rejected: {:?}", o.logs);
        o.events()[0]["data"][0]["mandate_id"].as_u64().unwrap()
    }

    /// Default mandate: intents receiver, 10 USDC/30d, 1% fee, 12 periods, 3 periods funded.
    pub fn create_default(&mut self) -> u64 {
        self.create(ALICE, 3 * AMOUNT, &create_msg(MERCHANT, "intents_account", AMOUNT, 100))
    }

    pub fn collect(&mut self, id: u64) -> Outcome<()> {
        self.call(MALLORY, 0, vec![], move |v| {
            let _ = v.collect(id);
        })
    }

    pub fn on_merchant_leg(
        &mut self,
        id: u64,
        share: u128,
        fee: u128,
        r: PromiseResult,
    ) -> Outcome<Option<bool>> {
        self.call(VAULT, 0, vec![r], move |v| match v.on_merchant_leg(id, U128(share), U128(fee)) {
            PromiseOrValue::Value(b) => Some(b),
            PromiseOrValue::Promise(_) => None,
        })
    }

    pub fn on_collected(&mut self, id: u64, share: u128, fee: u128, r: PromiseResult) -> Outcome<bool> {
        self.call(VAULT, 0, vec![r], move |v| v.on_collected(id, U128(share), U128(fee)))
    }

    pub fn cancel(&mut self, who: &str, id: u64) -> Outcome<Option<bool>> {
        self.call(who, 1, vec![], move |v| match v.cancel(id) {
            PromiseOrValue::Value(b) => Some(b),
            PromiseOrValue::Promise(_) => None,
        })
    }

    pub fn withdraw(&mut self, who: &str, id: u64) -> Outcome<()> {
        self.call(who, 1, vec![], move |v| {
            let _ = v.withdraw(id);
        })
    }

    pub fn on_refunded(
        &mut self,
        id: u64,
        amount: u128,
        by: Option<&str>,
        r: PromiseResult,
    ) -> Outcome<bool> {
        let by = by.map(acc);
        self.call(VAULT, 0, vec![r], move |v| v.on_refunded(id, U128(amount), by))
    }

    pub fn mandate(&mut self, id: u64) -> Option<MandateView> {
        self.view(|v| v.get_mandate(id))
    }

    pub fn tvl(&mut self, token: &str) -> u128 {
        self.view(|v| v.get_token_tvl(acc(token)).map_or(0, |t| t.tvl.0))
    }

    /// Full successful collect of an intents/near mandate; returns the collect's events.
    pub fn collect_ok(&mut self, id: u64) -> Vec<Value> {
        let m = self.mandate(id).unwrap();
        let (share, fee) = math::split(m.amount_per_period.0, m.fee_bps);
        self.collect(id).unwrap();
        let o = self.on_merchant_leg(id, share, fee, ok_u128(share));
        let mut evs = o.events();
        if o.unwrap().is_none() {
            evs.extend(self.on_collected(id, share, fee, ok_empty()).events());
        }
        evs
    }
}

pub fn ok_u128(n: u128) -> PromiseResult {
    PromiseResult::Successful(format!("\"{n}\"").into_bytes())
}
pub fn ok_empty() -> PromiseResult {
    PromiseResult::Successful(vec![])
}
pub fn failed() -> PromiseResult {
    PromiseResult::Failed
}

pub fn create_msg(merchant: &str, kind: &str, amount: u128, fee_bps: u16) -> Value {
    json!({"create_mandate": {
        "merchant_receiver": merchant,
        "receiver_kind": kind,
        "amount_per_period": amount.to_string(),
        "period_secs": 30 * 86_400,
        "max_periods": 12,
        "fee_bps": fee_bps,
        "ext_ref": "sub_test"
    }})
}

pub fn with(mut msg: Value, key: &str, val: Value) -> Value {
    msg["create_mandate"].as_object_mut().unwrap().insert(key.to_string(), val);
    msg
}

pub fn without(mut msg: Value, key: &str) -> Value {
    msg["create_mandate"].as_object_mut().unwrap().remove(key);
    msg
}
