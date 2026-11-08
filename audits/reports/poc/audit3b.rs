//! Audit 3, re-check 2 (42cff4f..9f75067). Scratch copy only.
use super::*;

const RELAYER: &str = "relayer.test";

/// NEP-366 meta-transaction shape, per nearcore `apply_delegate_action`: the inner receipt has
/// predecessor = the delegating user (alice), but signer_id = the relayer (outer tx signer).
/// The token therefore reports sender_id = alice while env::signer_account_id() = relayer.
/// The v1.5 signer rule refuses it, contrary to the code comment and LLD §3.1.
#[test]
fn poc_meta_tx_deposit_is_refused() {
    let mut s = Sim::new();
    s.register(ALICE, 2 * MILLINEAR + 10 * MILLINEAR);
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string();
    let o = s.ft_on_transfer_signed(USDC, ALICE, RELAYER, 3 * AMOUNT, &msg);
    assert_eq!(o.result, Ok(3 * AMOUNT));
    assert_eq!(o.logs, vec![ERR_UNSUPPORTED_SENDER.to_string()]);
}

/// Exits via meta-tx still work (they check predecessor, not signer).
#[test]
fn probe_meta_tx_cancel_still_works() {
    let mut s = Sim::new();
    let id = s.create_default();
    let o = s.call_signed(ALICE, RELAYER, 1, vec![], move |v| { let _ = v.cancel(id); });
    assert!(o.result.is_ok());
    assert_eq!(s.mandate(id).unwrap().status, Status::Canceled);
}
