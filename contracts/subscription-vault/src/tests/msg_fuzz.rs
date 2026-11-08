//! Native fuzzing of `msg` parsing (cargo-fuzz needs nightly; `fuzz/` holds the libFuzzer
//! target that CI runs). Three layers:
//! 1. arbitrary bytes/strings → `parse_msg` never panics; accepted messages satisfy the schema;
//! 2. structure-aware mutations of valid messages, checked against an independent oracle;
//! 3. end-to-end `ft_on_transfer` with fuzzed `msg`: either exactly one `mandate_created` /
//!    `topped_up` event and `0` returned, or exactly one stable error log and a full refund.

use super::*;
use crate::msg::{VaultMsg, parse_msg, validate_create};
use proptest::prelude::*;

const KEYS: [&str; 8] = [
    "merchant_receiver",
    "receiver_kind",
    "amount_per_period",
    "period_secs",
    "start_at",
    "max_periods",
    "fee_bps",
    "ext_ref",
];
const REQUIRED: [&str; 6] =
    ["merchant_receiver", "receiver_kind", "amount_per_period", "period_secs", "fee_bps", "ext_ref"];

/// Independent schema oracle for the parse step (not validation).
fn oracle_parses(v: &Value) -> bool {
    let Some(top) = v.as_object() else { return false };
    if top.len() != 1 {
        return false;
    }
    let (k, inner) = top.iter().next().unwrap();
    let Some(obj) = inner.as_object() else { return false };
    match k.as_str() {
        "top_up" => obj.len() == 1 && obj.get("mandate_id").and_then(Value::as_u64).is_some(),
        "create_mandate" => {
            if obj.keys().any(|k| !KEYS.contains(&k.as_str()))
                || REQUIRED.iter().any(|r| !obj.contains_key(*r))
            {
                return false;
            }
            let s = |k: &str| obj.get(k).and_then(Value::as_str);
            let account_ok = s("merchant_receiver").is_some_and(|a| a.parse::<AccountId>().is_ok());
            let kind_ok = matches!(s("receiver_kind"), Some("intents_account" | "near_account"));
            let amount_ok = s("amount_per_period").is_some_and(|a| a.parse::<u128>().is_ok());
            let period_ok = obj["period_secs"].as_u64().is_some();
            let start_ok = match obj.get("start_at") {
                None | Some(Value::Null) => true,
                Some(v) => v.as_str().is_some_and(|a| a.parse::<u64>().is_ok()),
            };
            let max_ok = match obj.get("max_periods") {
                None | Some(Value::Null) => true,
                Some(v) => v.as_u64().is_some_and(|n| n <= u64::from(u32::MAX)),
            };
            let fee_ok = obj["fee_bps"].as_u64().is_some_and(|n| n <= u64::from(u16::MAX));
            let ref_ok = s("ext_ref").is_some();
            account_ok && kind_ok && amount_ok && period_ok && start_ok && max_ok && fee_ok && ref_ok
        }
        _ => false,
    }
}

fn json_scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        any::<u64>().prop_map(|n| json!(n)),
        (-1e20f64..1e20).prop_map(|f| json!(f)),
        "[a-z0-9._+-]{0,70}".prop_map(Value::String),
        any::<String>().prop_map(Value::String),
        Just(json!([])),
        Just(json!({})),
    ]
}

fn field_value(key: &'static str) -> impl Strategy<Value = Value> {
    let valid: BoxedStrategy<Value> = match key {
        "merchant_receiver" => prop_oneof![
            Just(json!(MERCHANT)),
            Just(json!(VAULT)),
            "[a-z0-9]{2,20}\\.near".prop_map(Value::String),
            "[a-f0-9]{64}".prop_map(Value::String)
        ]
        .boxed(),
        "receiver_kind" => prop_oneof![Just(json!("intents_account")), Just(json!("near_account"))].boxed(),
        "amount_per_period" => {
            prop_oneof![any::<u128>().prop_map(|n| json!(n.to_string())), Just(json!("0")), Just(json!("+5"))]
                .boxed()
        }
        "period_secs" => {
            prop_oneof![any::<u64>().prop_map(|n| json!(n)), (86_000u64..90_000).prop_map(|n| json!(n))]
                .boxed()
        }
        "start_at" => prop_oneof![any::<u64>().prop_map(|n| json!(n.to_string())), Just(Value::Null)].boxed(),
        "max_periods" => {
            prop_oneof![(0u32..5).prop_map(|n| json!(n)), Just(Value::Null), Just(json!(u32::MAX))].boxed()
        }
        "fee_bps" => (0u16..1000).prop_map(|n| json!(n)).boxed(),
        _ => prop_oneof!["[ -~]{0,70}".prop_map(Value::String), any::<String>().prop_map(Value::String)]
            .boxed(),
    };
    prop_oneof![4 => valid, 1 => json_scalar()]
}

fn create_value() -> impl Strategy<Value = Value> {
    let fields: Vec<BoxedStrategy<Option<(String, Value)>>> = KEYS
        .iter()
        .map(|k| {
            let k: &'static str = k;
            prop_oneof![9 => field_value(k).prop_map(move |v| Some((k.to_string(), v))), 1 => Just(None)]
                .boxed()
        })
        .collect();
    (fields, prop::option::weighted(0.1, ("[a-z_]{1,12}", json_scalar())), prop::bool::weighted(0.95))
        .prop_map(|(fields, extra, tagged)| {
            let mut obj = serde_json::Map::new();
            for (k, v) in fields.into_iter().flatten() {
                obj.insert(k, v);
            }
            if let Some((k, v)) = extra {
                obj.insert(k, v);
            }
            if tagged { json!({ "create_mandate": Value::Object(obj) }) } else { Value::Object(obj) }
        })
}

fn any_msg_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        6 => create_value(),
        2 => prop_oneof![any::<u64>().prop_map(|n| json!({"top_up": {"mandate_id": n}})), json_scalar().prop_map(|v| json!({"top_up": {"mandate_id": v}}))],
        1 => json_scalar(),
        1 => (json_scalar(), json_scalar()).prop_map(|(a, b)| json!({"create_mandate": a, "top_up": b})),
    ]
}

fn assert_validated(m: &msg::CreateMandateMsg) {
    let vault = acc(VAULT);
    match validate_create(m, MAX_FEE_BPS, &vault) {
        Ok(()) => {
            assert!(m.amount_per_period.0 > 0);
            assert!(m.period_secs >= msg::MIN_PERIOD_SECS);
            assert_ne!(m.max_periods, Some(0));
            assert!(msg::ext_ref_is_valid(&m.ext_ref));
            assert_ne!(m.merchant_receiver, vault);
            assert!(m.fee_bps <= MAX_FEE_BPS);
            assert!(math::split(m.amount_per_period.0, m.fee_bps).0 > 0);
        }
        Err(code) => assert!(code == ERR_BAD_MSG || code == ERR_FEE_TOO_HIGH, "{code}"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse::<u32>().ok()).map_or(2_000, |c| c * 20),
        .. ProptestConfig::default()
    })]

    #[test]
    fn parse_never_panics_on_arbitrary_strings(s in any::<String>()) {
        if let Ok(VaultMsg::CreateMandate(m)) = parse_msg(&s) {
            assert_validated(&m);
        }
    }

    #[test]
    fn parse_never_panics_on_arbitrary_bytes(b in prop::collection::vec(any::<u8>(), 0..512)) {
        let s = String::from_utf8_lossy(&b);
        let _ = parse_msg(&s);
    }

    #[test]
    fn parse_matches_oracle(v in any_msg_value()) {
        let text = v.to_string();
        let parsed = parse_msg(&text);
        prop_assert_eq!(parsed.is_ok(), oracle_parses(&v) && text.len() <= msg::MAX_MSG_LEN, "msg {}", text);
        match parsed {
            Ok(VaultMsg::CreateMandate(m)) => assert_validated(&m),
            Ok(VaultMsg::TopUp(t)) => prop_assert_eq!(Some(t.mandate_id), v["top_up"]["mandate_id"].as_u64()),
            Err(code) => prop_assert_eq!(code, ERR_BAD_MSG),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(128),
        .. ProptestConfig::default()
    })]

    #[test]
    fn ft_on_transfer_fuzz_is_all_or_nothing(v in any_msg_value(), amount in 1u128..(2 * CAP)) {
        let mut s = Sim::new();
        s.register(ALICE, 12 * MILLINEAR);
        let id = s.create_default(); // so top_up {mandate_id: 0} can succeed
        let tvl_before = s.tvl(USDC);
        let o = s.ft_on_transfer(USDC, ALICE, amount, &v.to_string());
        let unused = o.result.clone().unwrap();
        prop_assert!(o.receipts.is_empty());
        prop_assert_eq!(o.logs.len(), 1);
        if unused == 0 {
            let ev = &o.events()[0];
            let name = ev["event"].as_str().unwrap();
            prop_assert!(name == "mandate_created" || name == "topped_up");
            prop_assert_eq!(s.tvl(USDC), tvl_before + amount);
            if name == "topped_up" {
                prop_assert_eq!(ev["data"][0]["mandate_id"].as_u64(), Some(id));
            }
        } else {
            prop_assert_eq!(unused, amount);
            prop_assert!(errors::ALL.contains(&o.logs[0].as_str()), "{:?}", o.logs);
            prop_assert_eq!(s.tvl(USDC), tvl_before);
        }
    }
}

#[test]
fn duplicate_and_case_variants_are_rejected() {
    let ok = r#"{"create_mandate":{"merchant_receiver":"m.near","receiver_kind":"near_account","amount_per_period":"5","period_secs":86400,"fee_bps":0,"ext_ref":""}}"#;
    assert!(parse_msg(ok).is_ok());
    for bad in [
        ok.replace(r#""fee_bps":0"#, r#""fee_bps":0,"fee_bps":1"#),
        ok.replace("create_mandate", "Create_Mandate"),
        ok.replace("create_mandate", "createMandate"),
        ok.replace("near_account", "NearAccount"),
        ok.replace(r#""period_secs":86400"#, r#""period_secs":86400.0"#),
        ok.replace(r#""period_secs":86400"#, r#""period_secs":8.64e4"#),
        format!("{ok}{ok}"),
        format!("{ok} trailing"),
        ok.replacen('{', "[", 1),
    ] {
        assert_eq!(parse_msg(&bad), Err(ERR_BAD_MSG), "{bad}");
    }
    // Whitespace is fine.
    assert!(parse_msg(&format!("  {ok}\n")).is_ok());
}
