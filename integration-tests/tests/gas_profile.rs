//! Gas profile per method (LLD contracts-vault §7: `collect` ≤ 60 Tgas burnt in total).
//! Run with `--nocapture`; CI copies the `GAS` lines into the job summary.
//!
//! Burnt gas (what the keeper pays) differs from the gas that must be *attached*: `collect`
//! reserves static gas for the token's `ft_transfer_call` (> 30 Tgas, NEP-141 reference
//! implementations) and for its callbacks, so the keeper attaches [`COLLECT_GAS`]; unused gas is
//! refunded.

mod common;

use common::*;
use near_workspaces::types::{Gas, NearToken};
use serde_json::json;

const COLLECT_BURNT_TARGET: u64 = 60_000_000_000_000;

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
    let collect_intents = gas_report("collect(intents_account)", &res);

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

    // Minimum attached gas that still collects (informational, 10 Tgas steps).
    let (probe, _) = create(&ctx, "intents_account", 3 * AMOUNT, None).await?;
    let mut min_ok = None;
    for tgas in (60..=150).step_by(10) {
        let res = collect(&ctx, probe, Gas::from_tgas(tgas)).await?;
        if res.is_success() && names(&vault_events(&res)) == vec!["collected"] {
            min_ok = Some(tgas);
            break;
        }
    }
    println!("GAS collect(intents_account) minimum attached gas that succeeds: {min_ok:?} Tgas");

    println!(
        "GAS collect burnt: intents {:.2} Tgas, near {:.2} Tgas (target ≤ 60)",
        collect_intents as f64 / 1e12,
        collect_near as f64 / 1e12
    );
    assert!(collect_intents <= COLLECT_BURNT_TARGET, "collect(intents) burnt {collect_intents} > 60 Tgas");
    assert!(collect_near <= COLLECT_BURNT_TARGET, "collect(near) burnt {collect_near} > 60 Tgas");
    assert!(min_ok.is_some(), "collect never succeeded with ≤ 150 Tgas attached");
    Ok(())
}
