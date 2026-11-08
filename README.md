# nearpay-contracts

> NEAR smart contracts. v1: subscription-vault (payer-funded, payer-cancellable mandates; permissionless collect).

| | |
|---|---|
| Group | On-chain |
| Kind | Rust / NEAR |
| Ships as | accounts: v1.vault.nearpay.near (immutable) |
| Owns | Vault contract, on-chain events |
| Depends on (published artifacts only) | near-sdk (Rust) |
| First built in | Stage 4 (step 4.1) |

## Documentation

This repo stays surgical: design docs live in [`nearpay-brain`](../nearpay-brain/README.md), not here.

- `nearpay-brain/lld/contracts-vault.md` — **low-level design (dev-ready; implement from this)**
- `nearpay-brain/lld/audits/contracts-vault.md` — LLD audit findings already folded into the LLD
- [`audits/README.md`](audits/README.md) — invariants V1–V10 and the tests that cover each
- `nearpay-brain/roadmap/development-plan.md` — stage and step this repo is built in
- `nearpay-brain/architecture/hld.md` — system design
- `nearpay-brain/architecture/repositories.md` — repo topology and dependency rules
- `nearpay-brain/decisions/ADR-0008-subscriptions.md`
- `nearpay-brain/architecture/flows.md#6-auto_debit-subscription-via-vault`

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

## Develop

```bash
cargo test -p subscription-vault                      # unit + proptest + native parser fuzz
cargo clippy --workspace --all-targets -- -D warnings # NEAR_SANDBOX_BIN_PATH=/nonexistent skips the sandbox download
scripts/build.sh                                      # needs cargo-near 0.22.0 + Rust 1.99.1 + wasm32-unknown-unknown
cargo test -p integration-tests -- --nocapture        # needs res/*.wasm (CI: reproducible vault + mocks) and the sandbox binary
scripts/build.sh --reproducible                       # Docker; the deployable vault wasm and its hash
```
`PROPTEST_CASES` scales the property tests (CI: 256).

## Status

Stage 4 step 4.1: subscription vault implemented per LLD v1.4 (AI audits 2026-11-08 folded in: `audits/2026-11-08-ai-audits.md`) with unit, property, fuzz and sandbox tests; awaiting CI and the independent AI-agent audits (ADR-0020). Not deployed.

## Built with

Libraries are listed here as they are added (hackathon rule Q-005). Code and documentation are written with AI assistance (Claude) under human direction and review.

- [`near-sdk`](https://crates.io/crates/near-sdk) 5.30.1 — contract SDK (the only dependency of the deployed contract)
- [`near-contract-standards`](https://crates.io/crates/near-contract-standards) 5.30.1 — NEP-141 implementation of the test-only mock token
- [`near-workspaces`](https://crates.io/crates/near-workspaces) 0.24.0 — sandbox integration tests
- [`proptest`](https://crates.io/crates/proptest) 1.12.0 — property tests and native fuzzing
- [`libfuzzer-sys`](https://crates.io/crates/libfuzzer-sys) 0.4.13 / [`cargo-fuzz`](https://crates.io/crates/cargo-fuzz) 0.13.2 — coverage-guided fuzzing
- [`cargo-near`](https://crates.io/crates/cargo-near) 0.22.0 (+ image `sourcescan/cargo-near:0.22.0-rust-1.99.1`) — wasm and reproducible builds
- `tokio`, `serde_json`, `anyhow` — integration-test plumbing
