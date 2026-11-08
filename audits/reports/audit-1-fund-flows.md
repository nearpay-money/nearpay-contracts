# Audit 1: subscription vault, fund flows and asynchronous execution

- **Target:** `nearpay-contracts` branch `stage-4/4.1-vault`, `contracts/subscription-vault` (near-sdk 5.30.1)
- **Spec:** `lld/contracts-vault.md` v1.3, ADR-0008, `architecture/security.md` T-10, `lld/billing.md` §7
- **Auditor:** independent AI agent (audit 1 of 2, ADR-0020). I did not write this code.
- **Focus:** every path that moves tokens or NEAR, the promise chains and their callbacks, the flight flags, arithmetic, gas reservation, NEP-145 storage, invariants V1–V10, and how well the tests cover them.
- **Method:** line-by-line review against the spec, checked against the near-sdk 5.30.1, near-contract-standards 5.30.1 and nearcore `near-parameters` 0.38 source in the cargo registry. I wrote PoC tests in a scratch copy (`/tmp/audit1/nearpay-contracts`, file `contracts/subscription-vault/src/tests/audit.rs`). I also measured the callbacks' host-side gas twice: under the harness's `RuntimeFeesConfig::test()` and under the **mainnet** fee parameters (`RuntimeConfigStore::new(None)`, enabled in the scratch copy with `AUDIT_MAINNET_FEES=1`). The existing suite passes: 63 tests in 205 s. The repo was not modified.

## Summary

| Severity | Count | IDs |
|---|---|---|
| Critical | 0 | |
| High | 0 | |
| Medium | 2 | M-1, M-2 |
| Low | 4 | L-1, L-2, L-3, L-4 |
| Info | 6 | I-1 … I-6 |

The core accounting is sound. `balance` / `in_flight` / `refund_in_flight` / `tvl` stay consistent under every interleaving I could build. That includes cancel, withdraw and top-up during a collect, failed refunds, partial and garbage results, and settlement after cancel. I found no double-spend, no case of refunding in-flight money, no overflow, and no way to move tokens to anyone other than the payer, the merchant (directly or via `intents_account`) or the fee receiver.

The findings are about the edges of the asynchronous design:
1. Gas is chosen by the permissionless caller.
2. Nothing recovers a callback that fails.
3. `sender_id` is treated as the human payer even when it is a relaying contract.
4. Collect parameters can be chosen by the payer, and caps can be squatted.

---

## Medium

### M-1: Permissionless `collect` with caller-chosen gas lets anyone starve the merchant leg and force `collect_failed` every period
**Verdict:** PLAUSIBLE. The mechanism is confirmed natively. Whether 5 Tgas is too little for `intents.near` has not been measured.

**Location:** `lib.rs:206-256` (`collect`: there is no `env::prepaid_gas()` floor), `lib.rs:47` (`GAS_FT_TRANSFER_CALL = 35 Tgas`), `lib.rs:233-243`.

**Description.** `collect` only needs enough gas for its own work plus the 70 Tgas of static gas it reserves for its children (35 Tgas merchant leg, 35 Tgas `on_merchant_leg`). Unused gas is split by weight, so a caller who attaches the minimum leaves about 0 unused gas. The merchant `ft_transfer_call` then runs with exactly 35 Tgas.

The code comment says the deployed USDC/USDT use the near-contract-standards 4.x logic. That logic forwards a fixed `prepaid_gas − 30 Tgas` to the receiver, so `intents.near::ft_on_transfer` gets about **5 Tgas**. That 5 Tgas must also pay for loading the large Verifier wasm (`wasm_contract_loading_bytes` = 1.09 Mgas/byte) and for the deposit's storage writes and `mt_mint` event. NEAR Intents' docs recommend 100 Tgas for a simple `ft_transfer_call` deposit. If `ft_on_transfer` runs out of gas, the token refunds the share, the vault reads `"0"`, and it emits `collect_failed`. The period is not counted.

No funds are lost. However, anyone can turn every due period into `collect_failed`, and it costs them about 1–2 mNEAR per attempt.

**Exploit / failure scenario.**
1. From the due time, the attacker submits `collect(id)` every block with about 76 Tgas attached.
2. While each starved attempt is in flight (4–5 blocks: token → intents → resolve → callback), the keeper's `collect` fails with `ERR_IN_FLIGHT`. Billing §7 then says "retry at `next_collect_at`", which is immediately, because the period was not counted.
3. The keeper only wins if its transaction lands in the one block between the attacker's attempts.
4. Billing marks the invoice paid only on `collected`, and it has no rule for `collect_failed`. Only `ERR_INSUFFICIENT_BALANCE` falls back to `send_invoice`. The merchant is therefore never paid by auto-debit, and billing gets no signal to fall back.

There is a second-order effect: `collect_failed {reason: "merchant_transfer_failed"}` blames the merchant's destination.

**Evidence.** `audit_collect_accepts_minimal_gas` (PoC below) binary-searches the smallest prepaid gas at which `collect` succeeds. Under mainnet fees it is **73 Tgas** natively (wasm execution adds about 2 Tgas on-chain). At that gas the merchant leg's prepaid gas is its static 35 Tgas.

The sandbox suite cannot show this, for two reasons:
- The mock token is near-contract-standards **5.30**, which forwards all remaining gas by weight. A receiver behind it gets about 25 Tgas, not 5.
- The mock intents receiver is a few storage writes, not the Verifier.

The README already says "Real USDC/USDT and `intents.near` gas use is not measured before mainnet."

**Recommended fix.**
- Make the receiver's gas independent of the caller. Either raise the merchant leg's static gas (`GAS_FT_TRANSFER_CALL = 30 Tgas + R`, where R is the measured `intents.near::ft_on_transfer` cost × 2, for example 80 Tgas), or add `require!(env::prepaid_gas() >= MIN_COLLECT_GAS)` with `MIN_COLLECT_GAS` covering all static gas plus R. A floor alone is not enough if the static values stay at 35 Tgas, because unused gas can still be ~0 at exactly the floor. Do both, or size the static value correctly.
- Re-derive the keeper's attached gas (150 Tgas today) from the new static total, for example 200 Tgas.
- Add a sandbox test that imports the real USDC (or USDT) and `intents.near` wasm from mainnet with `near-workspaces` `import_contract`. It should run `collect` at the minimum gas `collect` accepts and assert `collected`.
- Billing §7: define how the keeper handles `collect_failed`, for example retry with a cap, then fall back to `send_invoice` and open a case.

**PoC (scratch `audit.rs`):**
```rust
#[test]
fn audit_collect_accepts_minimal_gas() {
    let mut s = Sim::new();
    let id = s.create_default();
    let (mut lo, mut hi) = (1u64, 300u64); // Tgas
    while lo < hi {
        let mid = (lo + hi) / 2;
        let storage = Sim::take_storage();
        let snapshot = storage.clone();
        let mut ctx = s.context(MALLORY, 0);
        ctx.prepaid_gas = Gas::from_tgas(mid);
        let (vmc, fees) = if std::env::var("AUDIT_MAINNET_FEES").is_ok() {
            let c = near_parameters::RuntimeConfigStore::new(None).get_config(u32::MAX).clone();
            ((*c.wasm_config).clone(), (*c.fees).clone())
        } else { (test_vm_config(), RuntimeFeesConfig::test()) };
        env::set_blockchain_interface(MockedBlockchain::new(ctx, vmc, fees, vec![], storage, HashMap::new(), None));
        let mut v: Vault = env::state_read().unwrap();
        let r = catch_unwind(AssertUnwindSafe(|| { let p = v.collect(id); drop(p); }));
        std::mem::forget(v);
        s.install(VAULT, 0, vec![], snapshot);
        match r { Ok(_) => hi = mid, Err(_) => lo = mid + 1 }
    }
    // mainnet fees: lo = 73 -> merchant leg prepaid = 35 Tgas static, 4.x token forwards ~5 Tgas
    assert!(lo < 100, "collect has no minimum-gas guard");
}
```
Output with `AUDIT_MAINNET_FEES=1`: `collect succeeds natively with prepaid = 73 Tgas (static children 70 Tgas); merchant leg then gets ~35 Tgas -> 4.x token forwards ~5 Tgas to intents.near`.

---

### M-2: A mandate funded through NEAR Intents (or 1Click `customRecipientMsg`) gets `intents.near` as its payer, so the real payer cannot cancel and every refund is lost
**Verdict:** CONFIRMED (natively).

**Location:** `lib.rs:185` and `lib.rs:558-559` (`payer = sender_id`), `lib.rs:313` (cancel authority), `lib.rs:736-740` (refund = plain `ft_transfer(payer)`).

**Description.** The payer is the `sender_id` the token reports. A user who holds funds in NEAR Intents (the platform NearPay is built on) and funds a mandate with the Verifier's `ft_withdraw { receiver_id: vault, msg: Some(create_mandate…) }` triggers `token.ft_transfer_call` **from `intents.near`**. So `sender_id = intents.near`. The same happens on the 1Click `customRecipientMsg` path that ADR-0008 gates on SP-06. The vault accepts the deposit. Anyone may `storage_deposit(account_id = intents.near)`, and the user's wallet or app will do so to make the create succeed.

The result:
- The human payer is not `m.payer`, so `cancel` and `withdraw` fail with `ERR_UNAUTHORIZED` for them. V2 is broken.
- `intents.near` will never call `cancel`.
- If the merchant cancels, the refund is a plain `ft_transfer(intents.near, …)` with no `msg`. That is not an `ft_on_transfer` deposit, so the Verifier credits nobody and the tokens are stranded in the Verifier's token balance.
- Collects still work, so the merchant keeps getting paid until the balance runs out. The user's only "exit" is to let it drain.

**Exploit / failure scenario.** A user funds a 12-month mandate from their Intents balance, through a NearPay or third-party UI, or later through 1Click after SP-06. In month 3 they try to cancel and get `ERR_UNAUTHORIZED`. The remaining 9 months are either collected or, if the merchant cancels, refunded into an unattributed Verifier balance.

**PoC** (`audit_mandate_funded_via_intents_has_intents_as_payer`, passes):
```rust
s.register(INTENTS, REG + MCOST);
let o = s.ft_on_transfer(USDC, INTENTS, 3 * AMOUNT, &create_msg(MERCHANT, "intents_account", AMOUNT, 100).to_string());
assert_eq!(o.result, Ok(0));                                   // accepted
assert_eq!(s.mandate(id).unwrap().payer.as_str(), INTENTS);
assert_eq!(s.cancel(ALICE, id).err(), ERR_UNAUTHORIZED);       // human payer locked out
// merchant cancel -> ft_transfer { receiver_id: INTENTS } with no msg: credits nobody
```

**Recommended fix.** In `internal_create`, reject `sender_id == self.intents_account` (and `sender_id == env::current_account_id()`) with `ERR_BAD_MSG` or `ERR_UNAUTHORIZED`, so the token refunds the deposit. Document that only directly signed `ft_transfer_call`s can fund mandates.

For SP-06, a 1Click/Intents funding path needs an explicit design, for example a `payer` field in `msg` that is honoured only when `sender_id == intents_account`, with refunds sent as `ft_transfer_call(intents_account, msg = payer)`. That needs an LLD change and new tests. Do not enable that path without one.

---

## Low

### L-1: Nothing recovers a failed callback. A reverted `on_merchant_leg` / `on_collected` / `on_refunded` locks funds and the storage lock forever
**Verdict:** CONFIRMED for the impact. Unlikely today under mainnet parameters.

**Location:** `lib.rs:49-53` (static gas), `lib.rs:261-300`, `lib.rs:361-385`, `lib.rs:705-708` (`maybe_close` requires `!in_flight && refund_in_flight == 0`).

**Description.** If any callback receipt fails, its state changes are reverted, but the token transfer before it stands. Causes include out-of-gas or a future change to protocol fees, which matters for a contract with no keys. The consequences:
- **Collect callback fails:** `in_flight` stays `true` forever. `collect` fails with `ERR_IN_FLIGHT` forever, and the keeper retries forever. `amount_per_period` is never credited back, even if the merchant leg failed. The mandate can be canceled, but it never closes. The payer's 0.01 NEAR storage lock is never released and `storage_unregister` fails with `ERR_OPEN_MANDATES`. `tvl[token]` over-counts one period forever, so cap capacity is permanently reduced.
- **`on_refunded` fails:** `refund_in_flight` stays set, so `withdraw` fails with `ERR_IN_FLIGHT` forever. If the transfer failed, that amount is lost to the payer.

**Gas margins I measured** (host-side: receipt fees, children's prepaid gas, storage and log host calls; wasm instructions are not metered natively):

| Callback | Static | Host gas, mainnet fees | Host gas, `RuntimeFeesConfig::test()` |
|---|---|---|---|
| `on_merchant_leg` (fee branch) | 35 Tgas | **20.8 Tgas** (18 of it is children's static gas) | 37.4 Tgas (exceeds 35) |
| `on_collected` (exhaust + close) | 10 Tgas | 1.1 Tgas | 1.1 Tgas |
| `on_refunded` (close) | 10 Tgas | 1.0 Tgas | 1.0 Tgas |

Under mainnet parameters (function call 0.2 + 0.78 Tgas since protocol 66) each callback has more than 10 Tgas of headroom for wasm execution, so this is not exploitable today. The margins depend on protocol fees, which have been re-priced before. Under the pre-66 function-call fee (2.32 + 2.32 Tgas), `on_merchant_leg` would have needed about 37 Tgas, which is **more than its 35 Tgas**. There is no way to adjust the static values after deploy. Note also that a permissionless `collect` caller can always leave exactly the static gas (see M-1).

**PoC** (`audit_failed_collect_callback_locks_period_forever`, passes; simulates the reverted callback by never delivering it):
```rust
s.collect(id).unwrap();                      // on_merchant_leg reverted on-chain
s.cancel(ALICE, id).unwrap();
s.on_refunded(id, 2 * AMOUNT, Some(ALICE), ok_empty()).unwrap();
assert!(s.mandate(id).unwrap().in_flight);   // forever
assert_eq!(s.withdraw(ALICE, id).err(), ERR_INSUFFICIENT_BALANCE);
assert_eq!(s.call(ALICE, 1, vec![], |v| v.storage_unregister(None)).err(), ERR_OPEN_MANDATES);
assert_eq!(s.tvl(USDC), AMOUNT);             // TVL keeps counting the stuck period
```

**Recommended fix.**
- Make the static gas generous. Unused static gas is refunded, so this costs the keeper little: for example `on_merchant_leg` 50 Tgas, `on_collected` and `on_refunded` 20 Tgas, plus the floor from M-1.
- Add a sandbox test at the minimum gas `collect` accepts that asserts the callback receipts succeeded and `in_flight == false`. Today's `gas_profile` probe only notices this indirectly, through later `ERR_IN_FLIGHT`s.
- Optionally add a test with mainnet fees in the native harness, as in the scratch copy. The current harness uses `RuntimeFeesConfig::test()`, whose fees are about 2–100× mainnet's, so native gas numbers mean nothing.

### L-2: The payer chooses `fee_bps` (and `start_at`, `max_periods`), and billing's linkage rule does not check them, so the NearPay fee can be bypassed on linked subscriptions
**Verdict:** CONFIRMED (contract plus spec reading).

**Location:** `msg.rs:89` (only `fee_bps ≤ max_fee_bps` is enforced); `lld/billing.md` §6 linkage rule ("token/amount/period match the subscription's price").

**Description.** The payer signs the `msg`, so they can set `fee_bps = 0`. The merchant gets 100% and the treasury gets nothing, yet billing still links the mandate. Billing checks `ext_ref`, `merchant_receiver`, token, amount and period, but not `fee_bps`. Likewise, a far-future `start_at` leaves a linked auto-debit subscription whose `collect` keeps failing with `ERR_NOT_DUE`, and the keeper waits for `next_collect_at`.

**Fix (billing, not the contract):** the linkage rule must also require `fee_bps == configured fee`, `receiver_kind == "intents_account"` (the LLD already suggests this), `start_at` within the subscription's current period, and `max_periods` consistent with the subscription. Otherwise record the mandate as `unlinked`.

### L-3: Any account can squat the per-token TVL cap with refundable mandates and block all new mandates and top-ups
**Verdict:** CONFIRMED (`audit_tvl_cap_can_be_squatted_and_released`).

**Location:** `lib.rs:636-643` (`tvl_after_deposit`).

**Description.** The v1 cap is intentionally low (T-10). An attacker funds `cap / mandate_cap` mandates paying themselves (merchant = self, fee 0). That costs capital they can recover at any time, plus 0.012 NEAR of storage per mandate. While they hold it, every new `create_mandate` and every legitimate `top_up` for that token is refunded with `ERR_CAP_EXCEEDED: tvl`. The attacker cancels whenever they like and loses only gas. Funds are safe, but auto-debit signups for that token can be blocked indefinitely. ADR-0008's fallback is `send_invoice`.

**Fix / mitigation:** accept as a known v1 risk and document it, or monitor TVL concentration and alert when one payer holds more than X% of the cap. The owner sets cap sizing at deploy (LLD §6). A contract-side fix such as per-payer caps is weak, because accounts are cheap.

### L-4: `ft_transfer_call` returning `Failed` is read as "nothing moved". That is false if the token's `ft_resolve_transfer` fails
**Verdict:** PLAUSIBLE only with a non-compliant or broken token; inherent to NEP-141.

**Location:** `lib.rs:784-795` (`Err(PromiseError::Failed) => 0`).

**Description.** For the intents leg, the result the vault sees is the result of `ft_resolve_transfer`. If that receipt fails after the receiver kept the tokens (it has 5 Tgas reserved; a fault in the token could still make it fail), the transfer stands, but the vault credits the full amount back to the mandate. That breaks V1 by one share: `balance` counts tokens that are no longer held. Compliant near-contract-standards tokens never fail in resolve.

**Fix:** none needed for USDC/USDT. Record the NEP-141 assumption next to the "garbage success" assumption in LLD §3.2, and keep `allowed_tokens` limited to audited standard tokens.

---

## Info

- **I-1: Unparseable or over-long success is counted as the full share** (`lib.rs:791`). This is the documented choice: solvency over fairness. If a token ever returned the used amount as a JSON number instead of a string, every intents collect would charge the payer a full period while the share sat stranded in the vault. USDC/USDT return strings. A test asserting the exact result format of the deployed tokens, from imported mainnet wasm, would close this.
- **I-2: ADR-0008 and the LLD disagree on partial delivery.** ADR-0008 says a period that is "not fully delivered" is marked failed and not advanced. LLD v1.2 §3.2 (which the code follows) counts it with the delivered amount. Amend the ADR or add a note so the next reader does not "fix" the code.
- **I-3: Token-level blocklists and pauses limit V2.** If USDC or USDT blocks the payer, refunds fail and the balance stays withdrawable later, so funds are safe but stuck. If the token blocks the **vault** or the fee receiver, every mandate in that token freezes, or fees stay on the mandate. This is an inherent token-policy risk; list it in T-10's residual risk.
- **I-4: Test-model gaps** (none of them hides a logic bug I could find):
  1. The model never makes a callback fail, so the "callbacks always succeed" assumption behind V6 and V2 is untested (L-1).
  2. The native harness uses `RuntimeFeesConfig::test()`, which is not mainnet-like (function call 2.32 + 2.32 Tgas, data receipt 4.7 Tgas, against mainnet's 0.2 / 0.78 / 0.036). Any native gas reasoning is invalid.
  3. The sandbox mocks are more generous than production: the 5.x weighted-gas mock token and the trivial mock intents receiver hide M-1.
  4. `gas_profile`'s minimum-gas probe does not assert `in_flight == false` after each try.
  5. The proptest does not assert the keeper contract "exactly one of `collected` / `collect_failed` per successful collect transaction". It is only checked in unit tests.
  6. The model covers ≤ 8 mandates, ≤ 70 ops and 96 cases by default. Nightly CI could raise `PROPTEST_CASES`.
- **I-5: The keeper contract has holes beyond M-1.** Billing §7 does not cover `collect_failed`, a successful transaction that returns `false`. Billing also treats `ERR_IN_FLIGHT` as "retry at `next_collect_at`", which right after an uncounted attempt means immediately. Define a bounded retry.
- **I-6: NEAR and yocto bookkeeping is fine, but here is the arithmetic.** Each collect spends 1–2 yoctoNEAR of the vault's free NEAR (1 yocto per `ft_transfer` / `ft_transfer_call`). `cancel` and `withdraw` bring in 1 yocto for the one they spend. Contract rewards (30% of burnt gas) more than cover this. If a NEAR transfer from `storage_withdraw` or `storage_unregister` fails because the account was deleted, the NEAR stays with the vault (self-inflicted, unrecoverable).

---

## Verified OK

- **`ft_on_transfer` refund semantics (NEP-141).** It returns `0` when accepted and the full `amount` on every rejection, with no state change (all checks run before writes in both `internal_create` and `internal_top_up`). It panics for tokens that are not allowed, and the token then refunds. `amount = 0` is rejected. A payer who under-gasses the call makes it revert cleanly, and the token refunds. `sender_id` is trusted only from allowed tokens.
- **`collect` → `on_merchant_leg` → (fee) → `on_collected`.**
  - The fee leg is sent only when `used == share && fee > 0`.
  - `in_flight` is set before the promises and cleared only in `settle_collect`. There is no receipt window in which `in_flight == false` while a callback of the same collect is still pending.
  - Callbacks recompute share and fee from immutable terms. `on_collected` caps the `share` argument with `.min(share)`.
  - Both callbacks check `in_flight`, so stray or duplicate callbacks are no-ops.
  - Every callback reads exactly one promise result (`.then` on a single promise). The bounded read (64 bytes) cannot abort, because `TooLong` is handled.
- **Result parsing.** For the intents leg, the value is the `ft_resolve_transfer` "used" amount. near-contract-standards caps it to what was actually refunded (`min(receiver_balance, unused)`), so a partial or zero "used" never credits tokens that did not come back. A failed plain `ft_transfer` means nothing moved, and the 1 yocto is refunded to the vault. The garbage / too-long results are handled as documented.
- **Settlement arithmetic.** `sent = used + fee_paid ≤ amount`. `balance += amount − sent`, so credit-back never exceeds what the mandate already held, and V10 holds. `sub_tvl(sent)` keeps `tvl == Σ held` exactly. Partial delivery counts the period without a fee. A failed fee leg counts the period and keeps the fee in `balance` (`collected.fee = "0"`). `exhausted` is set only while the mandate is `Active`, so a canceled mandate is never exhausted by a late collect.
- **Cancel / withdraw / refunds.** A refund covers only `balance`, which excludes an in-flight collect (V1). At most one refund is in flight per mandate: `cancel` needs `Active`, `withdraw` needs not-`Active` and `refund_in_flight == 0`, and nothing can return a mandate to `Active`. A failed refund restores `balance`. A successful one decrements `tvl`. Cancel or withdraw during a collect settles correctly in every order, and the credit-back lands in `balance` for a later `withdraw`. Both need exactly 1 yocto (full-access key), so the keeper's function-call key cannot cancel. Only the payer or merchant can cancel; only the payer can withdraw. Refunds go only to `m.payer` (V4).
- **Closing.** `maybe_close` requires a closed status, `balance == 0`, `!in_flight` and `refund_in_flight == 0`. It removes the mandate, its index entry and (when the set becomes empty) the payer's index set, and releases exactly one storage lock (V8). Nested `IterableSet` writes are flushed and the outer entry is marked modified.
- **Arithmetic.** `math::split` is exact for all `u128` with no `amount × k` intermediate, and its proptest passes. `due_at` is overflow-checked. `held()` and `balance + amount` are bounded by caps far below `u128::MAX`. The release profile has `overflow-checks = true`. Rounding gives the fee receiver less than 1 base unit per period (share is floored). `validate_create` rejects a zero share.
- **Caps (V7, V10).** Create checks `amount_per_period ≤ mandate_cap` and `amount ≤ mandate_cap`. Top-up checks `held + amount` (in-flight collect included) and the TVL cap with `checked_add`. Credit-backs cannot raise `tvl`.
- **Rate limit (V3).** One period per call, `now ≥ start + n·period` with n counted from 0, and checks run in the documented order (V9). A late keeper catches up one period per call. A `start_at` in the past is clamped to now.
- **NEP-145.** Registration (0.002 NEAR) and the mandate lock (0.01 NEAR) cover worst-case bytes (64-character ids and `ext_ref`, first-mandate index creation; existing unit test). I estimate a worst-case first mandate at about 760 bytes, or 0.0076 NEAR. `storage_withdraw` and `storage_unregister` only release unlocked NEAR and need 1 yocto. `storage_deposit` refunds go to the predecessor. No path consumes vault storage without a lock, so withdrawals cannot push the vault below its storage stake. The deploy funding covers code and base state.
- **Privacy and immutability.** The callbacks are `#[private]`, and their bodies also call `assert_self`. `new` is `#[init] #[private]`. There are no setters, owner, upgrade or admin methods, and no method sends tokens elsewhere (V4, V5). `collect`, `ft_on_transfer` and the callbacks are non-payable.
- **Gas, mainnet parameters.** `on_merchant_leg` uses 20.8 of 35 Tgas host-side, and `on_collected` / `on_refunded` about 1 of 10 Tgas, all with headroom for wasm. Payer-gassed paths (cancel, withdraw, create) fail cleanly or restore balance when under-gassed. With the keeper's 150 Tgas, the 4.x receiver gets about 42 Tgas.
- **Property test model.** The model is a faithful token ledger for success, failure, partial and garbage outcomes with arbitrary delivery order. It checks V1 to V10, per-mandate conservation, and a final drain that closes every mandate and unregisters storage. The harness writes and re-reads state per call and reverts on panic, which matches on-chain receipt semantics apart from gas.

## PoC tests written (scratch only, `/tmp/audit1/nearpay-contracts/contracts/subscription-vault/src/tests/audit.rs`)

| Test | Shows | Result |
|---|---|---|
| `audit_collect_accepts_minimal_gas` | M-1: collect accepted at 73 Tgas (mainnet fees), merchant leg left with its 35 Tgas static gas | passes |
| `audit_mandate_funded_via_intents_has_intents_as_payer` | M-2 | passes |
| `audit_failed_collect_callback_locks_period_forever` | L-1 impact | passes |
| `audit_callback_host_gas` | L-1 margins (run with and without `AUDIT_MAINNET_FEES=1`) | passes, prints the table above |
| `audit_tvl_cap_can_be_squatted_and_released` | L-3 | passes |

The scratch copy adds the dev-dependency `near-parameters = "=0.38.0"` and an `AUDIT_MAINNET_FEES` switch in `tests/mod.rs::install`. Run with:
`AUDIT_MAINNET_FEES=1 cargo test -p subscription-vault audit -- --nocapture`.
