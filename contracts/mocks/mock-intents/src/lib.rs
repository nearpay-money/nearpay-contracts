//! Test-only stand-in for `intents.near` deposits: `ft_on_transfer(sender_id, amount, msg)` with
//! `msg` = the account to credit, as the vault's merchant leg sends it. Balances are kept per
//! `(account, "nep141:<token>")` like the Verifier's multi-token ledger.
//!
//! Per-receiver modes simulate receiver outcomes: `accept` (default), `reject` (refund all),
//! `partial:<permille>` (use part, refund the rest), `panic` (the call fails; the token refunds).
//!
//! Every deposit first burns `burn_tgas` (default 30 Tgas, settable) to stand in for the
//! Verifier's real `ft_on_transfer` cost (large wasm load, storage, `mt_mint` event), so starving
//! the receiver of gas shows up in the sandbox tests.

use near_sdk::json_types::U128;
use near_sdk::store::LookupMap;
use near_sdk::{AccountId, Gas, PanicOnDefault, env, near};

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct MockIntents {
    balances: LookupMap<String, u128>,
    modes: LookupMap<AccountId, String>,
    burn_tgas: u64,
}

#[near]
impl MockIntents {
    #[init]
    pub fn new() -> Self {
        Self { balances: LookupMap::new(b"b".to_vec()), modes: LookupMap::new(b"m".to_vec()), burn_tgas: 30 }
    }

    /// Sets the gas each deposit burns before crediting (sandbox only).
    pub fn set_burn_tgas(&mut self, tgas: u64) {
        self.burn_tgas = tgas;
    }

    /// Sets how deposits for `account_id` are handled (sandbox only).
    pub fn set_mode(&mut self, account_id: AccountId, mode: String) {
        self.modes.insert(account_id, mode);
    }

    pub fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> U128 {
        let _ = sender_id;
        let burn = Gas::from_tgas(self.burn_tgas);
        let mut seed = [0u8; 32];
        while env::used_gas() < burn {
            seed = env::sha256_array(seed);
        }
        let token = env::predecessor_account_id();
        let receiver: AccountId = msg.parse().unwrap_or_else(|_| env::panic_str("invalid receiver"));
        let mode = self.modes.get(&receiver).cloned().unwrap_or_else(|| "accept".to_string());
        let used = match mode.as_str() {
            "reject" => 0,
            "panic" => env::panic_str("receiver panicked"),
            m if m.starts_with("partial:") => {
                let permille: u128 = m["partial:".len()..].parse().unwrap_or(0);
                amount.0 * permille.min(1000) / 1000
            }
            _ => amount.0,
        };
        if used > 0 {
            let key = format!("{receiver}|nep141:{token}");
            let cur = self.balances.get(&key).copied().unwrap_or(0);
            self.balances.insert(key, cur + used);
        }
        U128(amount.0 - used)
    }

    /// NEP-245-style balance view: `token_id` = `nep141:<token account>`.
    pub fn mt_balance_of(&self, account_id: AccountId, token_id: String) -> U128 {
        U128(self.balances.get(&format!("{account_id}|{token_id}")).copied().unwrap_or(0))
    }
}
