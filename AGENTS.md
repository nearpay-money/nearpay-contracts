# AGENTS.md — nearpay-contracts

You are working in **nearpay-contracts**, one of 27 NearPay repositories (`nearpay-brain/architecture/repositories.md`).
Scope: NEAR smart contracts. v1: subscription-vault (payer-funded, payer-cancellable mandates; permissionless collect).

## Before changing anything

1. Read `../nearpay-brain/AGENTS.md`, then this repo's LLD (`../nearpay-brain/lld/contracts-vault.md`) — it is the implementation spec; do not make design decisions it already makes.
2. Contracts (requests, responses, events, errors) come from `@nearpay/spec`. If one must change, open a PR in `nearpay-spec` first; never hand-write contract types here.
3. Check your change against invariants INV-1…INV-9 in `nearpay-brain/product/requirements.md`.

## Rules for this repo

- No method may move funds except by the rules in ADR-0008; no admin/owner methods over balances.
- Deployments leave the account with NO access keys; upgrades ship as a new versioned account.
- Every change: unit + near-workspaces integration tests + property tests; audits recorded in `audits/`.

## Never

- Import source code from another NearPay repo (depend on published packages/images only).
- Read or write another service's database.
- Add design documents here — update `nearpay-brain` and link to it.
- Commit secrets, private keys or real customer data.
- Merge changes to paths listed in `.github/CODEOWNERS` without the required human review.
