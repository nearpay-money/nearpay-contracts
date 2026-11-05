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
- `nearpay-brain/lld/audits/contracts-vault.md` — audit findings already folded into the LLD
- `nearpay-brain/roadmap/development-plan.md` — stage and step this repo is built in
- `nearpay-brain/architecture/hld.md` — system design
- `nearpay-brain/architecture/repositories.md` — repo topology and dependency rules
- `nearpay-brain/decisions/ADR-0008-subscriptions.md`
- `nearpay-brain/architecture/flows.md#6-auto_debit-subscription-via-vault`

## Layout

```
nearpay-contracts/
├── audits/
├── contracts/
│   └── subscription-vault/
│       ├── src/
│       └── tests/
├── integration-tests/
├── scripts/
└── .github/
    └── workflows/
```

## Status

Scaffold only. Implementation starts in **Stage 4 (step 4.1)** (`nearpay-brain/roadmap/development-plan.md`).

## Built with

Libraries are listed here as they are added (hackathon rule Q-005). Code and documentation are written with AI assistance (Claude) under human direction and review.
