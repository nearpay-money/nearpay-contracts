# Audits — subscription vault

This folder is for the **independent AI-agent audits** (ADR-0020 decision 6): each audit is a
separate agent run that did not write the contract, works against
`nearpay-brain/lld/contracts-vault.md` (v1.5) V1–V10 and threat T-10, and records every finding
with its fix or an accepted-risk note. Audit reports go here as `YYYY-MM-DD-<auditor>.md`.
Nothing in this file is an audit verdict.

**Audits so far:** [`2026-11-08-ai-audits.md`](2026-11-08-ai-audits.md) — two AI audits and a re-audit, findings → fixes / accepted risks.

## Scope

| Item | Path |
|---|---|
| Contract | `contracts/subscription-vault/src/` (`lib.rs` methods and callbacks, `msg.rs` parsing/validation, `math.rs` arithmetic, `events.rs`, `errors.rs`, `types.rs`) |
| Unit / property / native fuzz tests | `contracts/subscription-vault/src/tests/` (`unit.rs`, `invariants.rs`, `msg_fuzz.rs`, `audit_fixes.rs`; harness in `mod.rs`) and `src/math.rs` (`mod tests`) |
| Sandbox tests + gas profile | `integration-tests/tests/` (`vault_flows.rs`, `min_gas.rs`, `gas_profile.rs`) against `contracts/mocks/` |
| cargo-fuzz target | `fuzz/fuzz_targets/parse_msg.rs` |
| Build / CI | `scripts/build.sh`, `.github/workflows/ci.yml`, `[package.metadata.near.reproducible_build]` in `contracts/subscription-vault/Cargo.toml` |

## Invariants and where they are tested

`P` = property test `invariants::invariants_hold_under_random_interleavings` (random interleavings
of create, top-up, collect, delayed/failed/partial/garbage callback delivery, cancel by payer /
merchant / stranger, withdraw, time travel and storage withdraw, checked after every step against
a token-ledger model, followed by a drain) and the fixed schedule
`invariants::everything_in_flight_then_reverse_delivery`.

| Invariant | Statement (LLD §5) | Tests |
|---|---|---|
| **V1** solvency | Σ balances + in-flight collects + in-flight refunds = `tvl[token]` ≤ tokens held; between receipts of one collect, tokens already sent are subtracted | P (`check`: tvl accounting, V1, "unexplained tokens"); unit `cancel_during_collect_never_refunds_in_flight_amount`, `collect_*` (tvl asserted after every outcome); sandbox `intents_flow_create_collect_cancel_withdraw`, `intents_receiver_refusals_restore_balance`, `fee_leg_failure_keeps_fee_on_mandate` (real token balances) |
| **V2** payer exit | an active mandate can always be canceled by its payer and its balance recovered (withdraw after a failed refund) | P (every cancel/withdraw attempt asserts the documented outcome; `drain` must close every mandate, return every unspent token and allow `storage_unregister`); unit `cancel_refund_failure_leaves_balance_for_withdraw`, `withdraw_*`; sandbox `intents_flow_create_collect_cancel_withdraw` (refund fails, then withdraw), `cancel_refunds_payer_directly`, `exhausted_mandate_returns_leftover_and_closes` |
| **V3** rate limit | per mandate, paid ≤ `collected_periods × amount_per_period`; `collected_periods` ≤ elapsed periods | P (`check`); unit `collect_success_with_fee` (period 2 not due one ns early), `late_collects_catch_up_one_period_per_call`, `collect_partial_delivery_counts_period_without_fee`; `math::tests::collected_never_exceeds_elapsed_when_due`, `due_at_is_monotonic`, `split_is_exact_floor` |
| **V4** recipients | tokens only to payer, merchant receiver (direct, or `intents_account` with `msg` = merchant) or fee receiver; NEAR only to the storage account owner | P (every created receipt is decoded and its receiver/args asserted per context); unit `collect_intents_receipts`, `collect_near_account_receipts`, `collect_success_with_fee`, `cancel_by_payer_refunds_and_closes`, `cancel_by_merchant_refunds_payer`, `storage_*`; sandbox `entry_point_guards` (no admin/setter/upgrade methods exist) |
| **V5** immutability | merchant receiver, kind, token, payer, amount, period, start, max periods, fee bps, ext_ref never change; no setters | P (`check` compares terms with the creation snapshot); sandbox `entry_point_guards` (config unchanged, re-init fails) |
| **V6** flight flags | `in_flight` ⇔ one collect awaiting callbacks; `refund_in_flight` = pending refund | P; unit `collect_preconditions` (`ERR_IN_FLIGHT`), `withdraw_errors` (`ERR_IN_FLIGHT` during refund), `stray_callbacks_change_nothing` |
| **V7** caps | `tvl ≤ tvl_cap` per token | P; unit `create_over_tvl_cap_is_refunded`, `top_up_rules`; sandbox `cap_exceeded_is_refunded` |
| **V8** storage | payer lock count = open mandates = index entries; locks cover worst-case bytes | P; unit `storage_cost_covers_worst_case_mandate`, `storage_unregister_rules`, `exactly_funded_mandate_is_removed_when_exhausted`, `views_by_payer_and_next_collect`; sandbox `missing_storage_is_refunded`, `cancel_refunds_payer_directly` |
| **V10** per-mandate cap (v1.3) | per mandate, balance + in-flight collect + in-flight refund ≤ the token's `mandate_cap`, and `amount_per_period ≤ mandate_cap` | P (`check`; amounts above the cap are generated, top-ups push mandates to the cap); unit `create_over_mandate_cap_is_refunded`, `top_up_respects_mandate_cap_including_in_flight`, `new_rejects_bad_config` (`mandate_cap > cap`); sandbox `mandate_cap_is_enforced` |
| **V9** collect rule | `collect` succeeds iff preconditions hold, else the documented code in the documented order | P (`check_collect_rule` on every collect); unit `collect_preconditions`, `every_error_string_is_reachable` |

## Other properties worth an auditor's attention

| Property | Tests |
|---|---|
| Fee leg is never sent unless the merchant share was fully delivered | unit `collect_merchant_leg_failure_restores_balance`, `collect_partial_delivery_counts_period_without_fee`; P (asserted on every fee-leg receipt); sandbox `near_account_receiver_and_merchant_leg_failure`, `intents_receiver_refusals_restore_balance` |
| Failed calls change nothing (all checks before any write) | harness reverts storage on panic and the tests assert state after each rejected call (`assert_rejected`); sandbox `under_gassed_collect_reverts_cleanly` |
| `ft_on_transfer` is all-or-nothing for any `msg` | `msg_fuzz::ft_on_transfer_fuzz_is_all_or_nothing`, unit `create_rejects_bad_msg_with_full_refund` |
| Parser accepts exactly the schema | `msg_fuzz::parse_matches_oracle` (independent oracle), `parse_never_panics_*`, `duplicate_and_case_variants_are_rejected`; cargo-fuzz `parse_msg` (CI) |
| Callbacks are private, init once, collect non-payable, cancel/withdraw need 1 yocto | unit `callbacks_are_private`; sandbox `entry_point_guards` (wasm entry-point guards) |
| Stable error strings and event wire format | unit `constants_match_lld`, `event_wire_format_is_exact`, exact-log assertions in `create_mandate_happy_path`, `top_up_rules`, `collect_success_with_fee`, `collect_merchant_leg_failure_restores_balance`, `cancel_by_payer_refunds_and_closes`, `cancel_refund_failure_leaves_balance_for_withdraw`; `every_error_string_is_reachable` |
| Gas | sandbox `gas_profile` (burnt gas per method; `collect` ≤ 60 Tgas burnt excluding the receiver); `min_gas.rs` (every path at the minimum accepted gas, callbacks succeed, flags cleared); unit `audit_fixes::collect_gas_floor_and_static_budgets` |
| Only directly signed deposits fund mandates (payer = transaction signer; no relayed `sender_id`) | unit `audit_fixes::relayed_deposit_from_intents_account_is_refunded`, `relayed_deposit_from_any_contract_is_refunded`; sandbox `deposit_relayed_by_intents_account_is_refunded` |
| Exits need `MIN_EXIT_GAS` (clear `ERR_GAS` below) | unit `audit_fixes::exits_need_min_exit_gas`; sandbox `min_gas::closing_paths_at_minimum_gas_release_everything`, `vault_flows::entry_point_guards` |
| Input bounds (int4/JS-safe numbers) | unit `audit_fixes::inputs_are_bounded`; `msg_fuzz` (`assert_validated`); cargo-fuzz |
| Callback failure is unrecoverable (documented state) | unit `audit_fixes::failed_collect_callback_leaves_the_period_stuck`, `failed_refund_callback_blocks_withdraw_forever` |

## Known limits of the test setup

- Locally only native unit/property tests run; wasm builds, sandbox tests, the gas profile, the
  reproducible build and cargo-fuzz run in CI only.
- The mock token forwards gas like near-contract-standards 4.x (`prepaid − 30 Tgas`) and the mock
  intents receiver burns 30 Tgas per deposit, but neither is the real contract. Real USDC/USDT and
  `intents.near` gas use is not measured before mainnet (see the 2026-11-08 audit record).
- Unit tests call the Rust methods directly, so near-sdk's generated guards (`#[private]`,
  `#[payable]`, init-once) are covered only by the sandbox tests; the method bodies repeat the
  private check as defence in depth.
