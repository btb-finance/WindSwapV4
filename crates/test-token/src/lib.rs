//! Minimal NEP-141 fungible test token for Wind Swap v4 on NEAR testnet.
//!
//! TEST ONLY. Not audited, not for production or mainnet.
//!
//! - `new` creates the token with metadata (NEP-148, spec `ft-1.0.0`) and zero supply.
//! - Core NEP-141 (`ft_transfer`, `ft_transfer_call`, `ft_total_supply`, `ft_balance_of`,
//!   `ft_resolve_transfer`), NEP-145 storage management and `ft_metadata` come from
//!   `near-contract-standards`.
//! - `mint` is an owner-only faucet so testers can get tokens. It is TEST ONLY.

use near_contract_standards::fungible_token::core::FungibleTokenCore;
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
    token: FungibleToken,
    metadata: LazyOption<FungibleTokenMetadata>,
}

#[near]
impl Contract {
    /// Creates the token with zero supply. `decimals` is the number of fractional digits.
    #[init]
    pub fn new(owner_id: AccountId, name: String, symbol: String, decimals: u8) -> Self {
        let metadata = FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name,
            symbol,
            icon: None,
            reference: None,
            reference_hash: None,
            decimals,
        };
        metadata.assert_valid();
        Self {
            owner_id,
            token: FungibleToken::new(StorageKey::FungibleToken),
            metadata: LazyOption::new(StorageKey::Metadata, Some(&metadata)),
        }
    }

    /// TEST ONLY faucet: mints `amount` to `account_id`, owner only.
    /// If `account_id` is not registered, it is registered first (the contract pays the storage).
    pub fn mint(&mut self, account_id: AccountId, amount: U128) {
        require!(
            env::predecessor_account_id() == self.owner_id,
            "Only the owner can mint"
        );
        if !self.token.accounts.contains_key(&account_id) {
            self.token.internal_register_account(&account_id);
        }
        self.token.internal_deposit(&account_id, amount.0);
        log!("Minted {} to {}", amount.0, account_id);
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
    use near_sdk::test_utils::{accounts, VMContextBuilder};
    use near_sdk::testing_env;

    const YOCTO: NearToken = NearToken::from_yoctonear(1);

    fn owner() -> AccountId {
        accounts(4)
    }

    fn setup() -> Contract {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(owner())
            .build());
        Contract::new(owner(), "Test Token".into(), "TEST".into(), 18)
    }

    fn as_account(account: AccountId, deposit: NearToken) {
        testing_env!(VMContextBuilder::new()
            .predecessor_account_id(account)
            .attached_deposit(deposit)
            .build());
    }

    #[test]
    fn init_sets_metadata_and_zero_supply() {
        let contract = setup();
        let meta = contract.ft_metadata();
        assert_eq!(meta.spec, FT_METADATA_SPEC);
        assert_eq!(meta.name, "Test Token");
        assert_eq!(meta.symbol, "TEST");
        assert_eq!(meta.decimals, 18);
        assert_eq!(contract.ft_total_supply().0, 0);
    }

    #[test]
    fn owner_mint_registers_account_and_credits_balance() {
        let mut contract = setup();
        let alice = accounts(0);
        assert!(contract.storage_balance_of(alice.clone()).is_none());

        contract.mint(alice.clone(), U128(1_000));

        assert!(contract.storage_balance_of(alice.clone()).is_some());
        assert_eq!(contract.ft_balance_of(alice).0, 1_000);
        assert_eq!(contract.ft_total_supply().0, 1_000);
    }

    #[test]
    fn mint_to_registered_account_adds_balance() {
        let mut contract = setup();
        contract.mint(accounts(0), U128(1_000));
        contract.mint(accounts(0), U128(500));
        assert_eq!(contract.ft_balance_of(accounts(0)).0, 1_500);
        assert_eq!(contract.ft_total_supply().0, 1_500);
    }

    #[test]
    #[should_panic(expected = "Only the owner can mint")]
    fn non_owner_mint_panics() {
        let mut contract = setup();
        as_account(accounts(0), NearToken::from_yoctonear(0));
        contract.mint(accounts(1), U128(1_000));
    }

    #[test]
    fn ft_transfer_between_registered_accounts() {
        let mut contract = setup();
        let alice = accounts(0);
        let bob = accounts(1);
        contract.mint(alice.clone(), U128(100));
        contract.mint(bob.clone(), U128(0));

        as_account(alice.clone(), YOCTO);
        contract.ft_transfer(bob.clone(), U128(40), None);

        assert_eq!(contract.ft_balance_of(alice).0, 60);
        assert_eq!(contract.ft_balance_of(bob).0, 40);
        assert_eq!(contract.ft_total_supply().0, 100);
    }

    #[test]
    #[should_panic]
    fn ft_transfer_requires_one_yocto() {
        let mut contract = setup();
        contract.mint(accounts(0), U128(100));
        as_account(accounts(0), NearToken::from_yoctonear(0));
        contract.ft_transfer(accounts(1), U128(10), None);
    }

    #[test]
    fn total_supply_tracks_mints() {
        let mut contract = setup();
        assert_eq!(contract.ft_total_supply().0, 0);
        contract.mint(accounts(0), U128(7));
        contract.mint(accounts(1), U128(11));
        assert_eq!(contract.ft_total_supply().0, 18);
    }
}
