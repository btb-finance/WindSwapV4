//! Tests for the EVM bridge: relayer auth, deposits, the asset and chain registries, withdrawals,
//! the signing callback, the daily cap, and the wallet-state rule for linking.

use std::panic::{catch_unwind, AssertUnwindSafe};

use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{keccak256, Address, Bytes, TxKind, U256};
use k256::ecdsa::SigningKey;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, Gas, PromiseError};

use super::*;
use crate::accounts::{engine_id_for, owner_hex, LinkRequest};
use crate::accounts_tests::{evm_sign, evm_wallet, hex, sk};
use crate::tests::{mpc_response, set_payout_ctx, sign_call_args, test_key};

const OWNER: &str = "owner.testnet";
const SELF: &str = "windswapv4-btb.testnet";
const ALICE: &str = "alice.testnet";
const BOB: &str = "bob.testnet";
const RELAYER: &str = "relayer.testnet";
const WNEAR: &str = "wnear.fakes.testnet";
const CHAIN: u64 = 84532;
const PATH: &str = "ethereum-1";
const ETH: &str = "base-sepolia:eth";
const USDC: &str = "base-sepolia:usdc";
/// Base Sepolia USDC, as the registry stores it (lower case).
const USDC_ADDR: &str = "0x036cbd53842c5426634e7929541ec2318f3dcf7e";
const RECIPIENT: &str = "0x2222222222222222222222222222222222222222";
const Q96_STR: &str = "79228162514264337593543950336";
const TICK_SPACING: i32 = 3;
const NOW: u64 = 1_000;
const DAY: u64 = 86_400;
const EXPIRES: u64 = 2_000;
const E18: u128 = 1_000_000_000_000_000_000;
/// Chain cap, and the fees every test uses (both below the cap).
const CHAIN_FEE_CAP: u128 = 30_000_000_000;
const FEE: u128 = 20_000_000_000;
const PRIORITY: u128 = 2_000_000_000;

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

fn ctx(predecessor: &str, block_secs: u64, deposit: u128) {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(acct(SELF))
        .predecessor_account_id(acct(predecessor))
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .prepaid_gas(Gas::from_tgas(300))
        .block_timestamp(block_secs * 1_000_000_000);
    testing_env!(
        builder.build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![]
    );
}

fn setup() -> Contract {
    ctx(OWNER, NOW, 0);
    Contract::new(acct(OWNER))
}

fn day_of(secs: u64) -> u64 {
    secs / DAY
}

fn balance(c: &Contract, who: &str, token: &str) -> u128 {
    c.get_balance(acct(who), token.to_string()).0
}

fn tx_hash(n: u8) -> String {
    format!("0x{}", format!("{n:02x}").repeat(32))
}

fn add_relayer(c: &mut Contract) {
    ctx(OWNER, NOW, 0);
    c.add_relayer(acct(RELAYER));
}

fn register_chain(c: &mut Contract, next_nonce: u64) {
    ctx(OWNER, NOW, 0);
    c.set_evm_chain(CHAIN, PATH.into(), next_nonce, U128(CHAIN_FEE_CAP));
}

fn register_eth(c: &mut Contract, per_deposit: u128, daily: u128) {
    ctx(OWNER, NOW, 0);
    c.register_evm_asset(
        CHAIN,
        "native".into(),
        ETH.into(),
        18,
        U128(per_deposit),
        U128(daily),
    );
}

fn register_usdc(c: &mut Contract, daily: u128) {
    ctx(OWNER, NOW, 0);
    c.register_evm_asset(
        CHAIN,
        USDC_ADDR.to_uppercase().replace("0X", "0x"),
        USDC.into(),
        6,
        U128(u128::MAX),
        U128(daily),
    );
}

/// The relayer reports a deposit from `from` (`0x…`). Returns what the contract answered.
fn deposit(c: &mut Contract, from: &str, tx: u8, log: u32, asset: &str, amount: u128) -> bool {
    ctx(RELAYER, NOW, 0);
    c.report_evm_deposit(
        CHAIN,
        tx_hash(tx),
        log,
        from.into(),
        asset.into(),
        U128(amount),
    )
}

fn link(c: &mut Contract, key: &SigningKey, account: &str) {
    let req = LinkRequest {
        wallet: evm_wallet(key),
        account: acct(account),
        contract: acct(SELF),
        nonce: 1,
        expires: EXPIRES,
    };
    let sig = format!("0x{}", hex(&evm_sign(key, &req.message())));
    ctx(account, NOW, 0);
    c.link_wallet(evm_wallet(key).to_string(), 1, EXPIRES, sig);
}

/// The `0x…` address of an EVM key.
fn address_of(key: &SigningKey) -> String {
    evm_wallet(key)
        .to_string()
        .strip_prefix("evm:")
        .unwrap()
        .to_string()
}

/// Links the wallet of seed `seed` to ALICE and deposits `amount` of ETH from it, so ALICE holds it.
fn fund_alice(c: &mut Contract, seed: u8, tx: u8, amount: u128) -> SigningKey {
    let key = sk(seed);
    link(c, &key, ALICE);
    assert!(deposit(c, &address_of(&key), tx, 0, "native", amount));
    key
}

/// Ready for withdrawals: relayer, chain (nonce 0), native ETH with the given daily cap.
fn ready(daily_cap: u128) -> Contract {
    let mut c = setup();
    add_relayer(&mut c);
    register_chain(&mut c, 0);
    register_eth(&mut c, 10 * E18, daily_cap);
    c
}

/// ALICE asks for a withdrawal of ETH. Returns its id.
fn withdraw(c: &mut Contract, amount: u128, to: &str) -> u64 {
    ctx(ALICE, NOW, 0);
    c.withdraw_evm(ETH.into(), U128(amount), to.into())
}

/// The relayer processes withdrawal `id` at block time `secs`. Returns the digest the MPC was
/// asked to sign, after checking the request's path and deposit.
fn process_at(c: &mut Contract, id: u64, secs: u64) -> [u8; 32] {
    ctx(RELAYER, secs, 1);
    c.process_withdrawal(id, U128(FEE), U128(PRIORITY)).detach();
    let (args, deposit, gas) = sign_call_args().expect("a sign call was created");
    assert_eq!(deposit, NearToken::from_yoctonear(1));
    assert_eq!(gas, Gas::from_tgas(50));
    let json: near_sdk::serde_json::Value = near_sdk::serde_json::from_slice(&args).unwrap();
    assert_eq!(json["request"]["path"], PATH);
    assert_eq!(json["request"]["domain_id"], 0);
    evm::hex_decode(json["request"]["payload_v2"]["Ecdsa"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}

fn process(c: &mut Contract, id: u64) -> [u8; 32] {
    process_at(c, id, NOW)
}

/// The EVM transaction the contract should build, as `EvmTx` (the callback's argument).
fn tx_of(nonce: u64, to: &str, value: u128, data: Vec<u8>, gas: u64) -> EvmTx {
    EvmTx {
        chain_id: CHAIN,
        nonce,
        to: to.into(),
        value_wei: U128(value),
        max_fee_per_gas: U128(FEE),
        max_priority_fee_per_gas: U128(PRIORITY),
        gas_limit: gas,
        data: evm::hex_0x(&data),
    }
}

fn native_tx(nonce: u64, value: u128) -> EvmTx {
    tx_of(nonce, RECIPIENT, value, vec![], 21_000)
}

fn addr(s: &str) -> [u8; 20] {
    let a: Address = s.parse().unwrap();
    let mut out = [0u8; 20];
    out.copy_from_slice(a.as_slice());
    out
}

/// `transfer(to, amount)` calldata, built here with alloy's keccak and not with `evm.rs`.
fn expected_erc20_data(to: [u8; 20], amount: u128) -> Vec<u8> {
    let mut data = keccak256(b"transfer(address,uint256)").0[..4].to_vec();
    data.extend([0u8; 12]);
    data.extend(to);
    data.extend([0u8; 16]);
    data.extend(amount.to_be_bytes());
    data
}

/// The signing hash of the transaction, computed by alloy from the expected fields.
fn expected_hash(nonce: u64, to: [u8; 20], value: u128, data: Vec<u8>, gas: u64) -> [u8; 32] {
    TxEip1559 {
        chain_id: CHAIN,
        nonce,
        gas_limit: gas,
        max_fee_per_gas: FEE,
        max_priority_fee_per_gas: PRIORITY,
        to: TxKind::Call(Address::from(to)),
        value: U256::from(value),
        access_list: Default::default(),
        input: Bytes::from(data),
    }
    .signature_hash()
    .0
}

/// Plays the MPC node for `digest` and runs the callback. Returns the raw transaction.
fn complete_ok(c: &mut Contract, id: u64, day: u64, tx: &EvmTx, digest: [u8; 32]) -> String {
    let sig = mpc_response(&test_key(), &digest, 0x1234_5678_9abc_def0, false);
    ctx(SELF, NOW, 0);
    assert!(c.on_withdrawal_signed(id, day, tx.clone(), Ok(sig)));
    c.get_withdrawal(id)
        .and_then(|w| w.raw_tx)
        .expect("signed transaction stored")
}

/// The MPC call failed, so the callback runs with an error.
fn complete_err(c: &mut Contract, id: u64, day: u64, tx: &EvmTx) -> bool {
    ctx(SELF, NOW, 0);
    c.on_withdrawal_signed(id, day, tx.clone(), Err(PromiseError::Failed))
}

/// Decodes a signed raw transaction with alloy. Returns the transaction and its recovered signer.
fn decode_signed(raw: &str) -> (TxEip1559, Address) {
    let bytes = evm::hex_decode(raw).unwrap();
    let envelope = TxEnvelope::decode_2718(&mut bytes.as_slice()).expect("valid envelope");
    let signer = envelope.recover_signer().unwrap();
    match envelope {
        TxEnvelope::Eip1559(signed) => (signed.tx().clone(), signer),
        _ => panic!("not an EIP-1559 transaction"),
    }
}

fn chain_next_nonce(c: &Contract) -> u64 {
    c.get_evm_chain(CHAIN).expect("chain set").next_nonce
}

// ---------- relayer admin and auth ----------

#[test]
fn relayers_can_be_added_and_removed_by_the_owner() {
    let mut c = setup();
    add_relayer(&mut c);
    assert_eq!(c.get_relayers(), vec![acct(RELAYER)]);
    ctx(OWNER, NOW, 0);
    c.remove_relayer(acct(RELAYER));
    assert!(c.get_relayers().is_empty());
}

#[test]
#[should_panic(expected = "only owner")]
fn add_relayer_is_owner_only() {
    let mut c = setup();
    ctx(ALICE, NOW, 0);
    c.add_relayer(acct(RELAYER));
}

#[test]
#[should_panic(expected = "already a relayer")]
fn add_relayer_rejects_duplicates() {
    let mut c = setup();
    add_relayer(&mut c);
    add_relayer(&mut c);
}

#[test]
#[should_panic(expected = "not a relayer")]
fn remove_relayer_rejects_unknown_account() {
    let mut c = setup();
    ctx(OWNER, NOW, 0);
    c.remove_relayer(acct(RELAYER));
}

#[test]
#[should_panic(expected = "only relayer")]
fn report_deposit_requires_a_relayer() {
    let mut c = ready(E18);
    ctx(BOB, NOW, 0);
    c.report_evm_deposit(
        CHAIN,
        tx_hash(1),
        0,
        address_of(&sk(1)),
        "native".into(),
        U128(1),
    );
}

#[test]
#[should_panic(expected = "only relayer")]
fn process_withdrawal_requires_a_relayer() {
    let mut c = ready(E18);
    fund_alice(&mut c, 2, 1, E18);
    withdraw(&mut c, 1, RECIPIENT);
    ctx(ALICE, NOW, 1);
    c.process_withdrawal(0, U128(FEE), U128(PRIORITY)).detach();
}

#[test]
#[should_panic(expected = "only relayer")]
fn mark_broadcast_requires_a_relayer() {
    let mut c = ready(E18);
    ctx(BOB, NOW, 0);
    c.mark_withdrawal_broadcast(0, tx_hash(9));
}

// ---------- deposits ----------

#[test]
fn deposit_from_an_unlinked_wallet_credits_its_own_owner() {
    let mut c = ready(E18);
    let key = sk(3);
    assert!(deposit(&mut c, &address_of(&key), 1, 0, "native", 5));
    let owner = c.resolve_wallet(evm_wallet(&key).to_string());
    assert_eq!(owner, owner_hex(&engine_id_for(&evm_wallet(&key))));
    assert_eq!(c.get_balance_of_owner(owner, ETH.into()).0, 5);
}

#[test]
fn deposit_from_a_linked_wallet_credits_the_account() {
    let mut c = ready(E18);
    let key = sk(4);
    link(&mut c, &key, ALICE);
    assert!(deposit(&mut c, &address_of(&key), 1, 0, "native", 7));
    assert_eq!(balance(&c, ALICE, ETH), 7);
}

#[test]
fn deposit_is_credited_once_per_tx_and_log_index() {
    let mut c = ready(E18);
    let key = sk(5);
    let from = address_of(&key);
    assert!(deposit(&mut c, &from, 1, 0, "native", 10));
    assert!(
        !deposit(&mut c, &from, 1, 0, "native", 10),
        "replay is a no-op"
    );
    assert!(
        !deposit(&mut c, &from, 1, 0, "native", 999),
        "replay with another amount too"
    );
    assert!(
        deposit(&mut c, &from, 1, 1, "native", 10),
        "another log of the same tx"
    );
    assert_eq!(c.resolve_wallet(evm_wallet(&key).to_string()), {
        owner_hex(&engine_id_for(&evm_wallet(&key)))
    });
    assert_eq!(
        c.get_balance_of_owner(c.resolve_wallet(evm_wallet(&key).to_string()), ETH.into())
            .0,
        20
    );
}

#[test]
#[should_panic(expected = "is not registered")]
fn deposit_rejects_an_unregistered_asset() {
    let mut c = ready(E18);
    deposit(&mut c, &address_of(&sk(6)), 1, 0, USDC_ADDR, 1);
}

#[test]
#[should_panic(expected = "above the per-deposit cap")]
fn deposit_rejects_amounts_above_the_cap() {
    let mut c = setup();
    add_relayer(&mut c);
    register_chain(&mut c, 0);
    register_eth(&mut c, 100, E18);
    deposit(&mut c, &address_of(&sk(6)), 1, 0, "native", 101);
}

#[test]
#[should_panic(expected = "amount must be positive")]
fn deposit_rejects_zero() {
    let mut c = ready(E18);
    deposit(&mut c, &address_of(&sk(6)), 1, 0, "native", 0);
}

#[test]
#[should_panic(expected = "tx hash must be 32 bytes")]
fn deposit_rejects_a_short_tx_hash() {
    let mut c = ready(E18);
    ctx(RELAYER, NOW, 0);
    c.report_evm_deposit(
        CHAIN,
        "0x1234".into(),
        0,
        address_of(&sk(6)),
        "native".into(),
        U128(1),
    );
}

#[test]
fn erc20_deposit_is_credited_under_its_internal_id() {
    let mut c = ready(E18);
    register_usdc(&mut c, E18);
    let key = sk(7);
    link(&mut c, &key, ALICE);
    assert!(deposit(
        &mut c,
        &address_of(&key),
        2,
        0,
        USDC_ADDR,
        1_500_000
    ));
    assert_eq!(balance(&c, ALICE, USDC), 1_500_000);
}

// ---------- asset and chain registries ----------

#[test]
#[should_panic(expected = "only owner")]
fn register_asset_is_owner_only() {
    let mut c = setup();
    ctx(ALICE, NOW, 0);
    c.register_evm_asset(CHAIN, "native".into(), ETH.into(), 18, U128(1), U128(1));
}

#[test]
#[should_panic(expected = "not a NEAR account id")]
fn register_asset_refuses_a_near_account_as_token_id() {
    let mut c = setup();
    ctx(OWNER, NOW, 0);
    c.register_evm_asset(CHAIN, "native".into(), WNEAR.into(), 18, U128(1), U128(1));
}

#[test]
fn registered_assets_are_viewable_with_normalised_addresses() {
    let mut c = setup();
    register_usdc(&mut c, 42);
    let view = c
        .get_evm_asset(CHAIN, USDC_ADDR.to_uppercase().replace("0X", "0x"))
        .expect("found with any case");
    assert_eq!(view.asset, USDC_ADDR);
    assert_eq!(view.token_id, USDC);
    assert_eq!(view.decimals, 6);
    assert_eq!(view.daily_withdraw_cap.0, 42);
    assert_eq!(c.get_evm_assets().len(), 1);
    assert!(c.get_evm_asset(CHAIN, "native".into()).is_none());
}

#[test]
fn re_registering_updates_caps_for_the_same_token_id() {
    let mut c = setup();
    register_eth(&mut c, 1, 2);
    register_eth(&mut c, 3, 4);
    let view = c.get_evm_asset(CHAIN, "native".into()).unwrap();
    assert_eq!(view.per_deposit_cap.0, 3);
    assert_eq!(view.daily_withdraw_cap.0, 4);
}

#[test]
#[should_panic(expected = "already registered under token id")]
fn an_asset_cannot_change_its_token_id() {
    let mut c = setup();
    register_eth(&mut c, 1, 1);
    ctx(OWNER, NOW, 0);
    c.register_evm_asset(
        CHAIN,
        "native".into(),
        "base-sepolia:eth2".into(),
        18,
        U128(1),
        U128(1),
    );
}

#[test]
#[should_panic(expected = "is used by another asset")]
fn a_token_id_cannot_move_to_another_asset() {
    let mut c = setup();
    register_eth(&mut c, 1, 1);
    ctx(OWNER, NOW, 0);
    c.register_evm_asset(CHAIN, USDC_ADDR.into(), ETH.into(), 6, U128(1), U128(1));
}

#[test]
fn create_pool_accepts_registered_internal_ids() {
    let mut c = setup();
    register_eth(&mut c, 1, 1);
    ctx(OWNER, NOW, 0);
    let pool = c.create_pool(ETH.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
    assert_eq!(c.get_pool(pool).token0, ETH);
    assert_eq!(c.get_pool(pool).token1, WNEAR);
}

#[test]
#[should_panic(expected = "invalid token id: base-sepolia:eth")]
fn create_pool_rejects_an_unregistered_internal_id() {
    let mut c = setup();
    ctx(OWNER, NOW, 0);
    c.create_pool(ETH.into(), WNEAR.into(), TICK_SPACING, Q96_STR.into());
}

#[test]
#[should_panic(expected = "only owner")]
fn set_evm_chain_is_owner_only() {
    let mut c = setup();
    ctx(ALICE, NOW, 0);
    c.set_evm_chain(CHAIN, PATH.into(), 0, U128(1));
}

#[test]
fn set_evm_chain_stores_settings_and_nonce_only_goes_up() {
    let mut c = setup();
    register_chain(&mut c, 5);
    let view = c.get_evm_chain(CHAIN).unwrap();
    assert_eq!(view.path, PATH);
    assert_eq!(view.next_nonce, 5);
    assert_eq!(view.max_fee_per_gas_cap.0, CHAIN_FEE_CAP);
    register_chain(&mut c, 7);
    assert_eq!(chain_next_nonce(&c), 7);
    assert!(c.get_evm_chain(1).is_none());
}

#[test]
#[should_panic(expected = "next_nonce cannot go back")]
fn set_evm_chain_refuses_to_reuse_a_nonce() {
    let mut c = setup();
    register_chain(&mut c, 7);
    register_chain(&mut c, 6);
}

// ---------- withdrawals ----------

#[test]
fn withdraw_evm_debits_and_records_a_pending_withdrawal() {
    let mut c = ready(E18);
    fund_alice(&mut c, 8, 1, 100);
    let id = withdraw(&mut c, 40, RECIPIENT);
    assert_eq!(id, 0);
    assert_eq!(balance(&c, ALICE, ETH), 60);
    let w = c.get_withdrawal(0).unwrap();
    assert_eq!(w.status, "Pending");
    assert_eq!(w.owner_hex, owner_hex(&owner_of_account(&acct(ALICE))));
    assert_eq!(w.token_id, ETH);
    assert_eq!(w.amount.0, 40);
    assert_eq!(w.to_chain_id, CHAIN);
    assert_eq!(w.to_address, RECIPIENT);
    assert_eq!(w.nonce, None);
    assert_eq!(c.get_pending_withdrawals(0, 10).len(), 1);
}

#[test]
#[should_panic(expected = "is not a registered EVM asset")]
fn withdraw_evm_rejects_an_unregistered_token() {
    let mut c = ready(E18);
    ctx(ALICE, NOW, 0);
    c.withdraw_evm(USDC.into(), U128(1), RECIPIENT.into());
}

#[test]
#[should_panic(expected = "insufficient")]
fn withdraw_evm_rejects_insufficient_balance() {
    let mut c = ready(E18);
    fund_alice(&mut c, 8, 1, 10);
    withdraw(&mut c, 11, RECIPIENT);
}

#[test]
#[should_panic(expected = "address must start with 0x")]
fn withdraw_evm_rejects_a_bad_address() {
    let mut c = ready(E18);
    fund_alice(&mut c, 8, 1, 10);
    withdraw(&mut c, 1, "2222222222222222222222222222222222222222");
}

#[test]
#[should_panic(expected = "amount must be positive")]
fn withdraw_evm_rejects_zero() {
    let mut c = ready(E18);
    fund_alice(&mut c, 8, 1, 10);
    withdraw(&mut c, 0, RECIPIENT);
}

#[test]
fn native_withdrawal_signs_the_expected_transfer() {
    let mut c = ready(E18);
    fund_alice(&mut c, 9, 1, 2 * E18);
    let id = withdraw(&mut c, 4 * E18 / 10, RECIPIENT);

    let digest = process(&mut c, id);
    let amount = 4 * E18 / 10;
    assert_eq!(
        digest,
        expected_hash(0, addr(RECIPIENT), amount, vec![], 21_000)
    );

    let raw = complete_ok(&mut c, id, day_of(NOW), &native_tx(0, amount), digest);
    let (tx, signer) = decode_signed(&raw);
    assert_eq!(tx.chain_id, CHAIN);
    assert_eq!(tx.nonce, 0);
    assert_eq!(tx.to, TxKind::Call(Address::from(addr(RECIPIENT))));
    assert_eq!(tx.value, U256::from(amount));
    assert!(tx.input.is_empty());
    assert_eq!(tx.gas_limit, 21_000);
    assert_eq!(tx.max_fee_per_gas, FEE);
    assert_eq!(tx.max_priority_fee_per_gas, PRIORITY);
    assert_eq!(
        signer,
        Address::from(test_key().address),
        "signed by the chain vault"
    );

    let w = c.get_withdrawal(id).unwrap();
    assert_eq!(w.status, "Signed");
    assert_eq!(w.nonce, Some(0));
    assert_eq!(w.raw_tx, Some(raw));
    assert_eq!(chain_next_nonce(&c), 1);
}

#[test]
fn erc20_withdrawal_calls_transfer_on_the_token() {
    let mut c = ready(E18);
    register_usdc(&mut c, E18);
    let key = sk(10);
    link(&mut c, &key, ALICE);
    assert!(deposit(
        &mut c,
        &address_of(&key),
        3,
        0,
        USDC_ADDR,
        2_000_000
    ));
    ctx(ALICE, NOW, 0);
    let id = c.withdraw_evm(USDC.into(), U128(1_500_000), RECIPIENT.into());

    let digest = process(&mut c, id);
    let data = expected_erc20_data(addr(RECIPIENT), 1_500_000);
    assert_eq!(
        digest,
        expected_hash(0, addr(USDC_ADDR), 0, data.clone(), 100_000)
    );

    let tx = tx_of(0, USDC_ADDR, 0, data.clone(), 100_000);
    let raw = complete_ok(&mut c, id, day_of(NOW), &tx, digest);
    let (signed, signer) = decode_signed(&raw);
    assert_eq!(signed.to, TxKind::Call(Address::from(addr(USDC_ADDR))));
    assert_eq!(signed.value, U256::ZERO);
    assert_eq!(signed.input, Bytes::from(data));
    assert_eq!(signed.gas_limit, 100_000);
    assert_eq!(signer, Address::from(test_key().address));
}

#[test]
fn nonces_follow_processing_order_and_signed_list_is_by_nonce() {
    let mut c = ready(E18);
    fund_alice(&mut c, 11, 1, 2 * E18);
    let first = withdraw(&mut c, 1, RECIPIENT);
    let second = withdraw(&mut c, 2, RECIPIENT);

    // The later withdrawal is processed first, so it takes nonce 0.
    let d_second = process(&mut c, second);
    assert_eq!(
        d_second,
        expected_hash(0, addr(RECIPIENT), 2, vec![], 21_000)
    );
    let raw_second = complete_ok(&mut c, second, day_of(NOW), &native_tx(0, 2), d_second);
    let d_first = process(&mut c, first);
    assert_eq!(
        d_first,
        expected_hash(1, addr(RECIPIENT), 1, vec![], 21_000)
    );
    complete_ok(&mut c, first, day_of(NOW), &native_tx(1, 1), d_first);

    let signed = c.get_signed_withdrawals(0, 10);
    assert_eq!(
        signed.iter().map(|w| w.id).collect::<Vec<_>>(),
        vec![second, first]
    );
    assert_eq!(signed[0].nonce, Some(0));
    assert_eq!(signed[0].raw_tx, Some(raw_second));
    assert!(c.get_pending_withdrawals(0, 10).is_empty());
    assert_eq!(c.get_signed_withdrawals(0, 1).len(), 1, "limit applies");
}

#[test]
#[should_panic(expected = "is above the chain cap")]
fn process_rejects_a_fee_above_the_chain_cap() {
    let mut c = ready(E18);
    fund_alice(&mut c, 12, 1, 10);
    let id = withdraw(&mut c, 1, RECIPIENT);
    ctx(RELAYER, NOW, 1);
    c.process_withdrawal(id, U128(CHAIN_FEE_CAP + 1), U128(PRIORITY))
        .detach();
}

#[test]
#[should_panic(expected = "is not pending")]
fn process_rejects_a_withdrawal_already_being_signed() {
    let mut c = ready(E18);
    fund_alice(&mut c, 12, 1, 10);
    let id = withdraw(&mut c, 1, RECIPIENT);
    process(&mut c, id);
    process(&mut c, id);
}

#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn process_requires_one_yocto() {
    let mut c = ready(E18);
    fund_alice(&mut c, 12, 1, 10);
    let id = withdraw(&mut c, 1, RECIPIENT);
    ctx(RELAYER, NOW, 0);
    c.process_withdrawal(id, U128(FEE), U128(PRIORITY)).detach();
}

#[test]
#[should_panic(expected = "attach at least 100 Tgas")]
fn process_requires_prepaid_gas() {
    let mut c = ready(E18);
    fund_alice(&mut c, 12, 1, 10);
    let id = withdraw(&mut c, 1, RECIPIENT);
    set_payout_ctx(RELAYER, 1, 99);
    c.process_withdrawal(id, U128(FEE), U128(PRIORITY)).detach();
}

#[test]
fn a_failed_signature_releases_the_nonce_and_the_daily_reservation() {
    // The cap allows exactly one withdrawal per day, so the second process only works if the
    // first reservation is released.
    let mut c = ready(4 * E18 / 10);
    fund_alice(&mut c, 13, 1, 2 * E18);
    let id = withdraw(&mut c, 4 * E18 / 10, RECIPIENT);
    let digest = process(&mut c, id);
    let tx = native_tx(0, 4 * E18 / 10);
    assert!(!complete_err(&mut c, id, day_of(NOW), &tx));

    let w = c.get_withdrawal(id).unwrap();
    assert_eq!(w.status, "Pending");
    assert_eq!(w.nonce, None);
    assert_eq!(chain_next_nonce(&c), 0, "nonce released");
    assert_eq!(
        balance(&c, ALICE, ETH),
        2 * E18 - 4 * E18 / 10,
        "no refund for a retry"
    );

    // Retrying reuses nonce 0 and the same digest.
    assert_eq!(process(&mut c, id), digest);
}

#[test]
fn a_failed_signature_after_a_later_nonce_leaves_a_gap_and_refunds() {
    let mut c = ready(E18);
    fund_alice(&mut c, 14, 1, E18);
    let a = withdraw(&mut c, 1, RECIPIENT);
    let b = withdraw(&mut c, 2, RECIPIENT);
    process(&mut c, a); // nonce 0
    let d_b = process(&mut c, b); // nonce 1
    assert!(!complete_err(&mut c, a, day_of(NOW), &native_tx(0, 1)));

    let failed = c.get_withdrawal(a).unwrap();
    assert_eq!(failed.status, "Failed");
    assert_eq!(failed.nonce, Some(0), "the gap is recorded");
    assert_eq!(chain_next_nonce(&c), 2, "nonce 1 stays reserved");
    assert_eq!(
        balance(&c, ALICE, ETH),
        E18 - 2,
        "failed amount credited back"
    );

    complete_ok(&mut c, b, day_of(NOW), &native_tx(1, 2), d_b);
    assert_eq!(c.get_withdrawal(b).unwrap().nonce, Some(1));
}

#[test]
fn a_bad_mpc_response_counts_as_a_failure_without_panicking() {
    let mut c = ready(E18);
    fund_alice(&mut c, 15, 1, E18);
    let id = withdraw(&mut c, 1, RECIPIENT);
    let digest = process(&mut c, id);
    let mut sig = mpc_response(&test_key(), &digest, 5, false);
    sig.scheme = "Ed25519".into();
    ctx(SELF, NOW, 0);
    assert!(!c.on_withdrawal_signed(id, day_of(NOW), native_tx(0, 1), Ok(sig)));
    assert_eq!(c.get_withdrawal(id).unwrap().status, "Pending");
    assert_eq!(chain_next_nonce(&c), 0);
}

#[test]
fn daily_cap_is_enforced_per_utc_day() {
    let mut c = ready(6 * E18 / 10);
    fund_alice(&mut c, 16, 1, 2 * E18);
    let first = withdraw(&mut c, 5 * E18 / 10, RECIPIENT);
    let second = withdraw(&mut c, 2 * E18 / 10, RECIPIENT);
    process(&mut c, first); // 0.5 reserved today

    // 0.5 + 0.2 > 0.6: refused, and nothing changes.
    let refused = catch_unwind(AssertUnwindSafe(|| process(&mut c, second)));
    assert!(refused.is_err(), "the cap refuses the second withdrawal");
    assert_eq!(c.get_withdrawal(second).unwrap().status, "Pending");
    assert_eq!(chain_next_nonce(&c), 1);

    // The next UTC day starts with an empty window.
    let digest = process_at(&mut c, second, NOW + DAY);
    assert_eq!(
        digest,
        expected_hash(1, addr(RECIPIENT), 2 * E18 / 10, vec![], 21_000)
    );
}

#[test]
fn broadcast_moves_a_signed_withdrawal_to_broadcast() {
    let mut c = ready(E18);
    fund_alice(&mut c, 17, 1, E18);
    let id = withdraw(&mut c, 1, RECIPIENT);
    let digest = process(&mut c, id);
    complete_ok(&mut c, id, day_of(NOW), &native_tx(0, 1), digest);

    ctx(RELAYER, NOW, 0);
    c.mark_withdrawal_broadcast(id, tx_hash(0xab));
    let w = c.get_withdrawal(id).unwrap();
    assert_eq!(w.status, "Broadcast");
    assert_eq!(w.nonce, Some(0));
    assert_eq!(w.tx_hash, Some(tx_hash(0xab)));
    assert!(c.get_signed_withdrawals(0, 10).is_empty());
}

#[test]
#[should_panic(expected = "is not signed")]
fn broadcast_requires_a_signed_withdrawal() {
    let mut c = ready(E18);
    fund_alice(&mut c, 17, 1, E18);
    let id = withdraw(&mut c, 1, RECIPIENT);
    ctx(RELAYER, NOW, 0);
    c.mark_withdrawal_broadcast(id, tx_hash(1));
}

// ---------- linking and wallet state ----------

#[test]
#[should_panic(expected = "cannot be linked")]
fn a_wallet_with_a_deposit_under_its_own_id_cannot_be_linked() {
    let mut c = ready(E18);
    let key = sk(20);
    assert!(deposit(&mut c, &address_of(&key), 1, 0, "native", 5));
    link(&mut c, &key, ALICE);
}

#[test]
fn a_fresh_wallet_can_be_linked() {
    let mut c = ready(E18);
    let key = sk(21);
    link(&mut c, &key, ALICE);
    assert_eq!(
        c.get_wallets(acct(ALICE)),
        vec![evm_wallet(&key).to_string()]
    );
}

#[test]
fn flagging_a_wallet_blocks_linking_and_is_idempotent() {
    let mut c = setup();
    let key = sk(22);
    let wallet = evm_wallet(&key).to_string();
    ctx(OWNER, NOW, 0);
    assert_eq!(c.flag_wallet_state(vec![wallet.clone()]), 1);
    assert_eq!(c.flag_wallet_state(vec![wallet]), 0);
    let res = catch_unwind(AssertUnwindSafe(|| link(&mut c, &key, ALICE)));
    assert!(res.is_err(), "a flagged wallet cannot be linked");
}

#[test]
#[should_panic(expected = "only owner")]
fn flagging_is_owner_only() {
    let mut c = setup();
    ctx(ALICE, NOW, 0);
    c.flag_wallet_state(vec![evm_wallet(&sk(23)).to_string()]);
}

#[test]
fn withdraw_evm_refunds_nothing_when_the_relayer_never_signs() {
    // Stays Pending until processed: the balance is already debited, the signing is separate.
    let mut c = ready(E18);
    fund_alice(&mut c, 24, 1, 5);
    let id = withdraw(&mut c, 5, RECIPIENT);
    assert_eq!(balance(&c, ALICE, ETH), 0);
    assert_eq!(c.get_withdrawal(id).unwrap().status, "Pending");
}
