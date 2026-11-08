# Audit 2: subscription vault. Access control, input validation, spec conformance, DoS

- **Target:** `nearpay-contracts` branch `stage-4/4.1-vault` @ `44637ed` (`contracts/subscription-vault`, mocks, integration tests, `.github/workflows/ci.yml`, `scripts/build.sh`)
- **Spec:** `lld/contracts-vault.md` v1.3 (wt/brain-4.1-vault), ADR-0008, threat T-10 (`architecture/security.md`), consumers `lld/billing.md` §3/§6/§7 (v1.3) and `lld/rails.md` §8.6
- **Auditor:** an independent AI agent (ADR-0020). I did not write this code.
- **Method:** manual review of every entry point, callback and parser path against the LLD; near-sdk 5.30.1, near-contract-standards 5.30.1 and near-vm-runner 0.38 sources checked where behaviour mattered. I ran the existing suite (`cargo test -p subscription-vault`: 63 passed) and wrote 8 extra PoC/probe tests in a scratch copy (`/tmp/audit2/repo/contracts/subscription-vault/src/tests/audit2.rs`, all pass). No wasm target was available, so the sandbox, gas-profile and reproducible-build claims below are reasoned, not executed. I did not modify or commit anything in the repo.

## Summary

| Severity | Count | IDs |
|---|---|---|
| Critical | 0 | |
| High | 0 | |
| Medium | 7 | M-1 to M-7 |
| Low | 5 | L-1 to L-5 |
| Info | 6 | I-1 to I-6 |

I found no way for an unauthorised party to move, redirect or freeze mandate funds. The access-control core holds:
- callbacks are really private;
- the token allow-list is enforced before any parsing;
- only the payer or the merchant can cancel, and only the payer can withdraw;
- no admin, upgrade or migration path exists;
- mandate terms are immutable.

The Medium findings fall into two groups:
- **Funding and DoS edges of a permissionless, immutable design:** M-1, M-2, M-5.
- **Mismatches between what the vault emits and what billing/rails are specified to consume:** M-3, M-4, M-6, plus M-7 (build/test fidelity).

---

## Medium

### M-1: A deposit relayed by `intents.near` (or any contract) makes the relayer the payer: the human loses the V2 exit and refunds are unattributed

- **Location:** `contracts/subscription-vault/src/lib.rs:176-186` (`payer = sender_id`), `:313` (cancel auth), `:341` (withdraw auth), `:729-746` (refund `ft_transfer` to `m.payer`)
- **Description:** `payer` is the NEP-141 `sender_id`. If the tokens come through an intermediary contract, `sender_id` is that contract, not the human. The obvious intermediary in NearPay's ecosystem is `intents.near`:
  - Intents `ft_withdraw` with `msg` performs `ft_transfer_call(receiver=vault, msg)`.
  - The 1Click `customRecipientMsg` delivery that ADR-0008 gates on SP-06 works the same way.

  The vault accepts such deposits. NEP-145 lets anyone `storage_deposit` for `intents.near`.
- **Failure scenario** (PoC `poc_intents_account_becomes_payer_and_receives_refund`):
  1. Alice funds a mandate from her Intents balance, so `payer = intents.near`.
  2. Alice's `cancel`/`withdraw` fail with `ERR_UNAUTHORIZED`.
  3. If the merchant cancels, the refund is a plain `ft_transfer` to `intents.near` with no `msg`. That credits no Intents user, so Alice's funds are effectively lost.
  4. Any Intents user can also `top_up` any mandate whose payer is `intents.near`.

  This breaks V2 ("a payer can always reach Canceled and recover balance") for the human payer.
- **Recommendation:**
  - In `ft_on_transfer`, reject with `ERR_UNAUTHORIZED` (full refund) when `sender_id == self.intents_account`. This is one comparison against an existing constructor parameter.
  - State in LLD §3.1 and the billing `prepare` flow that the payer must be the account that signs `ft_transfer_call`.
  - Before SP-06 enables 1Click funding, design an explicit, verified payer/refund-to (needs an ADR).
- **Test:** yes, unit PoC (below).

### M-2: TVL-cap squatting: one capital holder can lock every other payer out of a token indefinitely, at no loss

- **Location:** `lib.rs:548-552`, `:636-643` (`tvl_after_deposit`); caps are constructor-only (`lib.rs:141-154`)
- **Description:** The per-token TVL cap is global, first-come and immutable. T-10 calls for a deliberately low cap. Nothing limits how much of it one payer may occupy. A squatting mandate can be set up so nobody can take its funds:
  - `merchant_receiver` = self;
  - `start_at` = far future, so it is never due;
  - `fee_bps = 0`.

  The attacker can leave at any time with 100% of the tokens.
- **Failure scenario** (PoC `poc_tvl_cap_squatting_is_free_and_permanent_until_attacker_leaves`):
  - Setup: cap 1,000 USDC, `mandate_cap` 100 USDC.
  - The attacker opens 10 mandates, paying 0.102 NEAR of storage, and fills the cap.
  - Every new legitimate `create_mandate`/`top_up` is refunded with `ERR_CAP_EXCEEDED: tvl`.
  - The attacker's cost is only the opportunity cost of the capital.
  - The only remedy is deploying `v2.vault…` and migrating payers.
- **Recommendation:** This cannot be fully prevented in a permissionless, immutable design: per-payer limits are sybil-able, and the attacker can be their own merchant. So:
  1. Record it as accepted risk in T-10.
  2. Alert on `tvl / cap` and on mandates with `start_at` far in the future or with `merchant_receiver == payer`.
  3. Pre-stage the v2 deployment runbook.
  4. Optionally make squatting less convenient with M-3's bound on `start_at`.
- **Test:** yes, unit PoC.

### M-3: The vault accepts and emits values the billing mirror cannot store (poison events)

- **Location:** `msg.rs:43` (`period_secs: u64`, lower bound only), `:47-48` (`max_periods: Option<u32>`), `:45-46` (`start_at` unbounded), `:81-88`; `events.rs:35-37`. Consumer: `lld/billing.md` §3 `mandates.period_secs integer`, `max_periods integer` (Postgres int4, max 2,147,483,647). JS/TS consumers (rails) lose precision above 2^53.
- **Description:** Billing records every `mandate_created` (linked or `unlinked`). Anyone can create a mandate for 1 base unit plus 0.012 NEAR with:
  - `period_secs = 2^31`, or
  - `max_periods = 4,294,967,295`, or
  - `period_secs > 2^53`.

  The insert fails (`integer out of range`), or JS parsing silently rounds.
- **Failure scenario** (PoC `poc_values_outside_billing_int4_columns_are_accepted`, `poc_number_edge_cases`):
  - If the rails→billing consumer processes `evt.rails.vault.event` in order with retry, one such event blocks mirroring of every later vault event: `collected` evidence, cancels.
  - That stalls auto-debit invoice settlement until someone fixes it by hand.
  - Each attempt costs the attacker almost nothing, and the funds are recoverable.
- **Recommendation:**
  - **Bound the inputs at the source** (cheapest while the contract is still mutable): `period_secs ≤ 366 × 86400 × 5` (or another product maximum below 2^31), `max_periods ≤ 10_000` (or ≤ i32::MAX), `start_at ≤ now + 366 days`. Return `ERR_BAD_MSG` and add these to LLD §3.1 and `nearpay-spec`.
  - **Independently fix billing:** use bigint columns, and make the consumer park undecodable events instead of blocking on them.
- **Test:** yes, unit PoC (acceptance demonstrated; the billing failure is by inspection of the billing schema).

### M-4: Vault events can appear in failed receipts. Rails' indexer spec checks neither receipt status nor the emitting account, which allows phantom `mandate_created`/`topped_up` with a reusable `mandate_id`

- **Location:**
  - Vault: `lib.rs:575-598` (`mandate_created` logged before the state writes, which happen at method end and collection drop), `lib.rs:632` (`topped_up` logged before the flush), `lib.rs:321-322` (`canceled` logged before `maybe_close`).
  - Consumer: `nearpay-brain/lld/rails.md:331-339`: "for each receipt outcome with executor in {intents.near, v1.vault…, NEP-141 contracts…}: parse logs … nearpay_vault events → outbox".
- **Description:**
  - NEAR keeps logs of aborted executions: `near-vm-runner` `VMOutcome::abort` = `compute_outcome()` with `logs`, plus `aborted`.
  - The payer chooses the gas `ft_on_transfer` receives (the `ft_transfer_call` prepaid gas minus the token's reserve). So an attacker can tune gas so the receipt runs out *after* `mandate_created` is logged but *before* state is written.
  - The token then refunds the full amount. The failed receipt still carries a well-formed `nearpay_vault` `mandate_created`, with attacker-chosen `ext_ref`/`merchant_receiver`/amounts and `mandate_id = next_id`.
  - `next_id` was not consumed, so the next real mandate gets the same id.
  - Rails' LLD does not require receipt success, nor `executor == vault` for `nearpay_vault` events.
- **Failure scenario:**
  1. The attacker emits phantom `mandate_created{id: N, ext_ref: <victim sub id>, merchant_receiver: <merchant destination>, matching price}`.
  2. Billing links it and activates auto-debit for a mandate that does not exist. The keeper then gets `ERR_NOT_FOUND`, retries 5 times, and opens a case.
  3. The real mandate N created afterwards collides with `unique (vault_contract, onchain_id)`.
  4. Repeating this poisons ids continuously.
  5. Phantom `topped_up` inflates the mirror balance. A payer or merchant can also emit a phantom `canceled`.
- **Recommendation:**
  - **Rails** (required): accept `nearpay_vault` events only from receipts whose `executor_id == NP_VAULT_CONTRACT` and whose status is `SuccessValue`/`SuccessReceiptId`. Add both to rails §8.6 and to the billing consumer tests.
  - **Vault LLD §4.1:** add "events are valid only in successful receipts of the vault account".
  - Optional vault hardening: emit events as the last statement after an explicit `flush()` of the touched collections. This shrinks, but cannot close, the window, because the root state is still written after return.
- **Test:** not executable in the unit harness (it is runtime behaviour). Sandbox PoC sketch: sweep `ft_transfer_call` gas in 0.1 Tgas steps from 35 Tgas upward with a valid `create_mandate`. Assert that some result `is_failure()` while `outcome.logs` contains `"event":"mandate_created"`, and that `get_mandate(N)` is null.

### M-5: Permissionless `collect` lets any caller starve the `intents_account` merchant leg of gas and force `collect_failed`

- **Location:** `lib.rs:47` (`GAS_FT_TRANSFER_CALL = 35 Tgas`), `lib.rs:233-243` (static gas + unused-gas weight, with the caller choosing the total)
- **Description:** The merchant leg's guaranteed gas is the static 35 Tgas (PoC `poc_merchant_leg_static_gas_is_fixed`); everything above that is a share of whatever the caller attached. With the near-contract-standards 4.x behaviour (which the code comment says deployed tokens use), `ft_on_transfer` at the receiver gets `prepaid_gas − 30 Tgas`. So when someone calls `collect` with the minimum that schedules (about 80–90 Tgas per LLD §3.3), `intents.near`'s `ft_on_transfer` gets about 5 Tgas plus a small share.

  If that is not enough for `intents.near`:
  - the receipt fails;
  - the token refunds;
  - `on_merchant_leg` sees `used = 0`, emits `collect_failed`, and clears `in_flight`.

  The attacker can repeat this every time the period is due. Each attempt burns only about 0.003 NEAR, and the keeper's own attempts hit `ERR_IN_FLIGHT` while the attacker's chain is pending. Billing §7 does not say what to do with `collect_failed` (see L-1).
- **Why the tests do not catch it:** the mock FT (`contracts/mocks/mock-ft/src/lib.rs:41-52`) adds the `> 30 Tgas` check but then uses near-contract-standards 5.x, which forwards *all* remaining gas to the receiver. The mock intents receiver is also nearly free. The gas-profile minimum probe (`gas_profile.rs:69-77`) therefore overstates the receiver's budget. The audits README already notes that real `intents.near` gas is unmeasured.
- **Recommendation:** Guarantee the receiver budget no matter who calls. Either:
  - (a) set `GAS_FT_TRANSFER_CALL = 30 Tgas + RECEIVER_BUDGET`, with the budget measured against `intents.near` and doubled (e.g. 80–100 Tgas total); or
  - (b) `require!(env::prepaid_gas() >= MIN_COLLECT_GAS)` at the top of `collect`, with a new stable error string.

  Then add a sandbox test that imports mainnet USDC/USDT and `intents.near` (`worker.import_contract`) and collects at exactly the minimum attached gas.
- **Test:** partial. The unit PoC shows that the leg's gas floor is the fixed 35 Tgas whatever the caller attached. The exploit needs real `intents.near` gas numbers, which are unverified.

### M-6: The payer chooses `fee_bps`, and billing's linkage ignores it (fee bypass), along with `receiver_kind`, `start_at` and `max_periods`

- **Location:** `msg.rs:49`, `:89-91` (any `fee_bps ≤ max_fee_bps`, 0 included); consumer `lld/billing.md:114` (linkage compares only `ext_ref`, `merchant_receiver`, token, amount, period); the billing `mandates` table (`billing.md` §3) has no `fee_bps`/`receiver_kind`/`ext_ref` columns.
- **Description:**
  - A payer can edit the `msg` returned by `POST /v1/mandates/prepare` and set `fee_bps: 0`. The mandate is still linked, and NearPay's fee (ADR-0006, "Vault `collect` sends the fee share to the treasury") is never paid (PoC `poc_payer_chosen_zero_fee_is_accepted`).
  - The same gap lets these be linked:
    - a `near_account` mandate (LLD §4.1 says linkage "should" require `intents_account`; billing does not);
    - a far-future `start_at`, so the subscription shows auto-debit but nothing is ever due;
    - a `max_periods` shorter than the subscription.
- **Recommendation:** The billing linkage must require all of:
  - `fee_bps` = the merchant's contracted value;
  - `receiver_kind = intents_account`;
  - `start_at ≤ current_period_end`;
  - `max_periods` null or ≥ the remaining periods.

  Add the columns. Not a vault code change: the vault correctly treats the fee as a payer-signed term.
- **Test:** yes, unit PoC (vault side); billing side by inspection.

### M-7: The wasm that is tested is not the wasm that will be deployed (toolchain mismatch)

- **Location:** `.github/workflows/ci.yml:20-21,103-120` (integration and gas profile run on `res/` built by the host's Rust 1.97.0 + cargo-near 0.23.0); `contracts/subscription-vault/Cargo.toml:16-18` (deployable wasm built in `sourcescan/cargo-near:0.22.0-rust-1.99.1`); the comment calls it the "cargo-near 0.23.0 project template", and README says reproducible builds use cargo-near 0.23.0.
- **Description:**
  - The sandbox suite and gas profile never execute the artifact whose hash is published and deployed.
  - A different rustc (1.97 vs 1.99.1) and cargo-near (0.23 vs 0.22) can change codegen, the wasm features emitted, and gas.
  - For an immutable money contract, the deployed bytes must be the tested bytes.
  - I could not verify the image tag or digest (no registry access); its versions disagree with the rest of the repo.
- **Recommendation:**
  - Make `integration` depend on `reproducible` and run against `res-reproducible/`, keeping the non-reproducible build only for the mocks.
  - Align the image's cargo-near/Rust with `CARGO_NEAR_VERSION`/`RUST_VERSION`, or document why not.
  - Build the reproducible wasm twice (or on two runners) and assert equal hashes.
  - Record the deployed hash next to the CI run id.
- **Test:** n/a (CI configuration).

---

## Low

### L-1: Spec conflict on partial delivery, and billing has no rule for `collect_failed` or a short `merchant_share`

- **Location:** `lib.rs:656-701` (partial delivery counts the period); ADR-0008 ("marks the period failed (not advanced) if the merchant share was not fully delivered") and `quality/stress-test.md:104,148` (F-22, "period not advanced") vs LLD §3.2 v1.2 (partial counts); `billing.md` §7 (handles only `collected` and tx errors).
- **Description:**
  - The implementation follows the LLD. The ADR and the stress-test catalogue still describe the opposite, with no ADR amendment.
  - Billing marks an invoice paid on any `collected`, even if `merchant_share` < the expected share, despite LLD §3.2's "billing compares `merchant_share`".
  - A `collect_failed` (including one forced by M-5) has no defined handling, so a `mandate.collect` job may wait forever.
- **Recommendation:**
  - Amend ADR-0008 (or add a superseding note) and F-22.
  - Billing §7: on `collected` with `merchant_share` < expected, mark the invoice partially paid or open a case. On `collect_failed`, retry at `next_collect_at` with a cap, then fall back to `send_invoice`.
- **Test:** existing `collect_partial_delivery_counts_period_without_fee` shows the behaviour.

### L-2: Constructor accepts configurations that silently degrade forever

- **Location:** `lib.rs:126-154`
- **Description:** These are accepted:
  - `cap = 0` / `mandate_cap = 0` (token unusable);
  - `fee_receiver` or `intents_account` ∈ `allowed_tokens`;
  - `fee_receiver == intents_account`.

  More importantly, if `fee_receiver` is not storage-registered on every allowed token (or later unregisters), every fee leg fails. The fee then stays on the payer's mandate (`settle_collect`, payer-favourable) permanently, with only `collected.fee = "0"` as a signal. Nothing can fix this after deployment.
- **Recommendation:**
  - Reject zero caps and overlaps in `new`.
  - Deploy runbook (LLD §6): before `new`, check `storage_balance_of(treasury)` and `storage_balance_of(intents.near)` on each token.
  - Alert on `collected.fee == "0"` for mandates with `fee_bps > 0`.
- **Test:** no (by inspection).

### L-3: CI supply-chain hygiene

- **Location:** `ci.yml:34,52,67,76,94,106,111,134,143,163,174,181`; `.gitignore:12`; `ci.yml:74-82,141-149`
- **Description:**
  - The four actions (`actions/checkout`, `actions/cache`, `actions/upload-artifact`, `actions/download-artifact`) are pinned to a mutable `@v4` tag, not a commit SHA.
  - `fuzz/Cargo.lock` is gitignored, so the fuzz job resolves fresh transitive dependencies on every run and executes them on the runner.
  - The `cargo-near` binary is restored from `actions/cache` with no integrity check. The scoping rules limit cross-branch poisoning, but the binary that builds the published wasm should be verified.
  - near-workspaces downloads `near-sandbox` from S3 with no checksum (test-only).
  - What is fine:
    - `permissions: contents: read`;
    - `pull_request` (not `pull_request_target`);
    - `--locked` on workspace builds;
    - pinned toolchain and tool versions;
    - Docker image pinned by digest;
    - `passed_env = []`;
    - clean-tree check for reproducible builds.
- **Recommendation:**
  - Pin actions by SHA (Dependabot can bump them).
  - Commit `fuzz/Cargo.lock` and run with `--locked`.
  - Do not cache cargo-near in the `reproducible` job (or verify a sha256 of the binary).
  - Optionally set `SANDBOX_ARTIFACT_URL` to a checksummed mirror.
- **Test:** n/a.

### L-4: The deployment step allows a window in which the code can still be replaced

- **Location:** LLD §6 ("deploy + `new(...)`, then delete all access keys"), §3.5
- **Description:**
  - `new` is `#[init] #[private]`, which near-sdk enforces, so it cannot be front-run, and re-init fails (sandbox `entry_point_guards`).
  - But if the key deletion is a later transaction, the key holder (or a compromised key) can redeploy different code in the meantime. Users have no on-chain proof that the keys were removed atomically.
- **Recommendation:**
  - Do it as one batch transaction: `DeployContract` + `FunctionCall new` + `DeleteKey` (all keys).
  - Then publish `view_code` hash = the reproducible hash, and `view_access_key_list` = [].
  - Add this to the runbook as a checklist with RPC commands.
- **Test:** n/a.

### L-5: No test proves the closing callbacks succeed on their static gas alone, and a failure there would be permanent

- **Location:** `lib.rs:51,53` (`on_collected`, `on_refunded` 10 Tgas static); `gas_profile.rs:69-77` probes only a non-closing collect.
- **Description:** If the caller attaches the minimum, `on_collected`/`on_refunded` run on 10 Tgas. If they ran out of gas on the closing path (`maybe_close` removes the mandate and the index entry):
  - `in_flight`/`refund_in_flight` would stay set forever;
  - TVL would never decrement, letting someone burn TVL capacity permanently by repeatedly cancelling their own mandates;
  - a `collected` log could appear in a failed receipt (see M-4).

  My probe (`probe_callback_host_gas`) measured host-function gas for the worst case: a 61-character payer, a 64-byte `ext_ref`, with close. Results: `on_collected` (exhaust + close) about 1.12 Tgas, `on_refunded` (cancel + close) about 1.08 Tgas. Wasm compute and base costs are excluded. The margin looks adequate (roughly 4x), but it is not asserted on wasm.
- **Recommendation:**
  - Add sandbox tests that run `cancel` and an exhausting `collect` with the exact minimum attached gas, and assert the mandate is removed and `tvl` decremented.
  - Consider 20 Tgas static for these callbacks: it costs nothing, because unused gas is refunded.
- **Test:** yes, probe (measurement only; below).

---

## Info

- **I-1: Lenient number strings.** `U128`/`U64` accept a leading `+` and leading zeros (`near-sdk-core` uses `str::parse`), e.g. `"+0010000000"` (PoC `poc_number_edge_cases`). This does not match "decimal string" in LLD §3.1 or the fuzz oracle's assumptions. It is harmless, because events re-serialise canonically. Either document it or reject anything not matching `^(0|[1-9][0-9]*)$` in the spec schema.
- **I-2: Receivers are unrestricted.** `merchant_receiver` may be `intents_account` (with `near_account`, meaning a plain transfer with no Intents credit), `fee_receiver`, a token contract, or the payer itself (PoC `poc_merchant_receiver_may_be_config_accounts`). That only hurts the payer, and billing linkage filters it. `start_at` and `period_secs` have no upper bound (see M-3).
- **I-3: Pagination is unstable.** `get_mandates_by_payer` pagination is O(1) per skip (`IterableSet::Iter::nth`), capped at 100 per call, and bounded by storage the payer paid for. Swap-remove reorders it on close, so paging across writes can skip or duplicate entries (documented as "stable between writes").
- **I-4: Stray tokens are unrecoverable.** Tokens sent with plain `ft_transfer` (no call) are never attributed or recoverable. This follows by design from V4 and the absence of an admin. Document it for support.
- **I-5: Merchant cancel needs a NEAR-callable account.** It requires the merchant to sign from `merchant_receiver`. For Intents accounts that are not NEAR-callable (e.g. EVM-keyed implicit accounts without a wallet contract), the merchant cannot cancel. That is acceptable (the payer still can), but worth stating in LLD §3.4.
- **I-6: Stale docs.** README says "invariants V1–V9" (V10 exists) and "cargo-near 0.23.0 … reproducible builds" (the image is 0.22.0, see M-7).

---

## Verified OK

- **Token allow-list:** `ft_on_transfer` panics with `ERR_TOKEN_NOT_ALLOWED` before any parsing when the predecessor is not in `allowed_tokens`, so a fake token cannot create or top up (unit `ft_on_transfer_from_unknown_token_panics`, sandbox `disallowed_token_and_bad_msg_are_refunded`). A top-up in another allowed token is refunded with `ERR_TOKEN_NOT_ALLOWED`. `payer` and `token` are never read from `msg`.
- **Callbacks:** `on_merchant_leg`, `on_collected` and `on_refunded` are `#[private]` (near-sdk entry-point guard) and also `assert_self()` in the body. Stray or replayed callbacks are no-ops (`in_flight` / `refund_in_flight` guards). `on_merchant_leg` recomputes share/fee from mandate terms and ignores its arguments.
- **No admin path:**
  - `new` is `#[init] #[private]` with no `ignore_state`, so it runs once;
  - `PanicOnDefault`;
  - no setter, owner, upgrade, `migrate`, `deploy_contract` or `delete_account`;
  - the only promises are `ft_transfer`/`ft_transfer_call` on the mandate's token, NEAR `transfer` to the storage owner or depositor, and self-callbacks.
- **Authorisation:** `cancel` is payer or `merchant_receiver` only, `withdraw` is payer only, and both need exactly 1 yocto. `collect` rejects deposits. Check orders match LLD §3.2/§3.4/§3.1 exactly, including create/top-up order and the cap-detail log lines.
- **V4/V5:**
  - token recipients are only the payer (refunds), `merchant_receiver` (direct, or `intents_account` with `msg = merchant_receiver`) and `fee_receiver`;
  - the merchant `ft_transfer_call` attaches exactly 1 yocto;
  - no code path writes `payer`, `merchant_receiver`, `receiver_kind`, `token`, `amount_per_period`, `period_secs`, `start_at_ns`, `max_periods`, `fee_bps` or `ext_ref` after creation.
- **Msg parsing:**
  - 2048-byte cap before parsing;
  - `deny_unknown_fields` on the enum and both structs;
  - serde rejects duplicate fields and a second top-level key;
  - numbers must be JSON integers (no floats or negatives for `period_secs`/`fee_bps`/`max_periods`/`mandate_id`);
  - amounts must be strings;
  - `AccountId` is validated;
  - `ext_ref` is ≤ 64 printable-ASCII bytes after JSON unescape, so `\u0000` and non-ASCII are rejected and event JSON cannot be broken out of;
  - `amount_per_period == 0`, `period_secs < 86400`, `max_periods == 0`, a zero merchant share, `merchant_receiver == vault` and `amount == 0` are all rejected;
  - a past or null `start_at` is clamped to now.
- **Caps:**
  - per-mandate cap on `amount_per_period`, funding, and held-after-top-up (in-flight included);
  - TVL cap with `checked_add`;
  - constructor enforces `mandate_cap ≤ cap` and exactly one cap per token.
- **Storage griefing:**
  - every mandate locks 0.01 NEAR of the payer's own NEP-145 balance, and every registration 0.002 NEAR (worst case asserted by `storage_cost_covers_worst_case_mandate`);
  - closed mandates are removed and their storage released;
  - no mutating path iterates an unbounded collection;
  - views are capped (100) and use O(1) skip;
  - NEP-145 refunds go only to the depositor or the account owner.
- **Events:** NEP-297 envelope, field order and encodings match LLD §4.1 (`event_wire_format_is_exact`). Error strings match LLD §3, are prefix-free (substring matching by the keeper is unambiguous), and the keeper only needs `ERR_INSUFFICIENT_BALANCE`/`ERR_NOT_DUE`/`ERR_IN_FLIGHT` from `collect` panics.
- **Arithmetic:** `split` is exact for all u128 values, `due_at` is overflow-checked, and the release profile has `overflow-checks = true`.
- **Test suite:** the full existing suite passes locally (63 tests, including the property test with the default case count).

---

## PoC test code

Scratch file `/tmp/audit2/repo/contracts/subscription-vault/src/tests/audit2.rs`, registered with `mod audit2;` in `src/tests/mod.rs`, using the existing `Sim` harness. Run with: `cargo test -p subscription-vault audit2 -- --nocapture`. All 8 pass.

```rust
use super::*;

const REG: u128 = 2 * MILLINEAR;
const MCOST: u128 = 10 * MILLINEAR;

fn create_ok(s: &mut Sim, token: &str, payer: &str, amount: u128, msg: &Value) -> u64 {
    let o = s.ft_on_transfer(token, payer, amount, &msg.to_string());
    assert_eq!(o.result, Ok(0), "rejected: {:?}", o.logs);
    o.events()[0]["data"][0]["mandate_id"].as_u64().unwrap()
}

// M-1
#[test]
fn poc_intents_account_becomes_payer_and_receives_refund() {
    let mut s = Sim::new();
    s.call(MALLORY, REG + MCOST, vec![], |v| v.storage_deposit(Some(acc(INTENTS)), None)).unwrap();
    let msg = create_msg(MERCHANT, "intents_account", AMOUNT, 100);
    let id = create_ok(&mut s, USDC, INTENTS, 3 * AMOUNT, &msg); // sender_id = relaying intents contract
    assert_eq!(s.mandate(id).unwrap().payer, acc(INTENTS));
    assert_eq!(s.cancel(ALICE, id).err(), ERR_UNAUTHORIZED); // the human has no rights
    let o = s.cancel(MERCHANT, id);
    assert!(acts(&o.receipts).iter().any(|x| matches!(x, Act::Call { receiver, method, args, .. }
        if receiver == USDC && method == "ft_transfer" && args["receiver_id"] == INTENTS)));
}

// M-2
#[test]
fn poc_tvl_cap_squatting_is_free_and_permanent_until_attacker_leaves() {
    let mut cfg = default_config();
    let (cap, mcap) = (1_000_000_000u128, 100_000_000u128);
    cfg.tvl_caps[0] = TokenCap { token: acc(USDC), cap: U128(cap), mandate_cap: U128(mcap) };
    let mut s = Sim::with_config(cfg);
    s.register(MALLORY, REG + 10 * MCOST);
    let squat = with(create_msg(MALLORY, "near_account", mcap, 0), "start_at", json!(u64::MAX.to_string()));
    let mut ids = vec![];
    for _ in 0..10 { ids.push(create_ok(&mut s, USDC, MALLORY, mcap, &squat)); }
    assert_eq!(s.tvl(USDC), cap);
    s.register(ALICE, REG + MCOST);
    let o = s.ft_on_transfer(USDC, ALICE, AMOUNT, &create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string());
    assert_eq!(o.result, Ok(AMOUNT));
    assert_eq!(o.logs, vec![LOG_CAP_EXCEEDED_TVL.to_string()]);
    assert_eq!(s.collect(ids[0]).err(), ERR_NOT_DUE);
    let o = s.cancel(MALLORY, ids[0]);
    assert!(acts(&o.receipts).iter().any(|x| matches!(x, Act::Call { method, args, .. }
        if method == "ft_transfer" && args["receiver_id"] == MALLORY && args["amount"] == mcap.to_string())));
}

// M-3
#[test]
fn poc_values_outside_billing_int4_columns_are_accepted() {
    let mut s = Sim::new();
    s.register(MALLORY, REG + MCOST);
    let mut m = create_msg(MALLORY, "near_account", 1, 0);
    m = with(m, "period_secs", json!(i32::MAX as u64 + 1));
    m = with(m, "max_periods", json!(u32::MAX));
    let o = s.ft_on_transfer(USDC, MALLORY, 1, &m.to_string());
    assert_eq!(o.result, Ok(0));
    assert!(o.logs[0].contains("\"period_secs\":2147483648"));
    assert!(o.logs[0].contains("\"max_periods\":4294967295"));
}

// M-6
#[test]
fn poc_payer_chosen_zero_fee_is_accepted() {
    let mut s = Sim::new();
    s.register(ALICE, REG + MCOST);
    let id = create_ok(&mut s, USDC, ALICE, 3 * AMOUNT, &create_msg(MERCHANT, "intents_account", AMOUNT, 0));
    let evs = s.collect_ok(id);
    assert_eq!(evs[0]["data"][0]["fee"], "0");
    assert_eq!(evs[0]["data"][0]["merchant_share"], AMOUNT.to_string());
}

// M-5 (gas floor of the merchant leg is fixed, whatever the caller attached)
#[test]
fn poc_merchant_leg_static_gas_is_fixed() {
    let mut s = Sim::new();
    let id = s.create_default();
    let o = s.collect(id);
    let leg = acts(&o.receipts).iter().find_map(|x| match x {
        Act::Call { method, gas, weight, .. } if method == "ft_transfer_call" => Some((*gas, *weight)),
        _ => None,
    }).unwrap();
    assert_eq!(leg, (Gas::from_tgas(35), 1));
}

// I-1 / M-3
#[test]
fn poc_number_edge_cases() {
    let mut s = Sim::new();
    s.register(ALICE, REG + 4 * MCOST);
    let m = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "amount_per_period", json!("+0010000000"));
    let id = create_ok(&mut s, USDC, ALICE, AMOUNT, &m);
    assert_eq!(s.mandate(id).unwrap().amount_per_period, U128(AMOUNT));
    let big = (1u64 << 60) + 1;
    let m = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "period_secs", json!(big));
    let o = s.ft_on_transfer(USDC, ALICE, AMOUNT, &m.to_string());
    assert_eq!(o.result, Ok(0));
    assert!(o.logs[0].contains(&format!("\"period_secs\":{big}")));
    let m = with(create_msg(MERCHANT, "near_account", AMOUNT, 0), "start_at", json!(u64::MAX.to_string()));
    let id = create_ok(&mut s, USDC, ALICE, AMOUNT, &m);
    assert_eq!(s.mandate(id).unwrap().next_collect_at, Some(U64(u64::MAX)));
}

// I-2
#[test]
fn poc_merchant_receiver_may_be_config_accounts() {
    let mut s = Sim::new();
    s.register(ALICE, REG + 3 * MCOST);
    for r in [INTENTS, TREASURY, USDC] {
        create_ok(&mut s, USDC, ALICE, AMOUNT, &create_msg(r, "near_account", AMOUNT, 0));
    }
}

// L-5 probe: host-function gas of the worst-case closing callbacks (prints ~1.1 Tgas each)
fn flush_and_gas(v: &mut Vault) -> u64 {
    v.mandates.flush(); v.tvl.flush(); v.storage_accounts.flush(); v.payer_mandates.flush();
    env::state_write(v);
    env::used_gas().as_gas()
}

#[test]
fn probe_callback_host_gas() {
    let mut s = Sim::new();
    let long = "a".repeat(56) + ".test";
    s.register(&long, REG + MCOST);
    let mut m = create_msg(MERCHANT, "intents_account", AMOUNT, 100);
    m = with(m, "max_periods", json!(1));
    m = with(m, "ext_ref", json!("x".repeat(64)));
    let id = create_ok(&mut s, USDC, &long, AMOUNT, &m);
    let (share, fee) = math::split(AMOUNT, 100);
    s.collect(id).unwrap();
    assert!(s.on_merchant_leg(id, share, fee, ok_u128(share)).unwrap().is_none());
    let g = s.call(VAULT, 0, vec![ok_empty()], move |v| { v.on_collected(id, U128(share), U128(fee)); flush_and_gas(v) }).unwrap();
    println!("on_collected (exhaust + close) host gas: {:.3} Tgas", g as f64 / 1e12);
    let id = create_ok(&mut s, USDC, &long, 3 * AMOUNT,
        &with(create_msg(MERCHANT, "intents_account", AMOUNT, 100), "ext_ref", json!("y".repeat(64))));
    s.cancel(&long, id).unwrap();
    let by = acc(&long);
    let g = s.call(VAULT, 0, vec![ok_empty()], move |v| { v.on_refunded(id, U128(3 * AMOUNT), Some(by)); flush_and_gas(v) }).unwrap();
    println!("on_refunded (cancel + close) host gas: {:.3} Tgas", g as f64 / 1e12);
}
```

Proposed sandbox PoC for M-5 (not executed: no wasm target available locally):

```rust
// integration-tests: import mainnet USDC and intents.near into the sandbox, create an
// intents_account mandate, then collect with the minimum gas that schedules the chain.
let (id, _) = create(&ctx, "intents_account", 3 * AMOUNT, None).await?;
for tgas in 80..=95 {
    let res = collect(&ctx, id, Gas::from_tgas(tgas)).await?;
    if res.is_success() {
        // Finding confirmed if this prints collect_failed at the lowest succeeding gas.
        println!("{tgas} Tgas → {:?}", names(&vault_events(&res)));
        break;
    }
}
```
