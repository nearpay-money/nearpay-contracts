//! Full flows against the real wasm in a near-workspaces sandbox (LLD contracts-vault §7).

mod common;

use common::*;
use near_workspaces::types::{Gas, NearToken};
use serde_json::json;

#[tokio::test]
async fn intents_flow_create_collect_cancel_withdraw() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;
    let (share, fee) = split(AMOUNT, FEE_BPS);

    // Create, funded with 3 periods.
    let (id, res) = create(&ctx, "intents_account", 3 * AMOUNT, Some(12)).await?;
    let ev = &vault_events(&res)[0];
    assert_eq!(ev["version"], "1.0.0");
    assert_eq!(ev["data"][0]["payer"], ctx.alice.id().as_str());
    assert_eq!(ev["data"][0]["receiver_kind"], "intents_account");
    assert_eq!(ev["data"][0]["balance"], (3 * AMOUNT).to_string());
    assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 3 * AMOUNT);
    assert_eq!(ft_balance(&ctx.ft, ctx.alice.id()).await?, MINTED - 3 * AMOUNT);

    // Collect period 1 (due at start).
    let res = collect(&ctx, id, COLLECT_GAS).await?;
    ok(&res)?;
    let evs = vault_events(&res);
    assert_eq!(names(&evs), vec!["collected"]);
    assert_eq!(
        evs[0]["data"][0],
        json!({"mandate_id": id, "period": 1, "amount": AMOUNT.to_string(), "merchant_share": share.to_string(),
               "fee": fee.to_string(), "balance": (2 * AMOUNT).to_string()})
    );
    assert_eq!(intents_balance(&ctx, ctx.merchant.id()).await?, share);
    assert_eq!(ft_balance(&ctx.ft, ctx.treasury.id()).await?, fee);
    assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 2 * AMOUNT);
    assert!(res.json::<bool>()?);

    // Period 2 not due yet.
    let res = collect(&ctx, id, COLLECT_GAS).await?;
    assert_fails_with(&res, "ERR_NOT_DUE");

    // Payer leaves the token, so the cancel refund fails and the balance stays for withdraw.
    let res = ctx
        .alice
        .call(ctx.ft.id(), "ft_transfer")
        .args_json(json!({"receiver_id": ctx.bob.id(), "amount": (MINTED - 3 * AMOUNT).to_string()}))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?;
    ok(&res)?;
    let res = ctx
        .alice
        .call(ctx.ft.id(), "storage_unregister")
        .args_json(json!({}))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?;
    ok(&res)?;
    let res = cancel(&ctx, &ctx.alice, id).await?;
    assert!(res.is_success(), "{res:?}");
    let evs = vault_events(&res);
    assert_eq!(evs[0]["data"][0], json!({"mandate_id": id, "by": ctx.alice.id(), "refunded": "0"}));
    let m = mandate(&ctx, id).await?;
    assert_eq!(m["status"], "canceled");
    assert_eq!(m["balance"], (2 * AMOUNT).to_string());
    assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 2 * AMOUNT);

    // Re-register and withdraw.
    ft_register(&ctx.ft, ctx.alice.id()).await?;
    let res = withdraw(&ctx, &ctx.alice, id).await?;
    ok(&res)?;
    assert_eq!(
        vault_events(&res)[0]["data"][0],
        json!({"mandate_id": id, "amount": (2 * AMOUNT).to_string()})
    );
    assert_eq!(ft_balance(&ctx.ft, ctx.alice.id()).await?, 2 * AMOUNT);
    assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 0);
    assert!(mandate(&ctx, id).await?.is_null());
    let tvl: serde_json::Value =
        ctx.vault.view("get_token_tvl").args_json(json!({"token": ctx.ft.id()})).await?.json()?;
    assert_eq!(tvl["tvl"], "0");
    Ok(())
}

#[tokio::test]
async fn cancel_refunds_payer_directly() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;
    let (id, _) = create(&ctx, "near_account", 2 * AMOUNT, None).await?;
    let before = ft_balance(&ctx.ft, ctx.alice.id()).await?;
    // The merchant may cancel too; the refund still goes to the payer.
    let res = cancel(&ctx, &ctx.merchant, id).await?;
    ok(&res)?;
    assert_eq!(
        vault_events(&res)[0]["data"][0],
        json!({"mandate_id": id, "by": ctx.merchant.id(), "refunded": (2 * AMOUNT).to_string()})
    );
    assert_eq!(ft_balance(&ctx.ft, ctx.alice.id()).await?, before + 2 * AMOUNT);
    assert!(mandate(&ctx, id).await?.is_null());
    // Storage lock released to the payer.
    let sb: serde_json::Value = ctx
        .vault
        .view("storage_balance_of")
        .args_json(json!({"account_id": ctx.alice.id()}))
        .await?
        .json()?;
    assert_eq!(sb["available"], NearToken::from_millinear(20).as_yoctonear().to_string());
    // A stranger cannot cancel.
    let (id2, _) = create(&ctx, "near_account", AMOUNT, None).await?;
    assert_fails_with(&cancel(&ctx, &ctx.bob, id2).await?, "ERR_UNAUTHORIZED");
    Ok(())
}

#[tokio::test]
async fn near_account_receiver_and_merchant_leg_failure() -> anyhow::Result<()> {
    let ctx = setup(Setup { register_merchant: false, ..Setup::default() }).await?;
    let (share, fee) = split(AMOUNT, FEE_BPS);
    let (id, _) = create(&ctx, "near_account", 2 * AMOUNT, None).await?;

    // Merchant not registered on the token → ft_transfer fails → collect_failed, nothing moves.
    let res = collect(&ctx, id, COLLECT_GAS).await?;
    assert!(res.is_success(), "callback must succeed: {res:?}");
    let evs = vault_events(&res);
    assert_eq!(
        evs[0]["data"][0],
        json!({"mandate_id": id, "period": 1, "reason": "merchant_transfer_failed"})
    );
    assert_eq!(ft_balance(&ctx.ft, ctx.treasury.id()).await?, 0, "no fee when the merchant leg fails");
    assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 2 * AMOUNT);
    let m = mandate(&ctx, id).await?;
    assert_eq!((m["collected_periods"].as_u64(), m["in_flight"].as_bool()), (Some(0), Some(false)));
    assert_eq!(m["balance"], (2 * AMOUNT).to_string());

    // Once registered, the same period collects.
    ft_register(&ctx.ft, ctx.merchant.id()).await?;
    let res = collect(&ctx, id, COLLECT_GAS).await?;
    ok(&res)?;
    assert_eq!(vault_events(&res)[0]["data"][0]["period"], 1);
    assert_eq!(ft_balance(&ctx.ft, ctx.merchant.id()).await?, share);
    assert_eq!(ft_balance(&ctx.ft, ctx.treasury.id()).await?, fee);
    Ok(())
}

#[tokio::test]
async fn intents_receiver_refusals_restore_balance() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;
    let (id, _) = create(&ctx, "intents_account", 2 * AMOUNT, None).await?;
    for mode in ["reject", "panic"] {
        let res = ctx
            .intents
            .call("set_mode")
            .args_json(json!({"account_id": ctx.merchant.id(), "mode": mode}))
            .transact()
            .await?;
        ok(&res)?;
        let res = collect(&ctx, id, COLLECT_GAS).await?;
        assert!(res.is_success(), "{mode}: {res:?}");
        assert_eq!(names(&vault_events(&res)), vec!["collect_failed"], "{mode}");
        assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 2 * AMOUNT, "{mode}");
        assert_eq!(ft_balance(&ctx.ft, ctx.treasury.id()).await?, 0, "{mode}");
        assert_eq!(intents_balance(&ctx, ctx.merchant.id()).await?, 0, "{mode}");
    }
    // Partial use: period counted with the delivered amount, no fee.
    let res = ctx
        .intents
        .call("set_mode")
        .args_json(json!({"account_id": ctx.merchant.id(), "mode": "partial:500"}))
        .transact()
        .await?;
    ok(&res)?;
    let (share, _) = split(AMOUNT, FEE_BPS);
    let res = collect(&ctx, id, COLLECT_GAS).await?;
    ok(&res)?;
    let ev = &vault_events(&res)[0]["data"][0];
    assert_eq!(ev["merchant_share"], (share / 2).to_string());
    assert_eq!(ev["fee"], "0");
    assert_eq!(intents_balance(&ctx, ctx.merchant.id()).await?, share / 2);
    assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 2 * AMOUNT - share / 2);
    Ok(())
}

#[tokio::test]
async fn fee_leg_failure_keeps_fee_on_mandate() -> anyhow::Result<()> {
    let ctx = setup(Setup { register_treasury: false, ..Setup::default() }).await?;
    let (share, fee) = split(AMOUNT, FEE_BPS);
    let (id, _) = create(&ctx, "intents_account", 2 * AMOUNT, None).await?;
    let res = collect(&ctx, id, COLLECT_GAS).await?;
    assert!(res.is_success(), "{res:?}");
    let ev = &vault_events(&res)[0];
    assert_eq!(ev["event"], "collected");
    assert_eq!(ev["data"][0]["fee"], "0");
    assert_eq!(ev["data"][0]["amount"], share.to_string());
    assert_eq!(ev["data"][0]["balance"], (AMOUNT + fee).to_string());
    assert_eq!(intents_balance(&ctx, ctx.merchant.id()).await?, share);
    assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 2 * AMOUNT - share);
    Ok(())
}

#[tokio::test]
async fn cap_exceeded_is_refunded() -> anyhow::Result<()> {
    let ctx = setup(Setup { cap: 5 * AMOUNT, ..Setup::default() }).await?;
    let msg = create_msg(ctx.merchant.id(), "intents_account", AMOUNT, FEE_BPS, None);
    let res = ft_transfer_call(&ctx.ft, &ctx.alice, ctx.vault.id(), 6 * AMOUNT, &msg).await?;
    assert!(res.is_success(), "{res:?}");
    assert!(res.logs().contains(&"ERR_CAP_EXCEEDED"), "{:?}", res.logs());
    assert_eq!(res.json::<String>()?, "0", "ft_transfer_call reports 0 used");
    assert_eq!(ft_balance(&ctx.ft, ctx.alice.id()).await?, MINTED);
    assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 0);
    // Exactly the cap is accepted; one more unit is not (top-up).
    let (id, _) = create(&ctx, "intents_account", 5 * AMOUNT, None).await?;
    let res = ft_transfer_call(
        &ctx.ft,
        &ctx.alice,
        ctx.vault.id(),
        1,
        &json!({"top_up": {"mandate_id": id}}).to_string(),
    )
    .await?;
    assert!(res.logs().contains(&"ERR_CAP_EXCEEDED"));
    assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 5 * AMOUNT);
    Ok(())
}

#[tokio::test]
async fn missing_storage_is_refunded() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;
    // Bob holds tokens but never paid vault storage.
    let msg = create_msg(ctx.merchant.id(), "intents_account", AMOUNT, FEE_BPS, None);
    let res = ft_transfer_call(&ctx.ft, &ctx.bob, ctx.vault.id(), 2 * AMOUNT, &msg).await?;
    assert!(res.is_success());
    assert!(res.logs().contains(&"ERR_NO_STORAGE"), "{:?}", res.logs());
    assert_eq!(ft_balance(&ctx.ft, ctx.bob.id()).await?, MINTED);
    assert_eq!(ft_balance(&ctx.ft, ctx.vault.id()).await?, 0);
    Ok(())
}

#[tokio::test]
async fn disallowed_token_and_bad_msg_are_refunded() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;
    let msg = create_msg(ctx.merchant.id(), "intents_account", AMOUNT, FEE_BPS, None);
    let res = ft_transfer_call(&ctx.other_ft, &ctx.alice, ctx.vault.id(), AMOUNT, &msg).await?;
    assert!(format!("{res:?}").contains("ERR_TOKEN_NOT_ALLOWED"), "{res:?}");
    assert_eq!(ft_balance(&ctx.other_ft, ctx.alice.id()).await?, MINTED);
    assert_eq!(ft_balance(&ctx.other_ft, ctx.vault.id()).await?, 0);

    let res =
        ft_transfer_call(&ctx.ft, &ctx.alice, ctx.vault.id(), AMOUNT, r#"{"create_mandate":{}}"#).await?;
    assert!(res.logs().contains(&"ERR_BAD_MSG"));
    assert_eq!(ft_balance(&ctx.ft, ctx.alice.id()).await?, MINTED);
    Ok(())
}

#[tokio::test]
async fn exhausted_mandate_returns_leftover_and_closes() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;
    let (id, _) = create(&ctx, "near_account", AMOUNT + 7, Some(1)).await?;
    let res = collect(&ctx, id, COLLECT_GAS).await?;
    ok(&res)?;
    assert_eq!(names(&vault_events(&res)), vec!["collected", "exhausted"]);
    assert_fails_with(&collect(&ctx, id, COLLECT_GAS).await?, "ERR_MAX_PERIODS");
    assert_fails_with(&withdraw(&ctx, &ctx.bob, id).await?, "ERR_UNAUTHORIZED");
    let res = withdraw(&ctx, &ctx.alice, id).await?;
    ok(&res)?;
    assert_eq!(vault_events(&res)[0]["data"][0]["amount"], "7");
    assert!(mandate(&ctx, id).await?.is_null());
    Ok(())
}

#[tokio::test]
async fn entry_point_guards() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;
    let (id, _) = create(&ctx, "intents_account", 2 * AMOUNT, None).await?;
    // Callbacks are private.
    for (method, args) in [
        ("on_merchant_leg", json!({"mandate_id": id, "share": "1", "fee": "0"})),
        ("on_collected", json!({"mandate_id": id, "share": "1", "fee": "0"})),
        ("on_refunded", json!({"mandate_id": id, "amount": "1", "canceled_by": null})),
    ] {
        let res = ctx.alice.call(ctx.vault.id(), method).args_json(args).transact().await?;
        assert_fails_with(&res, "is private");
    }
    // Constructor runs once.
    let res = ctx
        .vault
        .call("new")
        .args_json(json!({"allowed_tokens": [ctx.other_ft.id()], "fee_receiver": ctx.bob.id(), "max_fee_bps": 10000,
                          "tvl_caps": [{"token": ctx.other_ft.id(), "cap": "1"}], "intents_account": ctx.bob.id()}))
        .transact()
        .await?;
    assert!(res.is_failure(), "re-init must fail: {res:?}");
    // collect does not accept deposits; cancel/withdraw require exactly 1 yocto.
    let res = ctx
        .keeper
        .call(ctx.vault.id(), "collect")
        .args_json(json!({"mandate_id": id}))
        .deposit(NearToken::from_yoctonear(1))
        .gas(COLLECT_GAS)
        .transact()
        .await?;
    assert_fails_with(&res, "doesn't accept deposit");
    let res =
        ctx.alice.call(ctx.vault.id(), "cancel").args_json(json!({"mandate_id": id})).transact().await?;
    assert_fails_with(&res, "yoctoNEAR");
    // No admin surface: these methods do not exist.
    for method in ["set_fee_receiver", "set_tvl_cap", "migrate", "upgrade", "owner_withdraw"] {
        let res = ctx.alice.call(ctx.vault.id(), method).args_json(json!({})).transact().await?;
        assert_fails_with(&res, "MethodNotFound");
    }
    // Config unchanged.
    let cfg: serde_json::Value = ctx.vault.view("get_config").await?.json()?;
    assert_eq!(cfg["fee_receiver"], ctx.treasury.id().as_str());
    assert_eq!(cfg["max_fee_bps"], 500);
    Ok(())
}

#[tokio::test]
async fn under_gassed_collect_reverts_cleanly() -> anyhow::Result<()> {
    let ctx = setup(Setup::default()).await?;
    let (id, _) = create(&ctx, "intents_account", 2 * AMOUNT, None).await?;
    let res = collect(&ctx, id, Gas::from_tgas(40)).await?;
    assert!(res.is_failure(), "{res:?}");
    let m = mandate(&ctx, id).await?;
    assert_eq!(m["in_flight"], false);
    assert_eq!(m["balance"], (2 * AMOUNT).to_string());
    // And a properly gassed collect then works.
    ok(&collect(&ctx, id, COLLECT_GAS).await?)?;
    Ok(())
}
