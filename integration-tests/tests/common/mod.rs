//! Shared sandbox setup: mock NEP-141 token(s), mock `intents.near`, the vault, and accounts.
#![allow(dead_code)]

use anyhow::{Context, Result, anyhow};
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{Value, json};
use std::path::PathBuf;

pub const AMOUNT: u128 = 10_000_000; // 10 USDC (6 decimals) per period
pub const FEE_BPS: u16 = 100;
pub const MINTED: u128 = 1_000_000_000;
pub const DEFAULT_CAP: u128 = 1_000_000_000_000;
pub const DEFAULT_MANDATE_CAP: u128 = 100_000_000_000;
/// Gas the keeper attaches to `collect` (LLD v1.4 §3.3).
pub const COLLECT_GAS: Gas = Gas::from_tgas(250);
/// Mirrors the contract's `MIN_COLLECT_GAS`.
pub const MIN_COLLECT_GAS: Gas = Gas::from_tgas(170);
/// Mirrors the contract's `MIN_EXIT_GAS` (cancel / withdraw).
pub const MIN_EXIT_GAS: Gas = Gas::from_tgas(50);

pub fn wasm(name: &str) -> Result<Vec<u8>> {
    let dir = std::env::var("NEARPAY_WASM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../res"));
    let path = dir.join(format!("{name}.wasm"));
    std::fs::read(&path).with_context(|| format!("missing {} — run scripts/build.sh first", path.display()))
}

pub fn split(amount: u128, fee_bps: u16) -> (u128, u128) {
    let k = 10_000 - u128::from(fee_bps);
    let share = (amount / 10_000) * k + (amount % 10_000) * k / 10_000;
    (share, amount - share)
}

pub struct Setup {
    pub cap: u128,
    pub mandate_cap: u128,
    pub register_treasury: bool,
    pub register_merchant: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Setup {
            cap: DEFAULT_CAP,
            mandate_cap: DEFAULT_MANDATE_CAP,
            register_treasury: true,
            register_merchant: true,
        }
    }
}

pub struct Ctx {
    pub worker: Worker<Sandbox>,
    pub root: Account,
    pub ft: Contract,
    /// A second token that is NOT in the vault's allowed list.
    pub other_ft: Contract,
    pub intents: Contract,
    pub vault: Contract,
    pub alice: Account,
    pub bob: Account,
    pub merchant: Account,
    pub treasury: Account,
    pub keeper: Account,
}

pub async fn sub(root: &Account, name: &str, near: u128) -> Result<Account> {
    Ok(root
        .create_subaccount(name)
        .initial_balance(NearToken::from_near(near))
        .transact()
        .await?
        .into_result()?)
}

pub async fn deploy(root: &Account, name: &str, wasm_name: &str) -> Result<Contract> {
    let acct = sub(root, name, 30).await?;
    Ok(acct.deploy(&wasm(wasm_name)?).await?.into_result()?)
}

pub fn ok(res: &ExecutionFinalResult) -> Result<()> {
    if res.is_success() && res.failures().is_empty() {
        Ok(())
    } else {
        Err(anyhow!("transaction failed: {res:?}"))
    }
}

pub fn assert_fails_with(res: &ExecutionFinalResult, needle: &str) {
    assert!(res.is_failure(), "expected failure with {needle}, got success: {res:?}");
    let dbg = format!("{res:?}");
    assert!(dbg.contains(needle), "expected {needle} in {dbg}");
}

/// NEP-297 events of `standard` in the logs of every receipt of `res`.
pub fn events(res: &ExecutionFinalResult, standard: &str) -> Vec<Value> {
    res.logs()
        .iter()
        .filter_map(|l| l.strip_prefix("EVENT_JSON:"))
        .filter_map(|j| serde_json::from_str::<Value>(j).ok())
        .filter(|e| e["standard"] == standard)
        .collect()
}

pub fn vault_events(res: &ExecutionFinalResult) -> Vec<Value> {
    events(res, "nearpay_vault")
}

/// `true` if some log line is a rejection with this stable code (text before `:`).
pub fn has_code(res: &ExecutionFinalResult, code: &str) -> bool {
    res.logs().iter().any(|l| l.split(':').next() == Some(code))
}

pub fn names(evs: &[Value]) -> Vec<String> {
    evs.iter().map(|e| e["event"].as_str().unwrap_or_default().to_string()).collect()
}

pub async fn ft_register(ft: &Contract, who: &AccountId) -> Result<()> {
    let res = ft
        .as_account()
        .call(ft.id(), "storage_deposit")
        .args_json(json!({"account_id": who, "registration_only": true}))
        .deposit(NearToken::from_millinear(13))
        .transact()
        .await?;
    ok(&res)
}

pub async fn ft_balance(ft: &Contract, who: &AccountId) -> Result<u128> {
    let v: String = ft.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
    Ok(v.parse()?)
}

pub async fn intents_balance(ctx: &Ctx, who: &AccountId) -> Result<u128> {
    let v: String = ctx
        .intents
        .view("mt_balance_of")
        .args_json(json!({"account_id": who, "token_id": format!("nep141:{}", ctx.ft.id())}))
        .await?
        .json()?;
    Ok(v.parse()?)
}

pub async fn setup(opts: Setup) -> Result<Ctx> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let ft = deploy(&root, "usdc", "mock_ft").await?;
    ok(&ft.call("new").transact().await?)?;
    let other_ft = deploy(&root, "evil", "mock_ft").await?;
    ok(&other_ft.call("new").transact().await?)?;
    let intents = deploy(&root, "intents", "mock_intents").await?;
    ok(&intents.call("new").transact().await?)?;

    let alice = sub(&root, "alice", 20).await?;
    let bob = sub(&root, "bob", 20).await?;
    let merchant = sub(&root, "merchant", 5).await?;
    let treasury = sub(&root, "treasury", 5).await?;
    let keeper = sub(&root, "keeper", 5).await?;

    let vault = deploy(&root, "vault", "subscription_vault").await?;
    let res = vault
        .call("new")
        .args_json(json!({
            "allowed_tokens": [ft.id()],
            "fee_receiver": treasury.id(),
            "max_fee_bps": 500,
            "tvl_caps": [{"token": ft.id(), "cap": opts.cap.to_string(), "mandate_cap": opts.mandate_cap.min(opts.cap).to_string()}],
            "intents_account": intents.id(),
        }))
        .transact()
        .await?;
    ok(&res)?;

    for who in [alice.id(), bob.id(), vault.id(), intents.id()] {
        ft_register(&ft, who).await?;
        ft_register(&other_ft, who).await?;
    }
    if opts.register_treasury {
        ft_register(&ft, treasury.id()).await?;
    }
    if opts.register_merchant {
        ft_register(&ft, merchant.id()).await?;
    }
    for who in [alice.id(), bob.id()] {
        ok(&ft
            .call("mint")
            .args_json(json!({"account_id": who, "amount": MINTED.to_string()}))
            .transact()
            .await?)?;
        ok(&other_ft
            .call("mint")
            .args_json(json!({"account_id": who, "amount": MINTED.to_string()}))
            .transact()
            .await?)?;
    }
    // Alice pre-pays vault storage for two mandates.
    let res = alice
        .call(vault.id(), "storage_deposit")
        .args_json(json!({}))
        .deposit(NearToken::from_millinear(22))
        .transact()
        .await?;
    ok(&res)?;

    Ok(Ctx { worker, root, ft, other_ft, intents, vault, alice, bob, merchant, treasury, keeper })
}

pub fn create_msg(
    merchant: &AccountId,
    kind: &str,
    amount: u128,
    fee_bps: u16,
    max_periods: Option<u32>,
) -> String {
    json!({"create_mandate": {
        "merchant_receiver": merchant,
        "receiver_kind": kind,
        "amount_per_period": amount.to_string(),
        "period_secs": 86_400,
        "max_periods": max_periods,
        "fee_bps": fee_bps,
        "ext_ref": "sub_it"
    }})
    .to_string()
}

pub async fn ft_transfer_call(
    ft: &Contract,
    from: &Account,
    to: &AccountId,
    amount: u128,
    msg: &str,
) -> Result<ExecutionFinalResult> {
    Ok(from
        .call(ft.id(), "ft_transfer_call")
        .args_json(json!({"receiver_id": to, "amount": amount.to_string(), "msg": msg}))
        .deposit(NearToken::from_yoctonear(1))
        .gas(Gas::from_tgas(100))
        .transact()
        .await?)
}

/// Creates a mandate funded with `funding`; returns (mandate id, the funding tx).
pub async fn create(
    ctx: &Ctx,
    kind: &str,
    funding: u128,
    max_periods: Option<u32>,
) -> Result<(u64, ExecutionFinalResult)> {
    let msg = create_msg(ctx.merchant.id(), kind, AMOUNT, FEE_BPS, max_periods);
    let res = ft_transfer_call(&ctx.ft, &ctx.alice, ctx.vault.id(), funding, &msg).await?;
    ok(&res)?;
    let evs = vault_events(&res);
    let created = evs
        .iter()
        .find(|e| e["event"] == "mandate_created")
        .ok_or_else(|| anyhow!("no mandate_created: {res:?}"))?;
    Ok((created["data"][0]["mandate_id"].as_u64().unwrap(), res))
}

pub async fn collect(ctx: &Ctx, id: u64, gas: Gas) -> Result<ExecutionFinalResult> {
    Ok(ctx
        .keeper
        .call(ctx.vault.id(), "collect")
        .args_json(json!({"mandate_id": id}))
        .gas(gas)
        .transact()
        .await?)
}

pub async fn cancel(ctx: &Ctx, who: &Account, id: u64) -> Result<ExecutionFinalResult> {
    Ok(who
        .call(ctx.vault.id(), "cancel")
        .args_json(json!({"mandate_id": id}))
        .deposit(NearToken::from_yoctonear(1))
        .gas(Gas::from_tgas(100))
        .transact()
        .await?)
}

pub async fn withdraw(ctx: &Ctx, who: &Account, id: u64) -> Result<ExecutionFinalResult> {
    Ok(who
        .call(ctx.vault.id(), "withdraw")
        .args_json(json!({"mandate_id": id}))
        .deposit(NearToken::from_yoctonear(1))
        .gas(Gas::from_tgas(100))
        .transact()
        .await?)
}

pub async fn mandate(ctx: &Ctx, id: u64) -> Result<Value> {
    Ok(ctx.vault.view("get_mandate").args_json(json!({"mandate_id": id})).await?.json()?)
}

pub fn gas_report(label: &str, res: &ExecutionFinalResult) -> u64 {
    let total = res.total_gas_burnt.as_gas();
    println!("GAS {label}: total {:.2} Tgas", total as f64 / 1e12);
    for o in res.outcomes() {
        println!("GAS   {:<40} {:>8.2} Tgas", o.executor_id.as_str(), o.gas_burnt.as_gas() as f64 / 1e12);
    }
    total
}
