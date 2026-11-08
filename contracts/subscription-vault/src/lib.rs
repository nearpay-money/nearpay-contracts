//! NearPay subscription vault (`v1.vault.nearpay.near`).
//!
//! Implementation spec: `nearpay-brain/lld/contracts-vault.md` (v1.3). ADR-0008.
//!
//! * Payers fund mandates with `ft_transfer_call(vault, amount, msg = {"create_mandate": …})`
//!   and top them up the same way (`{"top_up": …}`).
//! * Anyone may `collect(mandate_id)` once per elapsed period: merchant leg → (only if fully
//!   delivered) fee leg → `on_collected`.
//! * The payer (or the merchant) may `cancel` at any time; the remainder goes back to the payer.
//!   A failed refund leaves the balance on the mandate for `withdraw`.
//! * Configuration is constructor-only. There are no owner, admin, setter or upgrade methods, and
//!   no method sends tokens anywhere but to the payer, the merchant receiver (directly or via
//!   `intents_account`) and the fee receiver (invariant V4).

use near_sdk::json_types::{U64, U128};
use near_sdk::store::{IterableSet, LookupMap};
use near_sdk::{
    AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault, Promise, PromiseError, PromiseOrValue,
    assert_one_yocto, env, ext_contract, near, serde_json,
};

pub mod errors;
pub mod events;
pub mod math;
pub mod msg;
mod types;

#[cfg(test)]
mod tests;

pub use types::*;

use errors::*;
use msg::{CreateMandateMsg, VaultMsg};

/// Storage a payer must have available per open mandate (LLD §3): 0.01 NEAR.
pub const MANDATE_STORAGE_COST: NearToken = NearToken::from_millinear(10);
/// NEP-145 registration minimum (covers the payer's own storage record): 0.002 NEAR.
pub const STORAGE_REGISTRATION_COST: NearToken = NearToken::from_millinear(2);

/// Static gas for a NEP-141 `ft_transfer` (merchant NearAccount leg, fee leg, refunds).
pub const GAS_FT_TRANSFER: Gas = Gas::from_tgas(15);
/// Static gas for the merchant `ft_transfer_call` to `intents_account`. Deployed NEP-141 tokens
/// (near-contract-standards 4.x logic) require `prepaid_gas > 30 Tgas` and forward exactly
/// `prepaid − 30 Tgas` to the receiver, so `intents_account`'s `ft_on_transfer` is guaranteed
/// ≥ 70 Tgas whoever calls `collect` (NEAR Intents recommends 100 Tgas for a whole deposit
/// `ft_transfer_call`). Unused gas of `collect` is added on top by weight.
pub const GAS_FT_TRANSFER_CALL: Gas = Gas::from_tgas(100);
/// Static gas for `on_merchant_leg`: its own work plus scheduling the fee leg and `on_collected`.
pub const GAS_ON_MERCHANT_LEG: Gas = Gas::from_tgas(50);
/// Static gas for `on_collected`.
pub const GAS_ON_COLLECTED: Gas = Gas::from_tgas(20);
/// Static gas for `on_refunded`.
pub const GAS_ON_REFUNDED: Gas = Gas::from_tgas(20);
/// Minimum gas attached to `collect` (`ERR_GAS` below it): the 150 Tgas of static gas for the
/// merchant leg and `on_merchant_leg` plus 20 Tgas for `collect` itself. The static gas on each
/// receipt is what guarantees the budgets (a receipt with less cannot be created, and the whole
/// call reverts); the floor only turns an under-gassed call into a clear, early `ERR_GAS`. The
/// keeper attaches 250 Tgas (LLD §3.3; unused gas is refunded; the protocol's prepaid-gas limit
/// is 1 PGas since protocol 83, 300 Tgas before).
pub const MIN_COLLECT_GAS: Gas = Gas::from_tgas(170);
/// Minimum gas attached to `cancel` / `withdraw` (`ERR_GAS` below it): 35 Tgas of static gas for
/// the refund transfer and `on_refunded` plus their own work (≈ 40 Tgas measured) with margin.
/// Common tooling defaults to 30 Tgas; payers attach ≥ 50 Tgas (100 recommended) — LLD v1.5 §3.4.
pub const MIN_EXIT_GAS: Gas = Gas::from_tgas(50);

/// Max bytes read from a token's promise result (`"340282366920938463463374607431768211455"`).
const MAX_RESULT_LEN: usize = 64;
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Mandates,
    AllowedTokens,
    TvlCap,
    Tvl,
    MandateCap,
    StorageAccounts,
    PayerIndex,
    PayerMandates { payer_hash: [u8; 32] },
}

/// NEP-141 methods the vault calls on token contracts.
#[allow(dead_code)]
#[ext_contract(ext_ft)]
trait FungibleTokenCore {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
    fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<U128>;
}

/// Contract state (LLD §2). Every field is set once by `new`; only mandates, TVL, storage
/// balances and the payer index change afterwards, and only through the rules in §3.
#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Vault {
    mandates: LookupMap<u64, Mandate>,
    next_id: u64,
    allowed_tokens: IterableSet<AccountId>,
    fee_receiver: AccountId,
    max_fee_bps: u16,
    tvl_cap: LookupMap<AccountId, u128>,
    /// Per token: max tokens one mandate may hold (balance after a deposit) and max
    /// `amount_per_period` (ADR-0008 per-mandate cap; LLD v1.3).
    mandate_cap: LookupMap<AccountId, u128>,
    /// Per token: Σ `Mandate::held()` — tokens the vault holds on behalf of mandates.
    tvl: LookupMap<AccountId, u128>,
    intents_account: AccountId,
    storage_accounts: LookupMap<AccountId, StorageAccount>,
    payer_mandates: LookupMap<AccountId, IterableSet<u64>>,
}

fn fail(code: &str) -> ! {
    env::panic_str(code)
}

#[near]
impl Vault {
    /// Constructor (deploy-time only; LLD §2, §6). `#[private]`: must be called by the vault
    /// account itself, in the deploy batch, before its access keys are deleted.
    #[init]
    #[private]
    pub fn new(
        allowed_tokens: Vec<AccountId>,
        fee_receiver: AccountId,
        max_fee_bps: u16,
        tvl_caps: Vec<TokenCap>,
        intents_account: AccountId,
    ) -> Self {
        assert_private();
        let me = env::current_account_id();
        if allowed_tokens.is_empty()
            || max_fee_bps > 10_000
            || fee_receiver == me
            || intents_account == me
            || fee_receiver == intents_account
            || allowed_tokens.contains(&me)
            || allowed_tokens.contains(&fee_receiver)
            || allowed_tokens.contains(&intents_account)
            || tvl_caps.len() != allowed_tokens.len()
        {
            fail(ERR_BAD_CONFIG);
        }
        let mut tokens = IterableSet::new(StorageKey::AllowedTokens);
        for t in &allowed_tokens {
            if !tokens.insert(t.clone()) {
                fail(ERR_BAD_CONFIG); // duplicate token
            }
        }
        let mut caps = LookupMap::new(StorageKey::TvlCap);
        let mut mandate_caps = LookupMap::new(StorageKey::MandateCap);
        let mut tvl = LookupMap::new(StorageKey::Tvl);
        for c in &tvl_caps {
            if !tokens.contains(&c.token)
                || c.mandate_cap.0 == 0
                || c.mandate_cap.0 > c.cap.0
                || caps.insert(c.token.clone(), c.cap.0).is_some()
            {
                // cap for an unknown token, zero caps, mandate cap above the TVL cap, or two caps
                // for one token (mandate_cap > 0 and mandate_cap ≤ cap imply cap > 0)
                fail(ERR_BAD_CONFIG);
            }
            mandate_caps.insert(c.token.clone(), c.mandate_cap.0);
            tvl.insert(c.token.clone(), 0u128);
        }
        Self {
            mandates: LookupMap::new(StorageKey::Mandates),
            next_id: 0,
            allowed_tokens: tokens,
            fee_receiver,
            max_fee_bps,
            tvl_cap: caps,
            mandate_cap: mandate_caps,
            tvl,
            intents_account,
            storage_accounts: LookupMap::new(StorageKey::StorageAccounts),
            payer_mandates: LookupMap::new(StorageKey::PayerIndex),
        }
    }

    // ------------------------------------------------------------------ funding

    /// NEP-141 receiver. Creates or tops up a mandate. Returns the unused amount: `0` when
    /// accepted, the full `amount` when rejected (the rejection is logged as one line starting
    /// with a stable error string, e.g. `ERR_BAD_MSG` or `ERR_CAP_EXCEEDED: mandate`). Calls from
    /// a token outside `allowed_tokens` panic.
    pub fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> U128 {
        let token = env::predecessor_account_id();
        if !self.allowed_tokens.contains(&token) {
            fail(ERR_TOKEN_NOT_ALLOWED);
        }
        let result = if sender_id == self.intents_account
            || sender_id == env::current_account_id()
            || sender_id != env::signer_account_id()
        {
            // The payer must be the account that signed the transaction (a direct
            // `ft_transfer_call` signed by the payer), so the payer can always cancel and refunds
            // reach a human. A deposit relayed by any contract — intents.near (Intents
            // `ft_withdraw` with msg, 1Click `customRecipientMsg`), a bridge, a router, a smart
            // wallet — would make the relayer the payer (AI audits A1-M-2, A2-M-1, re-audit N-1).
            // NEP-366 meta-transaction deposits are refused too: their inner receipt's signer is
            // the relayer, not the user, so gasless/relayer wallets cannot fund or top up in v1
            // (cancel/withdraw via meta-tx still work: they check the predecessor). The intents
            // check is subsumed by the signer check but kept explicit. Smart-wallet payers are
            // unsupported in v1.
            Err(ERR_UNSUPPORTED_SENDER)
        } else if amount.0 == 0 {
            Err(ERR_BAD_MSG)
        } else {
            match msg::parse_msg(&msg) {
                Ok(VaultMsg::CreateMandate(m)) => self.internal_create(&token, &sender_id, amount.0, m),
                Ok(VaultMsg::TopUp(t)) => self.internal_top_up(&token, &sender_id, amount.0, t.mandate_id),
                Err(code) => Err(code),
            }
        };
        match result {
            Ok(()) => U128(0),
            Err(code) => {
                env::log_str(code);
                amount
            }
        }
    }

    // ------------------------------------------------------------------ collect

    /// Permissionless. Collects the next due period of an active mandate: merchant leg →
    /// (if fully delivered) fee leg → `on_collected`. Panics with a stable error string when the
    /// period cannot be collected (checked in this order): `ERR_GAS` (less than
    /// `MIN_COLLECT_GAS` attached), `ERR_NOT_FOUND`, `ERR_MAX_PERIODS`
    /// (exhausted), `ERR_NOT_ACTIVE` (canceled), `ERR_IN_FLIGHT`, `ERR_MAX_PERIODS`,
    /// `ERR_NOT_DUE`, `ERR_INSUFFICIENT_BALANCE`.
    pub fn collect(&mut self, mandate_id: u64) -> Promise {
        if env::prepaid_gas() < MIN_COLLECT_GAS {
            fail(ERR_GAS);
        }
        let now = env::block_timestamp();
        let m = self.mandates.get_mut(&mandate_id).unwrap_or_else(|| fail(ERR_NOT_FOUND));
        match m.status {
            Status::Active => {}
            Status::Exhausted => fail(ERR_MAX_PERIODS),
            Status::Canceled => fail(ERR_NOT_ACTIVE),
        }
        if m.in_flight {
            fail(ERR_IN_FLIGHT);
        }
        if m.max_reached() {
            fail(ERR_MAX_PERIODS);
        }
        match math::due_at(m.start_at_ns, m.collected_periods, m.period_secs) {
            Some(due) if now >= due => {}
            _ => fail(ERR_NOT_DUE),
        }
        if m.balance < m.amount_per_period {
            fail(ERR_INSUFFICIENT_BALANCE);
        }
        let (share, fee) = math::split(m.amount_per_period, m.fee_bps);
        // Move the period's amount out of the spendable balance while it is in flight, so a
        // concurrent cancel/withdraw can never refund it (V1).
        m.balance -= m.amount_per_period;
        m.in_flight = true;

        let merchant_leg = match m.receiver_kind {
            ReceiverKind::IntentsAccount => ext_ft::ext(m.token.clone())
                .with_attached_deposit(ONE_YOCTO)
                .with_static_gas(GAS_FT_TRANSFER_CALL)
                .with_unused_gas_weight(1)
                .ft_transfer_call(
                    self.intents_account.clone(),
                    U128(share),
                    None,
                    m.merchant_receiver.to_string(),
                ),
            ReceiverKind::NearAccount => ext_ft::ext(m.token.clone())
                .with_attached_deposit(ONE_YOCTO)
                .with_static_gas(GAS_FT_TRANSFER)
                .with_unused_gas_weight(1)
                .ft_transfer(m.merchant_receiver.clone(), U128(share), None),
        };
        merchant_leg.then(
            Self::ext(env::current_account_id())
                .with_static_gas(GAS_ON_MERCHANT_LEG)
                .with_unused_gas_weight(1)
                .on_merchant_leg(mandate_id, U128(share), U128(fee)),
        )
    }

    /// Callback after the merchant leg. If the full share was delivered and there is a fee,
    /// sends the fee leg and continues in `on_collected`; otherwise settles the period here.
    #[private]
    pub fn on_merchant_leg(&mut self, mandate_id: u64, share: U128, fee: U128) -> PromiseOrValue<bool> {
        assert_private();
        let Some(m) = self.mandates.get(&mandate_id) else {
            return PromiseOrValue::Value(false); // unreachable: in-flight mandates are never removed
        };
        if !m.in_flight {
            return PromiseOrValue::Value(false); // unreachable: one collect in flight at a time
        }
        // Amounts are recomputed from the (immutable) mandate terms; the arguments are
        // informational (they make the receipt self-describing in explorers).
        let _ = (share, fee);
        let (share, fee) = math::split(m.amount_per_period, m.fee_bps);
        let used = merchant_leg_delivered(m.receiver_kind, share);
        if used == share && fee > 0 {
            let fee_leg = ext_ft::ext(m.token.clone())
                .with_attached_deposit(ONE_YOCTO)
                .with_static_gas(GAS_FT_TRANSFER)
                .with_unused_gas_weight(1)
                .ft_transfer(self.fee_receiver.clone(), U128(fee), None);
            return PromiseOrValue::Promise(
                fee_leg.then(
                    Self::ext(env::current_account_id())
                        .with_static_gas(GAS_ON_COLLECTED)
                        .with_unused_gas_weight(1)
                        .on_collected(mandate_id, U128(share), U128(fee)),
                ),
            );
        }
        PromiseOrValue::Value(self.settle_collect(mandate_id, used, false))
    }

    /// Final collect callback (merchant leg fully delivered; fee leg result inspected here).
    #[private]
    pub fn on_collected(&mut self, mandate_id: u64, share: U128, fee: U128) -> bool {
        assert_private();
        let _ = fee; // informational, see on_merchant_leg
        let fee_sent = transfer_succeeded();
        // Only reached when the merchant leg delivered the full share.
        self.settle_collect(mandate_id, share.0, fee_sent)
    }

    // ------------------------------------------------------------------ exit

    /// Payer or merchant receiver; requires exactly 1 yoctoNEAR (full-access key) and at least
    /// `MIN_EXIT_GAS` (50 Tgas; `ERR_GAS` below, checked first). Sets
    /// `canceled` and refunds the whole balance to the payer. The `canceled` event is emitted when
    /// the refund outcome is known (`refunded` = amount actually returned; on a failed refund the
    /// balance stays on the mandate for `withdraw`).
    #[payable]
    pub fn cancel(&mut self, mandate_id: u64) -> PromiseOrValue<bool> {
        if env::prepaid_gas() < MIN_EXIT_GAS {
            fail(ERR_GAS);
        }
        assert_one_yocto();
        let caller = env::predecessor_account_id();
        let m = self.mandates.get_mut(&mandate_id).unwrap_or_else(|| fail(ERR_NOT_FOUND));
        if caller != m.payer && caller != m.merchant_receiver {
            fail(ERR_UNAUTHORIZED);
        }
        if m.status != Status::Active {
            fail(ERR_NOT_ACTIVE);
        }
        m.status = Status::Canceled;
        if m.balance == 0 {
            events::canceled(events::Canceled { mandate_id, by: &caller, refunded: U128(0) });
            self.maybe_close(mandate_id);
            return PromiseOrValue::Value(true);
        }
        let refund = m.balance;
        let payer = m.payer.clone();
        let token = m.token.clone();
        m.balance = 0;
        m.refund_in_flight = refund;
        PromiseOrValue::Promise(Self::refund_promise(token, payer, mandate_id, refund, Some(caller)))
    }

    /// Payer only; requires exactly 1 yoctoNEAR and `MIN_EXIT_GAS` (`ERR_GAS`, checked first). Returns
    /// the balance of a `canceled` or
    /// `exhausted` mandate. Errors: `ERR_NOT_FOUND`, `ERR_UNAUTHORIZED`, `ERR_STILL_ACTIVE`,
    /// `ERR_IN_FLIGHT` (a refund is pending), `ERR_INSUFFICIENT_BALANCE` (nothing to withdraw).
    #[payable]
    pub fn withdraw(&mut self, mandate_id: u64) -> Promise {
        if env::prepaid_gas() < MIN_EXIT_GAS {
            fail(ERR_GAS);
        }
        assert_one_yocto();
        let caller = env::predecessor_account_id();
        let m = self.mandates.get_mut(&mandate_id).unwrap_or_else(|| fail(ERR_NOT_FOUND));
        if caller != m.payer {
            fail(ERR_UNAUTHORIZED);
        }
        if m.status == Status::Active {
            fail(ERR_STILL_ACTIVE);
        }
        if m.refund_in_flight > 0 {
            fail(ERR_IN_FLIGHT);
        }
        if m.balance == 0 {
            fail(ERR_INSUFFICIENT_BALANCE);
        }
        let refund = m.balance;
        m.balance = 0;
        m.refund_in_flight = refund;
        Self::refund_promise(m.token.clone(), m.payer.clone(), mandate_id, refund, None)
    }

    /// Refund callback (`cancel` when `canceled_by` is set, else `withdraw`).
    #[private]
    pub fn on_refunded(&mut self, mandate_id: u64, amount: U128, canceled_by: Option<AccountId>) -> bool {
        assert_private();
        let ok = transfer_succeeded();
        let Some(m) = self.mandates.get_mut(&mandate_id) else {
            return false; // unreachable: mandates with a refund in flight are never removed
        };
        m.refund_in_flight = m.refund_in_flight.saturating_sub(amount.0);
        let token = m.token.clone();
        if ok {
            sub_tvl(&mut self.tvl, &token, amount.0);
        } else {
            m.balance += amount.0;
        }
        match canceled_by {
            Some(by) => events::canceled(events::Canceled {
                mandate_id,
                by: &by,
                refunded: U128(if ok { amount.0 } else { 0 }),
            }),
            None if ok => events::withdrawn(events::Withdrawn { mandate_id, amount }),
            None => {}
        }
        self.maybe_close(mandate_id);
        ok
    }

    // ------------------------------------------------------------------ NEP-145 storage

    /// NEP-145. Registers `account_id` (default: caller) or adds to its storage balance.
    #[payable]
    pub fn storage_deposit(
        &mut self,
        account_id: Option<AccountId>,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        let amount = env::attached_deposit().as_yoctonear();
        let account = account_id.unwrap_or_else(env::predecessor_account_id);
        let registration_only = registration_only.unwrap_or(false);
        let min = STORAGE_REGISTRATION_COST.as_yoctonear();
        let refund = if let Some(acct) = self.storage_accounts.get_mut(&account) {
            if registration_only {
                amount
            } else {
                acct.total += amount;
                0
            }
        } else {
            if amount < min {
                fail(ERR_NO_STORAGE);
            }
            let keep = if registration_only { min } else { amount };
            self.storage_accounts.insert(account.clone(), StorageAccount { total: keep, mandates: 0 });
            amount - keep
        };
        if refund > 0 {
            Promise::new(env::predecessor_account_id()).transfer(NearToken::from_yoctonear(refund)).detach();
        }
        self.storage_balance_of(account).unwrap_or_else(|| env::abort())
    }

    /// NEP-145. Withdraws up to the available (unlocked) storage balance; 1 yoctoNEAR required.
    #[payable]
    pub fn storage_withdraw(&mut self, amount: Option<U128>) -> StorageBalance {
        assert_one_yocto();
        let account = env::predecessor_account_id();
        let acct = self.storage_accounts.get_mut(&account).unwrap_or_else(|| fail(ERR_NO_STORAGE));
        let available = storage_available(acct);
        let amount = amount.map_or(available, |a| a.0);
        if amount > available {
            fail(ERR_INSUFFICIENT_BALANCE);
        }
        acct.total -= amount;
        if amount > 0 {
            Promise::new(account.clone()).transfer(NearToken::from_yoctonear(amount)).detach();
        }
        self.storage_balance_of(account).unwrap_or_else(|| env::abort())
    }

    /// NEP-145. Unregisters the caller and returns its whole storage balance; 1 yoctoNEAR
    /// required. Panics with `ERR_OPEN_MANDATES` while the caller has open mandates (`force` is
    /// not supported: mandates are closed only by their own rules).
    #[payable]
    pub fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        assert_one_yocto();
        let _ = force;
        let account = env::predecessor_account_id();
        let Some(acct) = self.storage_accounts.get(&account) else {
            return false;
        };
        if acct.mandates > 0 {
            fail(ERR_OPEN_MANDATES);
        }
        let total = acct.total;
        self.storage_accounts.remove(&account);
        if total > 0 {
            Promise::new(account).transfer(NearToken::from_yoctonear(total)).detach();
        }
        true
    }

    /// NEP-145.
    pub fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        StorageBalanceBounds { min: U128(STORAGE_REGISTRATION_COST.as_yoctonear()), max: None }
    }

    /// NEP-145.
    pub fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.storage_accounts
            .get(&account_id)
            .map(|a| StorageBalance { total: U128(a.total), available: U128(storage_available(a)) })
    }

    // ------------------------------------------------------------------ views

    pub fn get_mandate(&self, mandate_id: u64) -> Option<MandateView> {
        self.mandates.get(&mandate_id).map(|m| view(mandate_id, m))
    }

    /// Open mandates of `payer`, paginated (`limit` defaults to 50, max 100). Order is stable
    /// between writes but not by id.
    pub fn get_mandates_by_payer(
        &self,
        payer: AccountId,
        from_index: Option<u32>,
        limit: Option<u32>,
    ) -> Vec<MandateView> {
        let Some(ids) = self.payer_mandates.get(&payer) else {
            return vec![];
        };
        let from = from_index.unwrap_or(0) as usize;
        let limit = limit.unwrap_or(50).min(100) as usize;
        ids.iter()
            .skip(from)
            .take(limit)
            .filter_map(|id| self.mandates.get(id).map(|m| view(*id, m)))
            .collect()
    }

    /// When the next period becomes collectable (ns), for an active mandate below `max_periods`.
    pub fn next_collect_at(&self, mandate_id: u64) -> Option<U64> {
        self.mandates.get(&mandate_id).and_then(mandate_next_collect_at)
    }

    pub fn get_config(&self) -> ConfigView {
        ConfigView {
            allowed_tokens: self.allowed_tokens.iter().cloned().collect(),
            fee_receiver: self.fee_receiver.clone(),
            max_fee_bps: self.max_fee_bps,
            intents_account: self.intents_account.clone(),
            min_period_secs: msg::MIN_PERIOD_SECS,
            caps: self
                .allowed_tokens
                .iter()
                .map(|t| TokenCap {
                    token: t.clone(),
                    cap: U128(self.tvl_cap.get(t).copied().unwrap_or(0)),
                    mandate_cap: U128(self.mandate_cap.get(t).copied().unwrap_or(0)),
                })
                .collect(),
            mandate_storage_cost: U128(MANDATE_STORAGE_COST.as_yoctonear()),
            storage_registration_cost: U128(STORAGE_REGISTRATION_COST.as_yoctonear()),
        }
    }

    pub fn get_token_tvl(&self, token: AccountId) -> Option<TokenTvlView> {
        let cap = *self.tvl_cap.get(&token)?;
        let tvl = self.tvl.get(&token).copied().unwrap_or(0);
        let mandate_cap = self.mandate_cap.get(&token).copied().unwrap_or(0);
        Some(TokenTvlView { token, tvl: U128(tvl), cap: U128(cap), mandate_cap: U128(mandate_cap) })
    }
}

// ---------------------------------------------------------------------- internals

impl Vault {
    fn internal_create(
        &mut self,
        token: &AccountId,
        payer: &AccountId,
        amount: u128,
        m: CreateMandateMsg,
    ) -> Result<(), &'static str> {
        msg::validate_create(&m, self.max_fee_bps, &env::current_account_id(), env::block_timestamp())?;
        let acct = self.storage_accounts.get(payer).ok_or(ERR_NO_STORAGE)?;
        if storage_available(acct) < MANDATE_STORAGE_COST.as_yoctonear() {
            return Err(ERR_NO_STORAGE);
        }
        let mandate_cap = self.mandate_cap_of(token);
        if m.amount_per_period.0 > mandate_cap || amount > mandate_cap {
            return Err(LOG_CAP_EXCEEDED_MANDATE);
        }
        let new_tvl = self.tvl_after_deposit(token, amount)?;
        // ---- all checks passed: mutate
        let now = env::block_timestamp();
        let start_at_ns = m.start_at.map_or(now, |s| s.0.max(now));
        let mandate_id = self.next_id;
        self.next_id += 1;
        let mandate = Mandate {
            payer: payer.clone(),
            merchant_receiver: m.merchant_receiver,
            receiver_kind: m.receiver_kind,
            token: token.clone(),
            amount_per_period: m.amount_per_period.0,
            period_secs: m.period_secs,
            start_at_ns,
            max_periods: m.max_periods,
            fee_bps: m.fee_bps,
            collected_periods: 0,
            balance: amount,
            status: Status::Active,
            in_flight: false,
            refund_in_flight: 0,
            ext_ref: m.ext_ref,
        };
        events::mandate_created(events::MandateCreated {
            mandate_id,
            payer,
            merchant_receiver: &mandate.merchant_receiver,
            token,
            amount_per_period: U128(mandate.amount_per_period),
            period_secs: mandate.period_secs,
            start_at: U64(start_at_ns),
            max_periods: mandate.max_periods,
            fee_bps: mandate.fee_bps,
            ext_ref: &mandate.ext_ref,
            receiver_kind: mandate.receiver_kind,
            balance: U128(amount),
        });
        self.mandates.insert(mandate_id, mandate);
        self.tvl.insert(token.clone(), new_tvl);
        if let Some(acct) = self.storage_accounts.get_mut(payer) {
            acct.mandates += 1;
        }
        let ids = self.payer_mandates.entry(payer.clone()).or_insert_with(|| {
            IterableSet::new(StorageKey::PayerMandates { payer_hash: env::sha256_array(payer.as_bytes()) })
        });
        ids.insert(mandate_id);
        ids.flush();
        Ok(())
    }

    fn internal_top_up(
        &mut self,
        token: &AccountId,
        sender: &AccountId,
        amount: u128,
        mandate_id: u64,
    ) -> Result<(), &'static str> {
        let m = self.mandates.get(&mandate_id).ok_or(ERR_NOT_FOUND)?;
        if &m.payer != sender {
            return Err(ERR_UNAUTHORIZED);
        }
        if &m.token != token {
            return Err(ERR_TOKEN_NOT_ALLOWED);
        }
        if m.status != Status::Active {
            return Err(ERR_NOT_ACTIVE);
        }
        // Per-mandate cap on what the mandate holds after the deposit (an in-flight collect or
        // refund still counts: it may be credited back).
        let held_after = m.held().checked_add(amount).ok_or(LOG_CAP_EXCEEDED_MANDATE)?;
        if held_after > self.mandate_cap_of(token) {
            return Err(LOG_CAP_EXCEEDED_MANDATE);
        }
        let new_balance = m.balance + amount; // ≤ held_after, no overflow
        let new_tvl = self.tvl_after_deposit(token, amount)?;
        // ---- all checks passed: mutate
        self.tvl.insert(token.clone(), new_tvl);
        if let Some(m) = self.mandates.get_mut(&mandate_id) {
            m.balance = new_balance;
        }
        events::topped_up(events::ToppedUp { mandate_id, amount: U128(amount), balance: U128(new_balance) });
        Ok(())
    }

    fn tvl_after_deposit(&self, token: &AccountId, amount: u128) -> Result<u128, &'static str> {
        let cap = self.tvl_cap.get(token).copied().unwrap_or(0);
        let tvl = self.tvl.get(token).copied().unwrap_or(0);
        match tvl.checked_add(amount) {
            Some(t) if t <= cap => Ok(t),
            _ => Err(LOG_CAP_EXCEEDED_TVL),
        }
    }

    fn mandate_cap_of(&self, token: &AccountId) -> u128 {
        self.mandate_cap.get(token).copied().unwrap_or(0)
    }

    /// Settles an in-flight collect. `used` = merchant share actually delivered (capped at the
    /// share); `fee_sent` = fee leg succeeded. Returns `true` if the period was counted.
    ///
    /// * `used == share`: period counted; balance keeps the fee if the fee leg failed.
    /// * `0 < used < share` (partial delivery, not expected from compliant receivers): period
    ///   counted with the delivered amount (V3 holds; payer-favourable), no fee was sent.
    /// * `used == 0`: period not counted, whole amount credited back, `collect_failed`.
    fn settle_collect(&mut self, mandate_id: u64, used: u128, fee_sent: bool) -> bool {
        let Some(m) = self.mandates.get_mut(&mandate_id) else {
            return false;
        };
        if !m.in_flight {
            return false;
        }
        let amount = m.amount_per_period;
        let (share, fee) = math::split(amount, m.fee_bps);
        let used = used.min(share);
        let fee_paid = if fee_sent && used == share { fee } else { 0 };
        let sent = used + fee_paid; // ≤ share + fee = amount
        m.balance += amount - sent;
        m.in_flight = false;
        let period = m.collected_periods + 1;
        let token = m.token.clone();
        let counted = used > 0;
        let mut exhausted = false;
        if counted {
            m.collected_periods = period;
            events::collected(events::Collected {
                mandate_id,
                period,
                amount: U128(sent),
                merchant_share: U128(used),
                fee: U128(fee_paid),
                balance: U128(m.balance),
            });
            if m.status == Status::Active && m.max_reached() {
                m.status = Status::Exhausted;
                exhausted = true;
            }
        } else {
            events::collect_failed(events::CollectFailed {
                mandate_id,
                period,
                reason: events::REASON_MERCHANT_TRANSFER_FAILED,
            });
        }
        if exhausted {
            events::exhausted(events::Exhausted { mandate_id });
        }
        sub_tvl(&mut self.tvl, &token, sent);
        self.maybe_close(mandate_id);
        counted
    }

    /// Removes a closed mandate whose balance is zero with nothing in flight, and releases its
    /// storage lock to the payer's NEP-145 available balance (LLD v1.2 §3.4).
    fn maybe_close(&mut self, mandate_id: u64) {
        let closable = self.mandates.get(&mandate_id).is_some_and(|m| {
            m.status != Status::Active && !m.in_flight && m.refund_in_flight == 0 && m.balance == 0
        });
        if !closable {
            return;
        }
        let Some(m) = self.mandates.remove(&mandate_id) else {
            return;
        };
        let mut now_empty = false;
        if let Some(ids) = self.payer_mandates.get_mut(&m.payer) {
            ids.remove(&mandate_id);
            ids.flush();
            now_empty = ids.is_empty();
        }
        if now_empty {
            self.payer_mandates.remove(&m.payer);
        }
        if let Some(acct) = self.storage_accounts.get_mut(&m.payer) {
            acct.mandates = acct.mandates.saturating_sub(1);
        }
    }

    fn refund_promise(
        token: AccountId,
        payer: AccountId,
        mandate_id: u64,
        amount: u128,
        canceled_by: Option<AccountId>,
    ) -> Promise {
        ext_ft::ext(token)
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FT_TRANSFER)
            .with_unused_gas_weight(1)
            .ft_transfer(payer, U128(amount), None)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_ON_REFUNDED)
                    .with_unused_gas_weight(1)
                    .on_refunded(mandate_id, U128(amount), canceled_by),
            )
    }
}

/// Defence in depth for `#[private]` (which near-sdk enforces in the wasm entry point): the
/// method bodies check it too.
fn assert_private() {
    near_sdk::assert_self(); // panics "Method is private"
}

fn sub_tvl(tvl: &mut LookupMap<AccountId, u128>, token: &AccountId, amount: u128) {
    if amount == 0 {
        return;
    }
    let cur = tvl.get(token).copied().unwrap_or(0);
    tvl.insert(token.clone(), cur.saturating_sub(amount));
}

fn storage_available(a: &StorageAccount) -> u128 {
    let locked = STORAGE_REGISTRATION_COST.as_yoctonear()
        + u128::from(a.mandates) * MANDATE_STORAGE_COST.as_yoctonear();
    a.total.saturating_sub(locked)
}

/// `true` unless the single promise result of this callback failed. (An over-long success
/// value is still a success.)
fn transfer_succeeded() -> bool {
    !matches!(env::promise_result_checked(0, MAX_RESULT_LEN), Err(PromiseError::Failed))
}

/// Merchant share actually delivered by the merchant leg (promise result 0).
///
/// * Failed promise → 0 (the token's first receipt failed; nothing moved).
/// * `NearAccount` success → `share`.
/// * `IntentsAccount` success → the `U128` returned by `ft_transfer_call` (the used amount per
///   NEP-141 `ft_resolve_transfer`), capped at `share`. An unparseable or over-long success value
///   is treated as fully used: the token reported success, and assuming the tokens left keeps
///   the vault solvent (V1) — any tokens actually refunded stay in the vault, unattributed.
fn merchant_leg_delivered(kind: ReceiverKind, share: u128) -> u128 {
    match env::promise_result_checked(0, MAX_RESULT_LEN) {
        Err(PromiseError::Failed) => 0,
        Err(_) => share,
        Ok(bytes) => match kind {
            ReceiverKind::NearAccount => share,
            ReceiverKind::IntentsAccount => {
                serde_json::from_slice::<U128>(&bytes).map(|u| u.0.min(share)).unwrap_or(share)
            }
        },
    }
}

fn mandate_next_collect_at(m: &Mandate) -> Option<U64> {
    if m.status != Status::Active || m.max_reached() {
        return None;
    }
    math::due_at(m.start_at_ns, m.collected_periods, m.period_secs).map(U64)
}

fn view(mandate_id: u64, m: &Mandate) -> MandateView {
    MandateView {
        mandate_id,
        payer: m.payer.clone(),
        merchant_receiver: m.merchant_receiver.clone(),
        receiver_kind: m.receiver_kind,
        token: m.token.clone(),
        amount_per_period: U128(m.amount_per_period),
        period_secs: m.period_secs,
        start_at: U64(m.start_at_ns),
        max_periods: m.max_periods,
        fee_bps: m.fee_bps,
        collected_periods: m.collected_periods,
        balance: U128(m.balance),
        status: m.status,
        in_flight: m.in_flight,
        refund_in_flight: U128(m.refund_in_flight),
        ext_ref: m.ext_ref.clone(),
        next_collect_at: mandate_next_collect_at(m),
    }
}
