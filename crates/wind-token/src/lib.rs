//! WIND governance token for Wind Swap v4 on NEAR. NEP-141, 18 decimals.
//!
//! Design (see docs/plan.md, "The WIND Token: NEP-141 on NEAR"):
//!
//! - Only `minter_id` (the Wind Swap v4 contract account) can mint. The owner cannot mint.
//! - Supply is created only by `mint`, which the minter calls once per epoch for emissions
//!   and for the one-time migration of existing WIND.
//! - `burn` lets any holder destroy their own tokens. Needed for migration accounting.
//! - Owner can rotate the minter (`set_minter`) and the owner (`set_owner`). Nothing else.
//! - No pause, no blacklist, no freeze, by design. Transfers can never be stopped.
//! - NEP-141 core, NEP-145 storage management and NEP-148 metadata come from
//!   `near-contract-standards`.
//!
//! Storage: `mint` registers an unregistered recipient. The storage cost is paid from this
//! contract's NEAR balance, not by the minter, so the contract must be funded for storage.
//! Holders who register themselves via `storage_deposit` pay their own storage.
//!
//! Trust model: whoever holds the owner key can change the minter. Set `owner_id` to a
//! governance account the community controls before mainnet.

use near_contract_standards::fungible_token::core::FungibleTokenCore;
use near_contract_standards::fungible_token::events::{FtBurn, FtMint};
use near_contract_standards::fungible_token::metadata::{
    FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC,
};
use near_contract_standards::fungible_token::resolver::FungibleTokenResolver;
use near_contract_standards::fungible_token::FungibleToken;
use near_contract_standards::storage_management::{
    StorageBalance, StorageBalanceBounds, StorageManagement,
};
use near_sdk::collections::LazyOption;
use near_sdk::json_types::U128;
use near_sdk::{
    env, log, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault, PromiseOrValue,
};

/// WIND uses 18 decimals, matching the EVM WIND so ported Solidity math needs no scaling.
pub const DECIMALS: u8 = 18;

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    FungibleToken,
    Metadata,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner_id: AccountId,
    minter_id: AccountId,
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
}

fn wind_metadata() -> FungibleTokenMetadata {
    FungibleTokenMetadata {
        spec: FT_METADATA_SPEC.to_string(),
        name: "Wind".to_string(),
        symbol: "WIND".to_string(),
        icon: None,
        reference: None,
        reference_hash: None,
        decimals: DECIMALS,
    }
}

#[near]
impl Contract {
    /// Creates WIND with zero supply. `minter_id` is the only account that can mint,
    /// and in production it must be the Wind Swap v4 contract account.
    #[init]
    pub fn new(owner_id: AccountId, minter_id: AccountId) -> Self {
        let metadata = wind_metadata();
        metadata.assert_valid();
        Self {
            owner_id,
            minter_id,
            token: FungibleToken::new(StorageKey::FungibleToken),
            metadata: LazyOption::new(StorageKey::Metadata, Some(&metadata)),
        }
    }

    /// Mints `amount` to `account_id`. Only the minter may call.
    /// Registers `account_id` first if needed (contract pays the storage). Emits NEP-141 `ft_mint`.
    pub fn mint(&mut self, account_id: AccountId, amount: U128) {
        require!(
            env::predecessor_account_id() == self.minter_id,
            "Only the minter can mint"
        );
        require!(amount.0 > 0, "Amount must be positive");
        if !self.token.accounts.contains_key(&account_id) {
            self.token.internal_register_account(&account_id);
        }
        self.token.internal_deposit(&account_id, amount.0);
        FtMint {
            owner_id: &account_id,
            amount,
            memo: None,
        }
        .emit();
    }

    /// Burns `amount` of the caller's own WIND and emits NEP-141 `ft_burn`.
    /// Reduces total supply. Anyone can burn their own tokens, nobody else's.
    pub fn burn(&mut self, amount: U128) {
        require!(amount.0 > 0, "Amount must be positive");
        let account_id = env::predecessor_account_id();
        // internal_withdraw also reduces total_supply.
        self.token.internal_withdraw(&account_id, amount.0);
        FtBurn {
            owner_id: &account_id,
            amount,
            memo: None,
        }
        .emit();
    }

    /// Owner only. Replaces the account allowed to mint.
    pub fn set_minter(&mut self, new_minter: AccountId) {
        self.assert_owner();
        log!("Minter changed from {} to {}", self.minter_id, new_minter);
        self.minter_id = new_minter;
    }

    /// Owner only. Replaces the owner.
    pub fn set_owner(&mut self, new_owner: AccountId) {
        self.assert_owner();
        log!("Owner changed from {} to {}", self.owner_id, new_owner);
        self.owner_id = new_owner;
    }

    pub fn owner(&self) -> AccountId {
        self.owner_id.clone()
    }

    pub fn minter(&self) -> AccountId {
        self.minter_id.clone()
    }

    fn assert_owner(&self) {
        require!(
            env::predecessor_account_id() == self.owner_id,
            "Only the owner can call this"
        );
    }
}

#[near]
impl FungibleTokenCore for Contract {
    #[payable]
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        self.token.ft_transfer(receiver_id, amount, memo);
    }

    #[payable]
    fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<U128> {
        self.token.ft_transfer_call(receiver_id, amount, memo, msg)
    }

    fn ft_total_supply(&self) -> U128 {
        self.token.ft_total_supply()
    }

    fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.token.ft_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenResolver for Contract {
    #[private]
    fn ft_resolve_transfer(
        &mut self,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: U128,
    ) -> U128 {
        let (used_amount, burned_amount) =
            self.token
                .internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        if burned_amount > 0 {
            log!("Account @{} burned {}", sender_id, burned_amount);
        }
        used_amount.into()
    }
}

#[near]
impl StorageManagement for Contract {
    #[payable]
    fn storage_deposit(
        &mut self,
        account_id: Option<AccountId>,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        self.token.storage_deposit(account_id, registration_only)
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.token.storage_withdraw(amount)
    }

    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        self.token.storage_unregister(force)
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.token.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.token.storage_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenMetadataProvider for Contract {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        self.metadata.get().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use near_sdk::test_utils::{accounts, get_logs, VMContextBuilder};
    use near_sdk::testing_env;

    const YOCTO: NearToken = NearToken::from_yoctonear(1);

    fn owner() -> AccountId {
        accounts(4)
    }

    fn minter() -> AccountId {
        accounts(3)
    }

    fn setup() -> Contract {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(owner())
            .build());
        Contract::new(owner(), minter())
    }

    fn as_account(account: AccountId, deposit: NearToken) {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(account)
            .attached_deposit(deposit)
            .build());
    }

    #[test]
    fn init_sets_metadata_zero_supply_and_roles() {
        let contract = setup();
        let meta = contract.ft_metadata();
        assert_eq!(meta.spec, FT_METADATA_SPEC);
        assert_eq!(meta.name, "Wind");
        assert_eq!(meta.symbol, "WIND");
        assert_eq!(meta.decimals, 18);
        assert!(meta.icon.is_none());
        assert_eq!(contract.ft_total_supply().0, 0);
        assert_eq!(contract.owner(), owner());
        assert_eq!(contract.minter(), minter());
    }

    #[test]
    fn minter_mint_registers_account_and_credits_balance() {
        let mut contract = setup();
        let alice = accounts(0);
        assert!(contract.storage_balance_of(alice.clone()).is_none());

        as_account(minter(), NearToken::from_yoctonear(0));
        contract.mint(alice.clone(), U128(1_000));

        assert!(contract.storage_balance_of(alice.clone()).is_some());
        assert_eq!(contract.ft_balance_of(alice).0, 1_000);
        assert_eq!(contract.ft_total_supply().0, 1_000);
    }

    #[test]
    fn mint_emits_ft_mint_event() {
        let mut contract = setup();
        as_account(minter(), NearToken::from_yoctonear(0));
        contract.mint(accounts(0), U128(5));
        let logs = get_logs();
        assert!(
            logs.iter()
                .any(|l| l.contains("EVENT_JSON") && l.contains("ft_mint")),
            "missing ft_mint event: {logs:?}"
        );
    }

    #[test]
    fn mint_to_registered_account_adds_balance() {
        let mut contract = setup();
        as_account(minter(), NearToken::from_yoctonear(0));
        contract.mint(accounts(0), U128(1_000));
        contract.mint(accounts(0), U128(500));
        assert_eq!(contract.ft_balance_of(accounts(0)).0, 1_500);
        assert_eq!(contract.ft_total_supply().0, 1_500);
    }

    #[test]
    #[should_panic(expected = "Only the minter can mint")]
    fn owner_cannot_mint() {
        let mut contract = setup();
        // setup() runs as owner, so the owner is the predecessor here.
        contract.mint(accounts(1), U128(1_000));
    }

    #[test]
    #[should_panic(expected = "Only the minter can mint")]
    fn random_account_cannot_mint() {
        let mut contract = setup();
        as_account(accounts(0), NearToken::from_yoctonear(0));
        contract.mint(accounts(1), U128(1_000));
    }

    #[test]
    #[should_panic(expected = "Amount must be positive")]
    fn mint_zero_panics() {
        let mut contract = setup();
        as_account(minter(), NearToken::from_yoctonear(0));
        contract.mint(accounts(0), U128(0));
    }

    #[test]
    fn burn_reduces_balance_and_supply() {
        let mut contract = setup();
        as_account(minter(), NearToken::from_yoctonear(0));
        contract.mint(accounts(0), U128(100));

        as_account(accounts(0), NearToken::from_yoctonear(0));
        contract.burn(U128(30));

        assert_eq!(contract.ft_balance_of(accounts(0)).0, 70);
        assert_eq!(contract.ft_total_supply().0, 70);
        let logs = get_logs();
        assert!(
            logs.iter()
                .any(|l| l.contains("EVENT_JSON") && l.contains("ft_burn")),
            "missing ft_burn event: {logs:?}"
        );
    }

    #[test]
    #[should_panic]
    fn burn_more_than_balance_panics() {
        let mut contract = setup();
        as_account(minter(), NearToken::from_yoctonear(0));
        contract.mint(accounts(0), U128(10));
        as_account(accounts(0), NearToken::from_yoctonear(0));
        contract.burn(U128(11));
    }

    #[test]
    #[should_panic(expected = "Only the owner can call this")]
    fn non_owner_cannot_set_minter() {
        let mut contract = setup();
        as_account(accounts(0), NearToken::from_yoctonear(0));
        contract.set_minter(accounts(0));
    }

    #[test]
    #[should_panic(expected = "Only the owner can call this")]
    fn minter_cannot_set_minter() {
        let mut contract = setup();
        as_account(minter(), NearToken::from_yoctonear(0));
        contract.set_minter(minter());
    }

    #[test]
    #[should_panic(expected = "Only the owner can call this")]
    fn non_owner_cannot_set_owner() {
        let mut contract = setup();
        as_account(accounts(0), NearToken::from_yoctonear(0));
        contract.set_owner(accounts(0));
    }

    #[test]
    fn owner_can_rotate_minter_and_owner() {
        let mut contract = setup();
        contract.set_minter(accounts(2));
        assert_eq!(contract.minter(), accounts(2));

        contract.set_owner(accounts(1));
        assert_eq!(contract.owner(), accounts(1));
    }

    #[test]
    fn new_minter_can_mint_after_rotation() {
        let mut contract = setup();
        contract.set_minter(accounts(2));
        as_account(accounts(2), NearToken::from_yoctonear(0));
        contract.mint(accounts(0), U128(9));
        assert_eq!(contract.ft_balance_of(accounts(0)).0, 9);
    }

    #[test]
    #[should_panic(expected = "Only the minter can mint")]
    fn old_minter_loses_mint_rights_after_rotation() {
        let mut contract = setup();
        contract.set_minter(accounts(2));
        as_account(minter(), NearToken::from_yoctonear(0));
        contract.mint(accounts(0), U128(1));
    }

    #[test]
    fn ft_transfer_between_registered_accounts() {
        let mut contract = setup();
        let alice = accounts(0);
        let bob = accounts(1);
        as_account(minter(), NearToken::from_yoctonear(0));
        contract.mint(alice.clone(), U128(100));
        contract.mint(bob.clone(), U128(1));

        as_account(alice.clone(), YOCTO);
        contract.ft_transfer(bob.clone(), U128(40), None);

        assert_eq!(contract.ft_balance_of(alice).0, 60);
        assert_eq!(contract.ft_balance_of(bob).0, 41);
        assert_eq!(contract.ft_total_supply().0, 101);
    }

    #[test]
    #[should_panic]
    fn ft_transfer_requires_one_yocto() {
        let mut contract = setup();
        as_account(minter(), NearToken::from_yoctonear(0));
        contract.mint(accounts(0), U128(100));
        as_account(accounts(0), NearToken::from_yoctonear(0));
        contract.ft_transfer(accounts(1), U128(10), None);
    }

    #[test]
    fn total_supply_tracks_mints_and_burns() {
        let mut contract = setup();
        assert_eq!(contract.ft_total_supply().0, 0);
        as_account(minter(), NearToken::from_yoctonear(0));
        contract.mint(accounts(0), U128(7));
        contract.mint(accounts(1), U128(11));
        assert_eq!(contract.ft_total_supply().0, 18);
        as_account(accounts(1), NearToken::from_yoctonear(0));
        contract.burn(U128(1));
        assert_eq!(contract.ft_total_supply().0, 17);
    }
}
