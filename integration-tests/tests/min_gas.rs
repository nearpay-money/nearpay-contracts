//! Every keeper/payer path at the minimum gas the vault accepts (AI audits A1-M-1, A1-L-1,
//! A2-M-5, A2-L-5). The mock token forwards gas like near-contract-standards 4.x
//! (`prepaid − 30 Tgas` to the receiver) and the mock intents receiver burns 30 Tgas per deposit,
//! so a starved merchant leg would show up as `collect_failed`, and a starved callback as a
//! failed receipt with `in_flight` / `refund_in_flight` left set.

mod common;

use common::*;
use near_workspaces::types::Gas;
use serde_json::json;

/// Every receipt executed by the vault (the method and its callbacks) succeeded.
fn callbacks_ok(res: &near_workspaces::result::ExecutionFinalResult, vault: &near_workspaces::AccountId) {
    for o in res.receipt_outcomes() {
        if &o.executor_id == vault {
            assert!(o.is_success(), "vault receipt failed: {res:?}");
        }
    }
}

#[tokio::test]
async fn collect_at_minimum_gas_settles() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;
    let (intents_id, _) = create(&ctx, "intents_account", 3 * AMOUNT, None).await?;
    let (near_id, _) = create(&ctx, "near_account", 3 * AMOUNT, None).await?;

    // One Tgas below the floor is refused before anything happens.
    let res = collect(&ctx, intents_id, Gas::from_tgas(MIN_COLLECT_GAS.as_tgas() - 1)).await?;
    assert_fails_with(&res, "ERR_GAS");

    // Exactly the floor: the 30 Tgas receiver still gets its deposit, both callbacks succeed.
    for id in [intents_id, near_id] {
        let res = collect(&ctx, id, MIN_COLLECT_GAS).await?;
        callbacks_ok(&res, ctx.vault.id());
        assert_eq!(names(&vault_events(&res)), vec!["collected"], "{res:?}");
        let m = mandate(&ctx, id).await?;
        assert_eq!(m["in_flight"], false);
        assert_eq!(m["collected_periods"], 1);
    }
    let (share, _) = split(AMOUNT, FEE_BPS);
    assert_eq!(intents_balance(&ctx, ctx.merchant.id()).await?, share);
    Ok(())
}

#[tokio::test]
async fn heavier_receiver_still_fits_the_merchant_leg_budget() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;
    // 60 Tgas of receiver work, within the guaranteed 70 Tgas (100 static − 30 forwarded-away).
    ok(&ctx.intents.call("set_burn_tgas").args_json(json!({"tgas": 60})).transact().await?)?;
    let (id, _) = create(&ctx, "intents_account", 2 * AMOUNT, None).await?;
    let res = collect(&ctx, id, MIN_COLLECT_GAS).await?;
    callbacks_ok(&res, ctx.vault.id());
    assert_eq!(names(&vault_events(&res)), vec!["collected"], "{res:?}");
    Ok(())
}

#[tokio::test]
async fn closing_paths_at_minimum_gas_release_everything() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;

    // Exhausting collect that also removes the mandate (worst-case on_collected).
    let (id, _) = create(&ctx, "intents_account", AMOUNT, Some(1)).await?;
    let res = collect(&ctx, id, MIN_COLLECT_GAS).await?;
    callbacks_ok(&res, ctx.vault.id());
    assert_eq!(names(&vault_events(&res)), vec!["collected", "exhausted"]);
    assert!(mandate(&ctx, id).await?.is_null());

    // Cancel: find the least gas that is accepted; every lower attempt must change nothing, and
    // the accepted one must settle completely (refund done, mandate removed, TVL back to 0).
    let (id, _) = create(&ctx, "near_account", 2 * AMOUNT, None).await?;
    let mut accepted = None;
    for tgas in (20..=100).step_by(5) {
        let res = ctx
            .alice
            .call(ctx.vault.id(), "cancel")
            .args_json(json!({"mandate_id": id}))
            .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
            .gas(Gas::from_tgas(tgas))
            .transact()
            .await?;
        if res.is_success() {
            callbacks_ok(&res, ctx.vault.id());
            assert_eq!(vault_events(&res)[0]["data"][0]["refunded"], (2 * AMOUNT).to_string());
            accepted = Some(tgas);
            break;
        }
        let m = mandate(&ctx, id).await?;
        assert_eq!(
            (m["status"].as_str(), m["refund_in_flight"].as_str()),
            (Some("active"), Some("0")),
            "{tgas}"
        );
    }
    println!("GAS cancel minimum accepted: {accepted:?} Tgas");
    assert!(accepted.is_some());
    assert!(mandate(&ctx, id).await?.is_null());

    // Withdraw of an exhausted mandate's leftover, at its minimum accepted gas.
    let (id, _) = create(&ctx, "near_account", AMOUNT + 3, Some(1)).await?;
    ok(&collect(&ctx, id, COLLECT_GAS).await?)?;
    let mut accepted = None;
    for tgas in (20..=100).step_by(5) {
        let res = ctx
            .alice
            .call(ctx.vault.id(), "withdraw")
            .args_json(json!({"mandate_id": id}))
            .deposit(near_workspaces::types::NearToken::from_yoctonear(1))
            .gas(Gas::from_tgas(tgas))
            .transact()
            .await?;
        if res.is_success() {
            callbacks_ok(&res, ctx.vault.id());
            assert_eq!(names(&vault_events(&res)), vec!["withdrawn"]);
            accepted = Some(tgas);
            break;
        }
        assert_eq!(mandate(&ctx, id).await?["refund_in_flight"], "0", "{tgas}");
    }
    println!("GAS withdraw minimum accepted: {accepted:?} Tgas");
    assert!(accepted.is_some());
    assert!(mandate(&ctx, id).await?.is_null());
    let tvl: serde_json::Value =
        ctx.vault.view("get_token_tvl").args_json(json!({"token": ctx.ft.id()})).await?.json()?;
    assert_eq!(tvl["tvl"], "0");
    Ok(())
}
