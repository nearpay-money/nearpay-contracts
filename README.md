# nearpay-contracts

> NEAR smart contracts. v1: subscription-vault (payer-funded, payer-cancellable mandates; permissionless collect).

| | |
|---|---|
| Group | On-chain |
| Kind | Rust / NEAR |
| Ships as | accounts: v1.vault.nearpay.near (immutable) |
| Owns | Vault contract, on-chain events |
| Depends on (published artifacts only) | near-sdk (Rust) |
| Roadmap phase | P3 Auto-debit and hardening |

## Documentation

This repo stays surgical: design docs live in [`nearpay-brain`](../nearpay-brain/README.md), not here.

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

Scaffold only. Implementation starts in phase **P3 Auto-debit and hardening** (`nearpay-brain/roadmap/roadmap.md`).
