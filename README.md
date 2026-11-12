# nearpay-contracts

NEAR smart contracts for NearPay. v1 is one contract, the subscription vault: payer-funded, payer-cancellable mandates with a permissionless, rate-limited `collect`, used for auto-debit subscriptions.

Part of [NearPay](https://github.com/nearpay-money/nearpay) — start there for the live demo, the architecture diagram and the full "do X, see Y" walkthrough.

- **Where it sits:** a payer funds a mandate by `ft_transfer_call` from their own NEAR account; NearPay's billing keeper calls `collect` each period; rails forwards the contract's `nearpay_vault` events to billing. The contract has no owner or admin methods over balances.
- **Status:** built and tested, **not deployed on mainnet yet** (deployment is a human step). Planned account: `v1.nearpay-money.near`. The live closed beta (https://nearpay.money) therefore has no on-chain auto-debit yet; test mode uses the sandbox's simulated vault.

| | |
|---|---|
| Group | On-chain |
| Kind | Rust / NEAR |
| Ships as | one immutable account per version; not deployed on mainnet yet, planned account `v1.nearpay-money.near` |
| Owns | Vault contract, on-chain events |
| Depends on (published artifacts only) | near-sdk (Rust) |
| First built in | Stage 4 (step 4.1) |

## Documentation

Design docs live in [`nearpay-brain`](https://github.com/nearpay-money/nearpay-brain), not here.

- [`lld/contracts-vault.md`](https://github.com/nearpay-money/nearpay-brain/blob/main/lld/contracts-vault.md) — low-level design this repo is implemented from
- [`lld/audits/contracts-vault.md`](https://github.com/nearpay-money/nearpay-brain/blob/main/lld/audits/contracts-vault.md) — LLD audit findings already folded into the LLD
- [`audits/README.md`](audits/README.md) — invariants V1–V10 and the tests that cover each
- [`roadmap/development-plan.md`](https://github.com/nearpay-money/nearpay-brain/blob/main/roadmap/development-plan.md) — stage and step this repo is built in
- [`architecture/hld.md`](https://github.com/nearpay-money/nearpay-brain/blob/main/architecture/hld.md) — system design
- [`architecture/repositories.md`](https://github.com/nearpay-money/nearpay-brain/blob/main/architecture/repositories.md) — repo topology and dependency rules
- [`decisions/ADR-0008-subscriptions.md`](https://github.com/nearpay-money/nearpay-brain/blob/main/decisions/ADR-0008-subscriptions.md)
- [`architecture/flows.md#6-auto_debit-subscription-via-vault`](https://github.com/nearpay-money/nearpay-brain/blob/main/architecture/flows.md#6-auto_debit-subscription-via-vault)

## Layout

```
nearpay-contracts/
├── audits/                       # AI-agent audit reports; README maps invariants → tests
├── contracts/
│   ├── subscription-vault/       # the contract (src/) and its unit/property/fuzz tests (src/tests/)
│   └── mocks/                    # test-only: mock-ft (NEP-141), mock-intents (intents.near stand-in)
├── integration-tests/            # near-workspaces sandbox flows + gas profile
├── fuzz/                         # cargo-fuzz target for ft_on_transfer msg parsing (nightly, CI)
├── scripts/build.sh              # cargo-near wasm builds (+ --reproducible) and SHA256SUMS
└── .github/workflows/ci.yml
```

## Verify this repo

This is a Rust workspace (edition 2024, `rust-version` 1.95; CI lints and unit-tests with Rust 1.97.0). No Node, no local registry.

1. `cargo test -p subscription-vault` (unit tests, property tests of invariants V1–V10 and the native parser fuzz; no wasm toolchain, no Docker) → the run ends with `test result: ok. 75 passed; 0 failed` (the property tests take about two minutes after the first compile).
2. `NEAR_SANDBOX_BIN_PATH=/nonexistent cargo clippy --workspace --all-targets -- -D warnings` → finishes without warnings (the variable only stops near-workspaces from downloading its sandbox binary).
3. Sandbox integration tests, optional: `scripts/build.sh` (needs cargo-near 0.22.0 and Rust 1.99.1 with the `wasm32-unknown-unknown` target; writes `res/*.wasm` and `res/SHA256SUMS`), then `cargo test -p integration-tests -- --nocapture` (downloads the near-workspaces sandbox binary) → `test result: ok.` for `vault_flows`, `min_gas` and `gas_profile`.
4. Deployable artefact, optional: `scripts/build.sh --reproducible` (Docker, on a clean committed tree) → the vault wasm and its SHA-256, the hash that would be published at deployment.

By reading: [`audits/README.md`](audits/README.md) maps each invariant to the tests that cover it; [`audits/2026-11-08-ai-audits.md`](audits/2026-11-08-ai-audits.md) lists every audit finding with its fix or accepted-risk note; the three AI audit reports and their proof-of-concept tests are in [`audits/reports/`](audits/reports/). `.github/workflows/ci.yml` runs all of the above.

## Develop

```bash
cargo test -p subscription-vault                      # unit + proptest + native parser fuzz
cargo clippy --workspace --all-targets -- -D warnings # NEAR_SANDBOX_BIN_PATH=/nonexistent skips the sandbox download
scripts/build.sh                                      # needs cargo-near 0.22.0 + Rust 1.99.1 + wasm32-unknown-unknown
cargo test -p integration-tests -- --nocapture        # needs res/*.wasm (CI: reproducible vault + mocks) and the sandbox binary
scripts/build.sh --reproducible                       # Docker; the deployable vault wasm and its hash
```
`PROPTEST_CASES` scales the property tests (CI: 256).

## Calling the vault (gas and signers)

| Method | Who | Attach |
|---|---|---|
| `ft_transfer_call(vault, amount, msg)` on the token | the payer, **signing the transaction from their own account** (the payer must be the transaction signer; deposits relayed by a contract — intents.near, bridges, routers, smart wallets — and NEP-366 meta-transactions through a relayer/gasless wallet are refunded with `ERR_UNSUPPORTED_SENDER`) | 1 yoctoNEAR, ≥ 100 Tgas |
| `collect(mandate_id)` | anyone (the billing keeper) | **250 Tgas** (minimum 170 Tgas, else `ERR_GAS`; unused gas is refunded) |
| `cancel(mandate_id)` / `withdraw(mandate_id)` | payer (cancel: or merchant); also via meta-transaction | 1 yoctoNEAR and **≥ 50 Tgas, 100 recommended** — many tools default to 30 Tgas, which fails with `ERR_GAS` |

Details: [`lld/contracts-vault.md`](https://github.com/nearpay-money/nearpay-brain/blob/main/lld/contracts-vault.md) §3.1, §3.3, §3.4.

## Status

Stage 4 step 4.1: subscription vault implemented per LLD v1.5 with unit, property, fuzz and sandbox tests. Two AI-agent audits and a re-audit (ADR-0020) are done and folded in (`audits/2026-11-08-ai-audits.md`, reports in `audits/reports/`); neither audit found a critical or high issue. These are AI audits, not a human security audit.

Not deployed on mainnet. Deployment is a human step (build the reproducible wasm, deploy to the planned account `v1.nearpay-money.near`, remove all access keys, publish the hash). Until then billing's live auto-debit stays off and test mode runs against the sandbox's simulated vault.

## Built with

Code and documentation are written with AI assistance (Claude) under human direction and review. Built for the NEAR Hackathon (Nov 2026). Licence: Apache-2.0 (`LICENSE`).

- [`near-sdk`](https://crates.io/crates/near-sdk) 5.30.1 — contract SDK (the only dependency of the deployed contract)
- [`near-contract-standards`](https://crates.io/crates/near-contract-standards) 5.30.1 — NEP-141 implementation of the test-only mock token
- [`near-workspaces`](https://crates.io/crates/near-workspaces) 0.24.0 — sandbox integration tests
- [`proptest`](https://crates.io/crates/proptest) 1.12.0 — property tests and native fuzzing
- [`libfuzzer-sys`](https://crates.io/crates/libfuzzer-sys) 0.4.13 / [`cargo-fuzz`](https://crates.io/crates/cargo-fuzz) 0.13.2 — coverage-guided fuzzing
- [`cargo-near`](https://crates.io/crates/cargo-near) 0.22.0 (+ image `sourcescan/cargo-near:0.22.0-rust-1.99.1`) — wasm and reproducible builds
- `tokio`, `serde_json`, `anyhow` — integration-test plumbing
