//! Gas profile per method. Run with `--nocapture`; CI copies the `GAS` lines into the job
//! summary.
//!
//! LLD contracts-vault v1.4 §3.3: the "`collect` ≤ 60 Tgas" target covers the gas the vault's
//! design causes — the vault's own receipts and the token's — and excludes the merchant receiver's
//! `ft_on_transfer` (the Verifier's cost, emulated by the mock as 30 Tgas). Attached gas is a
//! different number: the keeper attaches [`COLLECT_GAS`] (250 Tgas; at least
//! [`MIN_COLLECT_GAS`]), and unused gas is refunded.

mod common;

use common::*;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use serde_json::json;

const COLLECT_BURNT_TARGET: u64 = 60_000_000_000_000;

/// Burnt gas without the receipts executed by `exclude` (the merchant receiver).
fn burnt_excluding(res: &ExecutionFinalResult, exclude: &near_workspaces::AccountId) -> u64 {
    let receiver: u64 = res
        .receipt_outcomes()
        .iter()
        .filter(|o| &o.executor_id == exclude)
        .map(|o| o.gas_burnt.as_gas())
        .sum();
    res.total_gas_burnt.as_gas() - receiver
}

#[tokio::test]
async fn gas_profile() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;

    let res = ctx
        .bob
        .call(ctx.vault.id(), "storage_deposit")
        .args_json(json!({}))
        .deposit(NearToken::from_millinear(12))
        .transact()
        .await?;
    ok(&res)?;
    gas_report("storage_deposit", &res);

    let (intents_id, res) = create(&ctx, "intents_account", 3 * AMOUNT, None).await?;
    gas_report("ft_transfer_call(create_mandate)", &res);
    let (near_id, _) = create(&ctx, "near_account", 3 * AMOUNT, None).await?;

    let top = json!({"top_up": {"mandate_id": intents_id}}).to_string();
    let res = ft_transfer_call(&ctx.ft, &ctx.alice, ctx.vault.id(), AMOUNT, &top).await?;
    ok(&res)?;
    gas_report("ft_transfer_call(top_up)", &res);

    let res = collect(&ctx, intents_id, COLLECT_GAS).await?;
    ok(&res)?;
    assert_eq!(names(&vault_events(&res)), vec!["collected"]);
    gas_report("collect(intents_account)", &res);
    let collect_intents = burnt_excluding(&res, ctx.intents.id());

    let res = collect(&ctx, near_id, COLLECT_GAS).await?;
    ok(&res)?;
    let collect_near = gas_report("collect(near_account)", &res);

    let res = collect(&ctx, near_id, COLLECT_GAS).await?;
    assert_fails_with(&res, "ERR_NOT_DUE");
    gas_report("collect(ERR_NOT_DUE)", &res);

    let res = cancel(&ctx, &ctx.alice, near_id).await?;
    ok(&res)?;
    gas_report("cancel", &res);

    // withdraw path: an exhausted mandate with leftover.
    let msg = create_msg(ctx.merchant.id(), "near_account", AMOUNT, FEE_BPS, Some(1));
    let res = ft_transfer_call(&ctx.ft, &ctx.alice, ctx.vault.id(), AMOUNT + 1, &msg).await?;
    ok(&res)?;
    let id = vault_events(&res)[0]["data"][0]["mandate_id"].as_u64().unwrap();
    ok(&collect(&ctx, id, COLLECT_GAS).await?)?;
    let res = withdraw(&ctx, &ctx.alice, id).await?;
    ok(&res)?;
    gas_report("withdraw", &res);

    println!(
        "GAS collect burnt (excluding the receiver's ft_on_transfer): intents {:.2} Tgas, near {:.2} Tgas (target ≤ 60)",
        collect_intents as f64 / 1e12,
        collect_near as f64 / 1e12
    );
    assert!(collect_intents <= COLLECT_BURNT_TARGET, "collect(intents) burnt {collect_intents} > 60 Tgas");
    assert!(collect_near <= COLLECT_BURNT_TARGET, "collect(near) burnt {collect_near} > 60 Tgas");
    Ok(())
}
