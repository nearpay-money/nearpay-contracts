# Audit 3: subscription vault. Re-audit of the fix diff `44637ed..42cff4f`

- **Target:** `nearpay-contracts` branch `stage-4/4.1-vault` at `42cff4f`: `git diff 44637ed 42cff4f` (contract, mocks, unit/sandbox tests, `.github/workflows/ci.yml`, `scripts/`, fuzz). The earlier findings come from `audit-1-fund-flows.md` (A1) and `audit-2-access-spec-dos.md` (A2). Their resolutions are recorded in `audits/2026-11-08-ai-audits.md`.
- **Spec:** `lld/contracts-vault.md` v1.4 (wt `brain-4.1-vault` at `03fcf2a`), ADR-0008, T-10 (`architecture/security.md` v1.2), and the consumers `billing.md` v1.4 §3/§6/§7.1 and `rails.md` v1.8 §8.6.
- **Auditor:** an independent AI agent (re-audit required by ADR-0020). I did not write this code or either earlier audit.
- **Method:**
  - I read every hunk of the diff against the LLD and the full post-fix `lib.rs`.
  - I checked gas against the `near-parameters` 0.38 fee tables and near-contract-standards 5.30.1 in the cargo registry, and against the CI logs of run 42489167233, fetched with `gh`.
  - I ran `cargo test -p subscription-vault` in a scratch clone at `42cff4f`: **70 passed**, 0 failed (287 s).
  - I wrote 2 PoC tests (`poc/audit3.rs`, scratch only; both pass).
  - I did not run the sandbox locally. The sandbox evidence below comes from the CI job log.
  - The repo was not modified.

## Summary

| Severity | New findings | IDs |
|---|---|---|
| Critical | 0 | |
| High | 0 | |
| Medium | 0 | |
| Low | 4 | N-1 … N-4 |
| Info | 6 | N-5 … N-10 |

All earlier findings that needed contract or CI changes are fixed correctly. I found no regression that can lose or misdirect funds.
- **Placement.** The new `ERR_UNSUPPORTED_SENDER` check sits after the allow-list and before any parsing or state change. It returns the full amount, so the token refunds the deposit.
- **Gas floor.** `ERR_GAS` comes first in `collect` and runs before any write.
- **Gas budgets.** They fit at both ends:
  - at the 170 Tgas floor, `collect` has about 15 Tgas of headroom;
  - at 300 Tgas, or at the 1 PGas that NEAR now allows, nothing can fail;
  - every callback runs on static gas, with 2.5–10× its measured burn.
- **Constructor.** The new checks are sound and reject no legitimate configuration.
- **CI.** It does run the sandbox suite against the reproducible wasm of that run, and I verified this in the logs.

The new findings are edges of the fixes:
- the relayed-sender fix covers only `intents_account` (N-1);
- the PR CI hash is a merge-ref build that can never be deployed or verified (N-2);
- the 366-day period bound rejects multi-year prices that billing allows (N-3);
- the larger refund budgets push `cancel`/`withdraw` above the common 30 Tgas default (N-4).

**Verdict: No open Critical/High.**

---

## 1. Earlier findings: status

Legend: **Resolved**; **Resolved (conditional)**: the fix is right but depends on an open pre-mainnet measurement; **Partial**; **Accepted**: correctly recorded as accepted risk; **Open**.

### Audit 1 (fund flows)

| ID | Finding | Status | Evidence |
|---|---|---|---|
| A1-M-1 | Caller-chosen gas starves the merchant leg | **Resolved (conditional)** | See below the table. |
| A1-M-2 | Deposit relayed by `intents.near` makes it the payer | **Resolved** for `intents_account` (see N-1 for other relayers) | `lib.rs:192-197` refunds `sender_id ∈ {intents_account, vault}` with `ERR_UNSUPPORTED_SENDER` after the allow-list panic (`:189`) and before `amount == 0` and parsing. No write happens before it. Create and top-up are both covered. Unit `relayed_deposit_from_intents_account_is_refunded`; sandbox `deposit_relayed_by_intents_account_is_refunded` checks real balances (intents back to `MINTED`, vault 0). The refund returns to the Intents `ft_withdraw` resolve, which re-credits the user. That is NEAR Intents behaviour, not verified against the deployed Verifier here. |
| A1-L-1 | A failed callback cannot be recovered | **Accepted, mitigated** | See below the table. |
| A1-L-2 | Payer-chosen `fee_bps`, `start_at`, `max_periods` | **Resolved (billing)** | `billing.md` §6 linkage now requires `fee_bps`, `receiver_kind`, `start_at ≤ current_period_end` and `max_periods`. The vault side is documented by `payer_signed_fee_is_a_mandate_term`. |
| A1-L-3 | TVL-cap squatting | **Accepted** | LLD §3.11, T-10 residual, test `accepted_risk_tvl_cap_squatting`. The claimed mitigation through the `start_at` bound is weak (N-9). |
| A1-L-4 | NEP-141 resolve assumption | **Accepted** | LLD §3.11. |
| A1-I-1 | Unparseable success counts as delivered | **Accepted** | LLD §3.2, §3.11. |
| A1-I-2 | ADR-0008 vs LLD on partial delivery | **Open (docs)** | The LLD keeps the behaviour, and billing §7.1 handles short shares. The ADR-0008 / F-22 amendment is still listed as a lead/owner task. |
| A1-I-3 | Token blocklists and pauses | **Accepted** | Added to the T-10 residual risk. |
| A1-I-4 | Test-model gaps | **Mostly resolved** | (1) Callback failure: native tests added. (3) Mocks: 4.x-style forwarding and a 30 Tgas receiver; the model is a conservative lower bound, see §3. (4) The minimum-gas probe now asserts the flags (`min_gas.rs`). (6) CI runs 256 proptest cases. (5) The keeper rule "exactly one of `collected`/`collect_failed`" is still not in the proptest (recorded as open). (2) The native harness still uses test fees; gas is asserted only in the sandbox, which is acceptable. |
| A1-I-5 | Keeper contract holes | **Resolved (billing)** | `billing.md` §7.1 covers `collect_failed` (N = 5, then fallback), `ERR_IN_FLIGHT` (wait for the outcome, re-check after 30 s) and `ERR_GAS` (alert). |
| A1-I-6 | yocto bookkeeping | No action needed | |

**A1-M-1 detail (Resolved, conditional).**
- **Code.** `collect` starts with `prepaid_gas < MIN_COLLECT_GAS (170 Tgas) → ERR_GAS` (`lib.rs:225`). The merchant `ft_transfer_call` now has 100 Tgas of static gas (`:48`, `:257`).
- **Why the caller no longer matters.** The receiver's budget comes from static gas: 100 − 30 = **≥ 70 Tgas** under the 4.x model. The floor only turns an under-gassed call into a clean, named error.
- **Tests.** Sandbox `collect_at_minimum_gas_settles` and `heavier_receiver_still_fits_the_merchant_leg_budget` (60 Tgas receiver at 170 Tgas attached) both passed in CI.
- **Condition.** The real `intents.near::ft_on_transfer` cost has not been measured yet. If it were between 70 and about 117 Tgas, an attacker attaching 170 Tgas could still force `collect_failed` while the keeper's 250 Tgas would succeed. Keep the pre-mainnet measurement as a hard gate.

**A1-L-1 detail (Accepted, mitigated).**
- **New static gas.**

  | Receipt | Static gas | CI burn |
  |---|---|---|
  | `on_merchant_leg` | 50 Tgas | 2.23 Tgas own + 35 Tgas children + ≈ 1.8 Tgas receipt fees ≈ 39 Tgas |
  | `on_collected` | 20 Tgas | 1.86 Tgas |
  | `on_refunded` | 20 Tgas | 2.50 Tgas |
  | transfers | 15 Tgas | 1.69 Tgas |

- **Sandbox coverage.** Every path runs at the minimum accepted gas, and the tests assert that every vault receipt succeeded and that the flags cleared (`min_gas.rs`).
- **Documentation.** The stuck states are documented natively (`failed_collect_callback_leaves_the_period_stuck`, `failed_refund_callback_blocks_withdraw_forever`) and in LLD §3.10.

### Audit 2 (access, spec, DoS)

| ID | Finding | Status | Evidence |
|---|---|---|---|
| A2-M-1 | Relayer becomes payer | **Resolved** for `intents_account`; **residual** for other relaying contracts (N-1) | As A1-M-2. |
| A2-M-2 | TVL-cap squatting | **Accepted** | As A1-L-3. |
| A2-M-3 | Unbounded `period_secs`, `max_periods`, `start_at` | **Resolved** (but see N-3) | See below the table. |
| A2-M-4 | Events in failed receipts | **Resolved (consumer spec)** | `rails.md` §8.6 forwards an event only if all hold: executor = `NP_VAULT_CONTRACT`, status `SuccessValue`/`SuccessReceiptId`, block final, schema valid; dedupe on `receipt_id:log_index`. LLD §4.1 states the validity rule. The optional vault hardening (emit after flush) was not done, which is acceptable. |
| A2-M-5 | Collect gas starvation | **Resolved (conditional)** | As A1-M-1. |
| A2-M-6 | Fee bypass through linkage | **Resolved (billing)** | As A1-L-2. Billing §3 adds the linkage columns. |
| A2-M-7 | Tested wasm ≠ deployed wasm | **Resolved**, with a process gap (N-2) | See below the table. |
| A2-L-1 | Partial-delivery spec conflict; billing rules | **Partial** | Billing §7.1 is done. The ADR-0008 / F-22 amendment is open. |
| A2-L-2 | Constructor accepts degenerate configurations | **Resolved** | See below the table. |
| A2-L-3 | CI supply chain | **Partial (as recorded)** | Done: `fuzz/Cargo.lock` committed with a drift check; cargo-near installed `--locked` with no cache. Open: actions pinned only to `@v4`; `NEAR_SANDBOX_SHA256` unset (CI log shows `EXPECTED:` empty, near-sandbox 2.14.0 sha256 `797b9d7a…f13`). See also N-8. |
| A2-L-4 | Code-replacement window at deploy | **Resolved (runbook)** | LLD §6 step 4 puts `DeployContract`, `new` and `DeleteKey` (all keys) in one batch. Batch actions are atomic, and `new` is `#[private]` (predecessor = vault in a self-signed batch). |
| A2-L-5 | Closing callbacks not tested at minimum gas | **Resolved** | `min_gas::closing_paths_at_minimum_gas_release_everything` covers three paths at the minimum gas: an exhausting collect that removes the mandate, cancel and withdraw. It asserts the mandate is removed and `tvl == 0`. Static gas is now 20 Tgas. |
| A2-I-1 | Lenient number strings | **Documented** | LLD §3.1. |
| A2-I-2 | Receivers unrestricted | **Accepted** | |
| A2-I-3 | Pagination unstable across writes | **Documented** | |
| A2-I-4 | Stray tokens | **Documented** | LLD §3.11. |
| A2-I-5 | Merchant cancel needs a NEAR-callable account | **Documented** | LLD §3.4. |
| A2-I-6 | Stale docs | **Resolved** | README, `build.sh` and the `Cargo.toml` comment now say cargo-near 0.22.0 / Rust 1.99.1 and V1–V10. |

**A2-M-3 detail (Resolved, but see N-3).**
- **Bounds.** `msg.rs`: `period_secs ≤ 366 d`, `max_periods ≤ 2^31−1`, `start_at ≤ now + 366 d`, all reported as `ERR_BAD_MSG`. The checks are inside the shape group, so the documented order (`ERR_BAD_MSG` → `ERR_FEE_TOO_HIGH` → zero share) still holds. `saturating_add` prevents overflow.
- **Tests.** Unit `inputs_are_bounded` (the audit PoCs, inverted); the oracle in `msg_fuzz` and the cargo-fuzz target both assert the bounds.
- **Billing.** Mandate columns are now bigint, and events that fail are parked instead of blocking the stream.

**A2-M-7 detail (Resolved, with a process gap, N-2).**
- **Dependency.** In CI, `integration` needs `reproducible`. It copies `res-reproducible/subscription_vault.wasm` over `res/` and verifies the artifact with `sha256sum -c`.
- **Run 42489167233.** The log shows `vault under test: 1bd3a04a…0af3` and 17/17 sandbox tests passed: gas_profile 1, min_gas 3, vault_flows 13.
- **Harness.** It loads only from `NEARPAY_WASM_DIR` (`common/mod.rs:wasm`) and never compiles the vault itself.
- **Toolchain.** Wasm jobs use Rust 1.99.1 and cargo-near 0.22.0, the image's versions.
- **Reproducibility.** Two reproducible builds produced the same hash.
- **Gap.** That hash is not a deployable artifact (N-2).

**A2-L-2 detail (Resolved).**
- **New rejections.** `new` now rejects `fee_receiver == intents_account`, `fee_receiver` or `intents_account` among the tokens, and `mandate_cap == 0`. `mandate_cap ≤ cap` then implies `cap > 0`.
- **One cap per token.** `len(tvl_caps) == len(tokens)`, combined with no duplicates and no unknown tokens, gives exactly one cap per token.
- **Nothing legitimate is rejected.** A treasury equal to `intents.near` would receive plain `ft_transfer`s that credit nobody, so rejecting it is right.
- **Tests.** Unit `new_rejects_bad_config` has 5 new cases.
- **Treasury registration.** This cannot be checked in the contract. It is now runbook step 2, plus a billing alert on `fee = "0"` with `fee_bps > 0`.

---

## 2. New findings

### Low

#### N-1: The relayed-sender fix covers only `intents_account`; other relaying contracts still become the payer
- **Location:** `lib.rs:192` (`sender_id == self.intents_account || sender_id == current_account_id()`). The LLD §3.1 claim "only directly signed `ft_transfer_call`s fund mandates" is stronger than the code.
- **Scenario:** Any contract that calls `token.ft_transfer_call(vault, msg)` on a user's behalf becomes `payer`. Examples:
  - a bridge finalising an inbound transfer with a `msg` (e.g. a locker that releases NEAR-native USDC with `ft_transfer_call`);
  - a router or aggregator;
  - a smart wallet that cannot make arbitrary calls.

  The human cannot `cancel` or `withdraw` (`ERR_UNAUTHORIZED`). If the merchant cancels, the refund is a plain `ft_transfer` to the relayer, which attributes it to nobody. This is the same impact as A1-M-2, through less likely paths. NearPay's own `prepare` flow tells payers to sign directly.

  PoC `poc_other_relayer_still_becomes_payer` (passes): with `bridge.test` as the sender, the deposit is accepted, `payer == bridge.test`, Alice's cancel fails with `ERR_UNAUTHORIZED`, and the merchant-cancel refund goes to `bridge.test`.
- **Fix:** choose one, while the contract is still mutable:
  - **(a)** Require `sender_id == env::signer_account_id()` in `ft_on_transfer`, otherwise refund with `ERR_UNSUPPORTED_SENDER`.
    - Allowed: a direct `ft_transfer_call` (signer = sender) and NEP-366 meta-transactions (the inner receipt's signer is the delegating user).
    - Refused: every relayer, including `intents.near`, so the `intents_account` special case becomes redundant.
    - Also refused: DAO/multisig payers. That is a product decision, but they are rare for consumer subscriptions.
  - **(b)** Keep the code and weaken LLD §3.1 to "deposits relayed by `intents_account` are refused; deposits relayed by other contracts make that contract the payer", then record it as accepted risk in §3.11 and T-10.

  Either way, add a unit test for the chosen rule.

#### N-2: The CI-recorded reproducible hash `1bd3a04…` is a build of the ephemeral PR merge ref. It cannot be reproduced from `42cff4f` and must not be deployed or published
- **Location:** `.github/workflows/ci.yml` (`on: pull_request`, `actions/checkout@v4` defaults to `refs/pull/N/merge`); LLD §6 runbook step 1.
- **Evidence:** The reproducible job log of run 42489167233 (event `pull_request`, headSha `42cff4f`) shows:
  - `HEAD is now at 408b500 Merge 42cff4f… into c569507…`
  - `NEP330_BUILD_INFO_SOURCE_CODE_SNAPSHOT: git+https://github.com/nearpay-money/nearpay-contracts?rev=408b50092e…`
  - `NEP330_LINK: …/tree/408b500…`

  cargo-near embeds these strings in the wasm's `contract_source_metadata`, so the bytes and the hash depend on the commit id.
  - `c569507` is an ancestor of `42cff4f`, so the code is identical.
  - However, building at `42cff4f`, or at the post-merge `main` commit, gives a different hash.
  - The embedded rev `408b500` lives only on `refs/pull/1/merge`, which GitHub removes when the PR closes. NEP-330 / SourceScan verification of `1bd3a04…` would then fail.
- **Scenario:** Someone takes "the reproducible wasm that passed the sandbox suite" from the PR run and deploys it, or publishes `1bd3a04…`, as the task statement and the audit record imply. The deployed code is correct, but nobody can verify it, which defeats T-10's "publish the code hash" control. Process only; no funds are at risk.
- **Fix:**
  - **Runbook step 1:** say explicitly "the `push` run on `main` (or a release tag), never a `pull_request` run". Record only that hash, with that run URL. That run also tests its own bytes, because `integration` needs `reproducible`.
  - **Optionally,** in PR runs, check out `ref: ${{ github.event.pull_request.head.sha }}` in the `reproducible` job, so its hash belongs to a real branch commit. Then prefer a merge strategy that keeps that commit (merge commit or fast-forward, not squash), so the PR-tested bytes are the deployable bytes.
  - **Audit record:** add a note that `1bd3a04…` is not a deploy hash.

#### N-3: The new `period_secs ≤ 366 days` bound rejects multi-year prices that billing allows
- **Location:** `msg.rs:22` (`MAX_PERIOD_SECS = 366 × 86400`); `billing.md` §3 prices: `interval in ('day','week','month','year')`, `interval_count between 1 and 12`. `POST /v1/mandates/prepare` has no matching restriction.
- **Scenario:** A merchant sells a 2-year plan (`year × 2`). `prepare` builds a mandate with `period_secs ≈ 63 072 000`. The payer signs, and the vault refunds with `ERR_BAD_MSG`. Auto-debit is impossible for every price longer than 366 days, and the payer pays a wasted transaction. Funds are refunded.

  The bound is stricter than its stated purpose (int4 safety). 12 × 366 days = 379 468 800 s is still below 2^31 − 1. PoC `poc_multi_year_price_is_refused_although_int4_safe` (passes).
- **Fix:** choose one.
  - Raise `MAX_PERIOD_SECS` to `12 × 366 × 86400`. That covers every billing price, stays int4/JS-safe, and keeps squatting-relevant bounds similar.
  - Make `prepare` refuse prices longer than 366 days (`mandate_unsupported_interval`), and record the limit in billing §6 and the spec.

  The same check applies to the 366-day `start_at` bound if trials longer than a year are ever allowed. Today they are not specified.

#### N-4: `cancel`/`withdraw` now need about 40 Tgas, above the 30 Tgas default of common NEAR tooling
- **Location:** `lib.rs:42,54` (`GAS_FT_TRANSFER` 8→15 and `GAS_ON_REFUNDED` 10→20, so 35 Tgas static on the refund path); LLD §3.3 notes "≈ 35 Tgas static plus their own work".
- **Evidence:**
  - CI `min_gas`: `cancel minimum accepted: Some(40) Tgas` and `withdraw … Some(40)` (5 Tgas steps, so 35 Tgas was refused).
  - Arithmetic: 2.33 burnt + 35 static + ≈ 1.8 receipt fees ≈ 39 Tgas.
  - Before the diff the path needed about 22 Tgas.
- **Scenario:** V2 matters most when NearPay's UI is unavailable. A payer who cancels through a generic tool that defaults to 30 Tgas (near-api-js `DEFAULT_FUNCTION_CALL_GAS`, near-cli JS, some wallet or explorer "call method" forms) gets `Exceeded the prepaid gas`. The failure is clean, with no state change, but the message does not say what to do. Funds are safe; only exit usability suffers.
- **Fix:**
  - Document "attach ≥ 50 Tgas (100 recommended)" for `cancel`/`withdraw` in the spec and the payer help text, and have the NearPay frontend attach 100 Tgas.
  - Optionally add `require!(prepaid_gas >= 45 Tgas, ERR_GAS)` to both, so the failure carries a stable, explainable code.
  - Do not lower the static budgets to fit 30 Tgas; the margin matters more (A1-L-1).

### Info

- **N-5: The "300 Tgas per call" assumption is out of date.**
  - **What changed.** In `near-parameters` 0.38, protocol 83 raised `max_total_prepaid_gas` / `max_gas_burnt` from 300 Tgas to **1 PGas** (`res/runtime_configs/83.yaml`; stable protocol is 88). The CI sandbox, near-sandbox 2.14.0, runs that config.
  - **Effect on the design.** Nothing breaks. A caller attaching more than 300 Tgas only gives more gas to the merchant leg and its callback.
  - **Where it is stale.** The comment in `collect_gas_floor_and_static_budgets` ("fits the 300 Tgas per-call limit") and LLD §3.3 ("≤ 300 Tgas per call") should say "≤ the protocol's prepaid-gas limit (300 Tgas before protocol 83, 1 PGas after)". Check the live mainnet protocol version before deploy.
- **N-6: The LLD wording about the gas floor is imprecise.**
  - **What the LLD says.** §3.10 says "the floor guarantees them" (the static budgets).
  - **What actually holds.** The static gas on each receipt is the guarantee, because a receipt with less cannot be created and the whole `collect` reverts. The floor only turns that revert into `ERR_GAS` early.
  - **Why it matters.** If the fee schedule changes and 170 Tgas no longer covers `collect`'s own work, `collect` at the floor fails with "Exceeded the prepaid gas" before any write. It never leaves a stuck state. This is correct, but the wording could lead someone to lower static budgets "because the floor covers it".
- **N-7: The mock token's gas model is a sound lower bound.** I compared it with the real implementations:
  - The mock forwards `prepaid − 30 Tgas` as static gas with **weight 0**, and 5 Tgas for resolve.
  - near-contract-standards 4.x (deployed tokens) forwards the same static gas but with near-sdk's default unused-gas weight of 1, so the receiver gets at least as much.
  - near-contract-standards 5.30.1 (`core_impl.rs:141-152`) forwards *all* remaining gas with weight 1.

  So sandbox margins are conservative for both. Two things remain unproven, as the record already says:
  - the real `intents.near` receiver cost (the mock burns 30–60 Tgas);
  - any extra reserve a real token might take above 30 Tgas.

  USDC/USDT `ft_transfer`, with blocklist and pause checks, should also be measured against the new 15 Tgas transfer budget. The mock burns 1.69 Tgas.
- **N-8: CI supply-chain leftovers (beyond the recorded open items).**
  - **(a)** The near-sandbox checksum step runs *after* the tests, so even once `NEAR_SANDBOX_SHA256` is set, an unverified binary has already run. Move the check before `cargo test` (set `NEAR_SANDBOX_BIN_PATH` to a verified download).
  - **(b)** The fuzz job's `git diff --exit-code fuzz/Cargo.lock` also runs after `cargo fuzz run` has resolved and executed the dependencies. Run `cargo fetch --locked --manifest-path fuzz/Cargo.toml` first, so drift fails before anything executes.
  - **(c)** Actions are still `@v4` (recorded open). GitHub is forcing `checkout`/`upload-artifact` from Node 20 to Node 24, so a bump plus SHA pin is due anyway.
- **N-9: The TVL-squatting mitigation claim through the `start_at` bound is ineffective.**
  - **What is claimed.** The record says the bound "limits how long a mandate can sit undue".
  - **Why it does not help.** A squatter does not need an undue mandate. With `merchant_receiver = self`, `fee_bps = 0`, `period_secs = 366 d` and `amount_per_period = 1` base unit, the mandate is due but drains 1 unit per year, so it holds the cap indefinitely while every collect pays the attacker back.
  - **Recommendation.** The accepted-risk record is right; drop the bound as a mitigation. Monitoring should flag `merchant_receiver == payer` and tiny `amount_per_period` relative to the balance.
- **N-10: The pre-fix baseline never built to wasm.** At `44637ed` and earlier (`3dfb74d`) the free fn `next_collect_at` collided with the exported view (E0428 on wasm32, per commit `42cff4f`). No wasm or sandbox evidence exists for any commit before `42cff4f`. Neither earlier audit relied on wasm runs (A2 said so explicitly), so their conclusions stand. This re-audit's sandbox evidence is the first.

---

## 3. Gas analysis (mainnet fees; burn figures from CI run 42489167233, mock token and receiver)

Receipt fees per child function call: action receipt 0.108 Tgas + function call 0.78 Tgas (protocol ≥ 66) execution, prepaid by the parent. A data dependency adds 0.036 Tgas.

| Point | Used by `collect` | Headroom |
|---|---|---|
| `collect` at 170 Tgas | 2.32 burnt + 150 static (100 merchant leg + 50 `on_merchant_leg`) + ≈ 1.8 fees ≈ **154.2 Tgas** | ≈ **15.8 Tgas**, split by weight 1:1 |
| `collect` at 250 Tgas (keeper) | as above | merchant leg ≈ 148 Tgas (receiver ≥ 118 under the 4.x model), `on_merchant_leg` ≈ 98 Tgas |
| `collect` at 300 Tgas | as above | merchant leg ≈ 173 Tgas, `on_merchant_leg` ≈ 123 Tgas; no receipt exceeds any limit |
| `collect` at 1 PGas (now allowed) | as above | ≈ 522 / 472 Tgas; harmless |
| `on_merchant_leg` (fee branch) | 2.23 + 35 (15 fee leg + 20 `on_collected`) + ≈ 1.8 ≈ 39 of ≥ 50 static | ≥ 11 Tgas |
| `on_collected` | 1.86 of 20 | |
| `on_refunded` | 2.50 of 20 | |
| merchant / fee / refund `ft_transfer` (mock) | 1.69 of 15 | |
| `cancel`/`withdraw` | 2.33 + 35 + ≈ 1.8 ≈ 39 Tgas | minimum accepted 40 Tgas (N-4) |

- **At the floor**, the CI sandbox ran `collect` at exactly 170 Tgas for both receiver kinds and for an exhausting/closing collect. Every vault receipt succeeded and the expected events were emitted. 169 Tgas → `ERR_GAS`.
- **Scheduling.** `collect` cannot fail to schedule its chain at ≥ 170 Tgas under today's fees. If it ever did, the receipt reverts entirely (promises are created only on success), so no state is stuck.
- **Burn target.** Sandbox: intents 13.49 Tgas, near 11.00 Tgas, excluding the receiver; the receiver burnt 31.23 Tgas. The target is ≤ 60 Tgas.

## 4. Verified OK in the diff (no finding)

- **`ERR_UNSUPPORTED_SENDER` placement.**
  - It comes after the allow-list check, so an unknown token still panics and nothing is attributed to it.
  - It runs before `amount == 0`, parsing and every write, and returns the full `amount`. For `amount = 0` it returns 0, which is harmless.
  - It covers create and top-up. No accepted path changed.
- **`ERR_GAS`.**
  - It runs first, before any read or write, and the keeper's 250 Tgas never hits it.
  - The LLD §3.2 order and billing §7.1 (`ERR_GAS` → alert) agree with the code.
  - Unit `every_error_string_is_reachable` and `constants_match_lld` include both new codes.
- **Bounds.** Exact boundaries are accepted (366 d, `i32::MAX`, now + 366 d) and boundary + 1 is rejected (unit test). A past `start_at` is still clamped to now. No overflow: `saturating_add`, and `due_at` is checked.
- **Constructor.** See A2-L-2. `get_config` and `get_token_tvl` are unchanged.
- **Mocks.**
  - mock-ft keeps `assert_one_yocto`, the `> 30 Tgas` rule and a `#[private]` resolver, and resolves with the standard `internal_ft_resolve_transfer`.
  - mock-intents' `set_burn_tgas` is unrestricted, which is acceptable for a sandbox mock.
  - Its burn loop makes a starved receiver fail visibly, so the token refunds and the vault emits `collect_failed`.
- **Rename `next_collect_at` → `mandate_next_collect_at`.** Behaviour is unchanged; the view keeps its name and ABI.
- **CI.**
  - The `shell: bash` default gives `-eo pipefail`, so `cargo test … | tee` fails the job on a test failure.
  - `permissions: contents: read`; trigger is `pull_request`, not `_target`.
  - The reproducible image is pinned by digest; the clean-tree check passes; the second build goes to a separate directory and is diffed.
- **Native suite at `42cff4f`:** 70/70 passed locally (`nice -n 15 env CARGO_BUILD_JOBS=1 cargo test -p subscription-vault --locked`).

## 5. PoC tests (scratch only; saved as `poc/audit3.rs`)

| Test | Shows | Result |
|---|---|---|
| `poc_other_relayer_still_becomes_payer` | N-1 | passes |
| `poc_multi_year_price_is_refused_although_int4_safe` | N-3 | passes |

To run them, add `mod audit3;` to `src/tests/mod.rs` in a scratch copy and run `cargo test -p subscription-vault audit3`.

## 6. Open items before mainnet (carried over and new)

1. Measure the real `intents.near::ft_on_transfer` cost of a USDC/USDT deposit (≪ 70 Tgas), and USDC/USDT `ft_transfer` (≪ 15 Tgas). This is the gate for A1-M-1 / A2-M-5.
2. Deploy only a hash from a `push`/tag run on `main` (N-2).
3. Decide N-1 (signer check or documented residual) and N-3 (raise the bound or restrict `prepare`). Both are cheap while the contract is mutable.
4. Document the gas `cancel`/`withdraw` need for payers (N-4).
5. Amend ADR-0008 / F-22 (A1-I-2, A2-L-1). Pin actions by SHA and set `NEAR_SANDBOX_SHA256`, verified before the tests (A2-L-3, N-8).

**Final verdict: No open Critical/High.**

---

## Re-check 2: `42cff4f..9f75067` (commits `eb1f31f`, `9f75067`; LLD v1.5 at brain `fa7700b`)

### What I checked
- I reviewed every hunk of `git diff 42cff4f 9f75067`: contract, tests, harness, CI, README and the audit record. I also reviewed the LLD v1.5 and billing v1.5 diffs (`03fcf2a..fa7700b`).
- **Meta-transaction semantics.** I checked them against nearcore source: `runtime/runtime/src/actions.rs::apply_delegate_action` on `master`, fetched from raw.githubusercontent.com.
- **Native suite at `9f75067`** (scratch clone): **75 passed**, 0 failed. That is the repo's 73 tests plus my 2 PoCs, saved as `poc/audit3b.rs`.
- **CI run 42493773739** (`pull_request`, headSha `9f75067`): all 6 jobs succeeded. From the job logs:
  - **Reproducible job:** it built the head commit, not a merge ref. The log shows `HEAD is now at 9f75067`, and the NEP-330 snapshot is `?rev=9f750678…`. Both builds gave `c025b71eca28ff36e8ed930a8abe8bab9a081558eb2477203b5eab2f11bd7396`. Because this was a PR run, that hash is labelled "NOT a deploy hash", which is correct.
  - **Integration job:** `vault under test: c025b71e…7396`; 17/17 sandbox tests passed. `GAS cancel minimum accepted: Some(50)` and `withdraw … Some(50)`, so every lower attempt failed with `ERR_GAS`. Collect burn is unchanged: 13.49 Tgas (intents) and 11.00 Tgas (near).
  - **Fuzz job:** the steps ran in this order: fetch from the lockfile (`--locked`), then fuzz, then the lock-drift check. All succeeded.
- The repo is unmodified and the scratch copies are deleted.

### Status of N-1 … N-10

| ID | Status | Evidence |
|---|---|---|
| N-1 | **Resolved** for relaying contracts, but the stated rationale is wrong (R-1) | `lib.rs` `ft_on_transfer`: refunded with `ERR_UNSUPPORTED_SENDER` if `sender_id ∈ {intents_account, vault}` or `sender_id != env::signer_account_id()`. The order is unchanged: allow-list panic, then sender checks, then `amount == 0` and parsing, all before any write. Unit `relayed_deposit_from_any_contract_is_refunded` covers create, top-up, intents relaying for a user, and intents signing itself. A third party cannot become, or make someone else, the payer, because `sender_id` is the token's predecessor and must also be the signer. A keyed account that signs its own transaction (signer = sender) is accepted, and it can cancel by itself. |
| N-2 | **Resolved**, one residual (R-2) | The `reproducible` job checks out `pull_request.head.sha \|\| github.sha`. The summary says `DEPLOYABLE` only when `push` and `refs/heads/main`, and it prints the commit. LLD §6 step 1 says to deploy only push-run hashes. Verified in run 42493773739 above. Fork PRs are fine: `refs/pull/N/head` keeps the head commit reachable in the base repo. |
| N-3 | **Resolved** | `MAX_PERIOD_SECS = 12 × 366 × 86400 = 379 468 800` (< 2^31 − 1, JS-safe). In ns, 3.8e17 is far below u64. `due_at` is overflow-checked and returns `None` (`ERR_NOT_DUE`) only after about 48 twelve-year periods. The fuzz oracle and cargo-fuzz use the constant. Unit `multi_year_period_is_accepted_and_collects_on_schedule` collects period 1 at start, refuses period 2 1 ns early, and collects it at exactly 730 days. |
| N-4 | **Resolved** | `MIN_EXIT_GAS` = 50 Tgas is checked first in `cancel` and `withdraw`, before the 1-yocto check; LLD §3.4 documents the order. Measured need is about 39 Tgas, so the headroom is about 11 Tgas. Unit `exits_need_min_exit_gas`. Sandbox: accepted at exactly 50 Tgas, `ERR_GAS` from 20 to 45 Tgas, and `entry_point_guards` gives `ERR_GAS` at 30 Tgas. Billing `prepare` attaches 100 Tgas. The static budgets were not lowered. A contract merchant that cancels must forward ≥ 50 Tgas (acceptable). |
| N-5 | **Resolved** | The `MIN_COLLECT_GAS` comment, the test comment and LLD §3.3 now cite 1 PGas since protocol 83. |
| N-6 | **Resolved** | LLD §3.3/§3.10 and the doc comment now say the static gas is the guarantee and the floors only produce an early error. |
| N-7 | Noted | The pre-mainnet gate now also covers USDC/USDT `ft_transfer` against the 15 Tgas budget. |
| N-8 | **(b) Resolved**, (a) and (c) open as recorded | Verified the step order in the CI job. |
| N-9 | **Resolved** | The claim is withdrawn in LLD §3.11 and the record. Monitoring now includes tiny `amount_per_period` relative to the balance. |
| N-10 | Noted | |

### New findings

#### R-1 (Low): The signer rule also refuses NEP-366 meta-transactions (and other relayer-submitted account models). The code, LLD v1.5 §3.1 and the audit record say the opposite
- **Origin.** The wrong claim began in my own N-1 recommendation ("the inner receipt's signer is the delegating user"). It is false, and I correct it here.
- **Location:**
  - `lib.rs` `ft_on_transfer` comment ("or a NEP-366 meta-transaction whose inner signer is the user");
  - LLD v1.5 §3.1 "Who may fund" ("a NEP-366 meta-transaction (the inner receipt's signer is the delegating user)");
  - `audits/2026-11-08-ai-audits.md` row N-1;
  - billing v1.5 §5 `prepare` (silent on meta-transactions).
- **Evidence.** nearcore `apply_delegate_action` builds the inner receipt with `predecessor_id: sender_id.clone()` (the delegating user) and `signer_id: action_receipt.signer_id().clone()`, which is the **relayer** who signed the outer transaction. The source comment adds "Gas is refunded to the signer, this is Relayer".
  - So in a meta-transaction `ft_transfer_call` by Alice, the token reports `sender_id = alice` while `env::signer_account_id() = relayer`.
  - The vault refunds with `ERR_UNSUPPORTED_SENDER`.
  - PoC `poc_meta_tx_deposit_is_refused` passes.
  - Exits are not affected: `cancel`/`withdraw` check the predecessor, and `probe_meta_tx_cancel_still_works` passes.
- **Impact.** The rule fails closed: tokens go back to Alice through `ft_resolve_transfer` and no funds are at risk. The cost is reach. Payers whose transactions a relayer submits cannot fund or top up mandates:
  - gasless or "sponsored" wallets using NEP-366;
  - FastAuth-style relayer flows;
  - by the same mechanism, ETH-implicit (NEP-518) wallet-contract accounts driven by a relayer. I did not verify this last case.

  The vault cannot tell "Alice via meta-transaction" from "contract X in a transaction signed by R": both reach it as `sender ≠ signer`. So no simple rule accepts one and refuses the other.
- **Fix:**
  - Keep the rule; failing closed is right for an immutable contract.
  - Correct the code comment, LLD §3.1 ("meta-transactions and relayer-submitted accounts are refused; the payer must sign and pay gas"), the audit-record row and billing §5 `prepare` text.
  - Add the PoC as a regression test that pins the refusal.
  - Have the frontend detect relayer-based wallets before the user signs.
  - If v1 needs gasless funding, that is a product decision needing an explicit-payer design (like SP-06), not a tweak.

#### R-2 (Low): The `DEPLOYABLE` label is written before the run has passed
- **Location:** the `ci.yml` `reproducible` job "Hash" step; `concurrency: cancel-in-progress: true`.
- **Scenario.** On a push to `main`, the `reproducible` job labels its hash `DEPLOYABLE` as soon as it finishes. `integration`, which needs it, runs afterwards and can still fail. A later push can also cancel the run (cancel-in-progress applies to `main` too). Either way, a job summary shows a "DEPLOYABLE" hash whose sandbox suite never passed. LLD §6 step 1 assumes "that run … ran the sandbox suite against these bytes", but nothing enforces it. Process only; it matters because the contract is immutable.
- **Fix:**
  - Emit the label from a final job with `needs: [lint, unit, integration, fuzz, reproducible]`, which runs only if all succeed, and have the `reproducible` job print a neutral label.
  - Make runbook step 1 require `gh run view <id> --json conclusion` = `success`.
  - Optionally set `cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}`.

#### R-3 (Info): On PR runs, only the `reproducible` job builds the head commit
`wasm` (the mocks), `integration` (the harness), `unit`, `lint` and `fuzz` still check out the merge ref. This is harmless while the PR base is an ancestor of the head, as it is today (`c569507`). Once `main` moves ahead, PR runs test head-commit vault bytes with mocks and harness code from the merge. The authoritative run is the push run on `main`, which is consistent. Either check out the head sha in `wasm` and `integration` too, or note this in the README.

### Verdict (re-check 2)
Every N-1 … N-10 item that needed code or CI changes is fixed. None of the changes can lose or misdirect funds:
- the new checks sit before any write;
- every rejection path refunds the full amount;
- `ERR_GAS` comes first and changes no state;
- the bound fits int4;
- the CI changes do what they claim.

R-1 is a documentation and reach correction (the rule fails closed). R-2 and R-3 are process items.

**Verdict: No open Critical/High.**
