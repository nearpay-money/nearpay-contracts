//! libFuzzer target: `parse_msg` + `validate_create` never panic, and every message they accept
//! satisfies the LLD rules.
#![no_main]

use libfuzzer_sys::fuzz_target;
use subscription_vault::math::split;
use subscription_vault::msg::{MIN_PERIOD_SECS, VaultMsg, ext_ref_is_valid, parse_msg, validate_create};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let Ok(VaultMsg::CreateMandate(m)) = parse_msg(text) else { return };
    let vault = "vault.near".parse().unwrap();
    for max_fee_bps in [0u16, 500, 10_000] {
        match validate_create(&m, max_fee_bps, &vault) {
            Ok(()) => {
                assert!(m.amount_per_period.0 > 0);
                assert!(m.period_secs >= MIN_PERIOD_SECS);
                assert_ne!(m.max_periods, Some(0));
                assert!(ext_ref_is_valid(&m.ext_ref));
                assert_ne!(m.merchant_receiver, vault);
                assert!(m.fee_bps <= max_fee_bps);
                let (share, fee) = split(m.amount_per_period.0, m.fee_bps);
                assert!(share > 0);
                assert_eq!(share + fee, m.amount_per_period.0);
            }
            Err(code) => assert!(code == "ERR_BAD_MSG" || code == "ERR_FEE_TOO_HIGH"),
        }
    }
});
