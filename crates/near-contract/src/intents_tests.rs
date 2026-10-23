//! Contract-level tests for EVM intents: submission, the trigger in `report_evm_deposit`, retry,
//! cancellation, expiry and the open-intent limit.

use k256::ecdsa::SigningKey;
use near_sdk::serde_json;

use super::*;
use crate::accounts::{
    action_message, cancel_intent_message, engine_id_for, intent_message, owner_hex,
    owner_of_account, LinkRequest, WalletId,
};
use crate::accounts_tests::{evm_sign, evm_wallet, hex, sk};
use crate::intents::{Intent, MAX_OPEN_INTENTS};
use crate::oneclick::Receiver;
use crate::oneclick_tests::{
    acct, assert_owed, ctx, deliver_msg, evm_pool, hop, setup, usdc_eth_pool, ALICE, BASE_SEPOLIA,
    BOB, ETH, EXPIRES, NOW, RELAYER, SELF, TO_EVM, USDC, USDC_EVM,
};

/// One ETH deposit, the size of every test deposit.
const DEPOSIT: u128 = 1_000_000_000_000_000; // 1e15

fn sign_hex(key: &SigningKey, msg: &str) -> String {
    format!("0x{}", hex(&evm_sign(key, msg)))
}

fn wallet_of(key: &SigningKey) -> String {
    evm_wallet(key).to_string()
}

/// The EVM address of `key` without the `evm:` prefix, as the relayer reports it.
fn address_of(key: &SigningKey) -> String {
    wallet_of(key).strip_prefix("evm:").unwrap().to_string()
}

/// The internal owner id of a wallet that is not linked.
fn owner_of_wallet(key: &SigningKey) -> String {
    owner_hex(&engine_id_for(&evm_wallet(key)))
}

fn swap_intent(token_in: &str, amount: u128, pool: u64, min_out: u128, to: &str) -> Intent {
    Intent::Swap {
        token_in: token_in.into(),
        amount_in: U128(amount),
        path: vec![hop(pool, token_in)],
        min_amount_out: U128(min_out),
        to: to.parse::<Receiver>().unwrap(),
    }
}

/// Submits `intent` for the wallet `key`, relayed by BOB, and returns the intent id.
fn submit(c: &mut Contract, key: &SigningKey, intent: &Intent, nonce: u64, expires: u64) -> u64 {
    let json = serde_json::to_string(intent).unwrap();
    let msg = intent_message(&acct(SELF), &evm_wallet(key), nonce, expires, &json);
    ctx(BOB, NOW);
    c.submit_intent(
        wallet_of(key),
        intent.clone(),
        nonce,
        expires,
        sign_hex(key, &msg),
    )
}

/// The relayer reports an ETH deposit of `amount` from the wallet `key`. Returns the result.
fn deposit_eth(c: &mut Contract, key: &SigningKey, amount: u128, block_secs: u64, tx: u64) -> bool {
    ctx(RELAYER, block_secs);
    c.report_evm_deposit(
        BASE_SEPOLIA,
        format!("0x{tx:064x}"),
        0,
        address_of(key),
        "native".into(),
        U128(amount),
    )
}

fn status_of(c: &Contract, id: u64) -> String {
    c.get_intent(id).expect("intent exists").status
}

fn eth_of_owner(c: &Contract, owner: &str) -> u128 {
    c.get_balance_of_owner(owner.to_string(), ETH.into()).0
}

#[test]
fn an_intent_runs_on_the_deposit_and_pays_out_to_evm() {
    let (mut c, pool) = with_pool();
    let key = sk(7);
    let quote = c.quote_exact_input(vec![hop(pool, ETH)], U128(DEPOSIT)).0;
    let id = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, &format!("evm:{TO_EVM}")),
        1,
        EXPIRES,
    );
    assert_eq!(id, 0);
    assert_eq!(status_of(&c, id), "Open");

    assert!(deposit_eth(&mut c, &key, DEPOSIT, NOW, 1));

    let view = c.get_intent(id).unwrap();
    assert_eq!(view.status, "Executed");
    match view.result {
        Some(crate::intents::IntentResult::Swap { amount_out, .. }) => {
            assert_eq!(amount_out.0, quote)
        }
        other => panic!("expected a swap result, got {}", other.is_some()),
    }
    // The payout is a pending EVM withdrawal of the output asset to the recipient.
    let pending = c.get_pending_withdrawals(0, 10);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].token_id, USDC_EVM);
    assert_eq!(pending[0].amount.0, quote);
    assert_eq!(pending[0].to_address, TO_EVM);
    // The owner's ETH is spent.
    assert_eq!(eth_of_owner(&c, &owner_of_wallet(&key)), 0);
    // The deposit is counted once, as held tokens, and the payout is owed until broadcast.
    assert_owed(&c, ETH, DEPOSIT);
    assert_owed(&c, USDC_EVM, 0);
}

#[test]
fn a_failed_intent_is_marked_and_the_deposit_stays_credited() {
    let (mut c, pool) = with_pool();
    let key = sk(8);
    let quote = c.quote_exact_input(vec![hop(pool, ETH)], U128(DEPOSIT)).0;
    let id = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, quote + 1, "internal"),
        1,
        EXPIRES,
    );

    // The report succeeds even though the intent cannot run.
    assert!(deposit_eth(&mut c, &key, DEPOSIT, NOW, 2));

    let view = c.get_intent(id).unwrap();
    assert_eq!(view.status, "Failed");
    assert!(view.reason.is_some(), "the failure has a reason");
    assert_eq!(
        eth_of_owner(&c, &owner_of_wallet(&key)),
        DEPOSIT,
        "deposit kept"
    );
    assert!(c.get_pending_withdrawals(0, 10).is_empty());
    assert_owed(&c, ETH, DEPOSIT);
}

#[test]
fn at_most_one_intent_runs_per_report_and_execute_intent_runs_the_next() {
    let (mut c, pool) = with_pool();
    let key = sk(9);
    let first = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
        1,
        EXPIRES,
    );
    let second = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
        2,
        EXPIRES,
    );

    // Enough for both, but one report runs one intent.
    assert!(deposit_eth(&mut c, &key, 2 * DEPOSIT, NOW, 3));
    assert_eq!(status_of(&c, first), "Executed");
    assert_eq!(status_of(&c, second), "Open");
    assert_eq!(eth_of_owner(&c, &owner_of_wallet(&key)), DEPOSIT);

    // Anyone may retry it, and the balance is there.
    ctx(BOB, NOW);
    let view = c.execute_intent(second);
    assert_eq!(view.status, "Executed");
    assert_eq!(eth_of_owner(&c, &owner_of_wallet(&key)), 0);
    assert_owed(&c, ETH, 2 * DEPOSIT);
}

#[test]
#[should_panic(expected = "insufficient")]
fn execute_intent_needs_the_balance() {
    let (mut c, pool) = with_pool();
    let key = sk(10);
    let id = submit(
        &mut c,
        &key,
        &swap_intent(ETH, 5 * DEPOSIT, pool, 1, "internal"),
        1,
        EXPIRES,
    );
    deposit_eth(&mut c, &key, DEPOSIT, NOW, 4);
    assert_eq!(status_of(&c, id), "Open", "too small: not run");
    ctx(BOB, NOW);
    c.execute_intent(id);
}

#[test]
fn an_intent_only_runs_on_its_own_token() {
    let (mut c, pool) = with_pool();
    let key = sk(11);
    let id = submit(
        &mut c,
        &key,
        &swap_intent(USDC_EVM, 1, pool, 1, "internal"),
        1,
        EXPIRES,
    );
    // An ETH deposit does not trigger an intent that spends USDC.
    deposit_eth(&mut c, &key, DEPOSIT, NOW, 5);
    assert_eq!(status_of(&c, id), "Open");
}

#[test]
#[should_panic(expected = "wallet signature does not verify")]
fn a_signature_from_another_key_is_refused() {
    let (mut c, pool) = with_pool();
    let intent = swap_intent(ETH, DEPOSIT, pool, 1, "internal");
    let json = serde_json::to_string(&intent).unwrap();
    let msg = intent_message(&acct(SELF), &evm_wallet(&sk(12)), 1, EXPIRES, &json);
    ctx(BOB, NOW);
    c.submit_intent(
        wallet_of(&sk(13)),
        intent,
        1,
        EXPIRES,
        sign_hex(&sk(12), &msg),
    );
}

#[test]
#[should_panic(expected = "wallet signature does not verify")]
fn a_signed_intent_cannot_be_changed() {
    let (mut c, pool) = with_pool();
    let signed = swap_intent(ETH, DEPOSIT, pool, 1, "internal");
    let json = serde_json::to_string(&signed).unwrap();
    let msg = intent_message(&acct(SELF), &evm_wallet(&sk(14)), 1, EXPIRES, &json);
    // The relayer submits a larger amount than the wallet signed.
    let changed = swap_intent(ETH, 2 * DEPOSIT, pool, 1, "internal");
    ctx(BOB, NOW);
    c.submit_intent(
        wallet_of(&sk(14)),
        changed,
        1,
        EXPIRES,
        sign_hex(&sk(14), &msg),
    );
}

#[test]
#[should_panic(expected = "action nonce was already used for this wallet")]
fn the_nonce_is_shared_with_execute() {
    let (mut c, pool) = with_pool();
    let key = sk(15);
    submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
        9,
        EXPIRES,
    );

    // A signed action with the same nonce is refused.
    let action = serde_json::to_string(&crate::SignedAction::Distribute).unwrap();
    let msg = action_message(&acct(SELF), &evm_wallet(&key), 9, EXPIRES, &action);
    ctx(BOB, NOW);
    let _ = c.execute(
        wallet_of(&key),
        crate::SignedAction::Distribute,
        9,
        EXPIRES,
        sign_hex(&key, &msg),
    );
}

#[test]
#[should_panic(expected = "intent has expired")]
fn an_expired_intent_cannot_be_submitted() {
    let (mut c, pool) = with_pool();
    let key = sk(16);
    let intent = swap_intent(ETH, DEPOSIT, pool, 1, "internal");
    let json = serde_json::to_string(&intent).unwrap();
    let msg = intent_message(&acct(SELF), &evm_wallet(&key), 1, NOW + 10, &json);
    ctx(BOB, NOW + 20);
    c.submit_intent(wallet_of(&key), intent, 1, NOW + 10, sign_hex(&key, &msg));
}

#[test]
fn an_expired_intent_does_not_run_and_reads_as_expired() {
    let (mut c, pool) = with_pool();
    let key = sk(17);
    let expires = NOW + 100;
    let id = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
        1,
        expires,
    );
    // The deposit arrives after the expiry: nothing runs, the deposit stays.
    assert!(deposit_eth(&mut c, &key, DEPOSIT, expires + 1, 6));
    assert_eq!(status_of(&c, id), "Expired");
    assert_eq!(eth_of_owner(&c, &owner_of_wallet(&key)), DEPOSIT);
}

#[test]
fn the_open_intents_of_an_owner_are_capped() {
    let (mut c, pool) = with_pool();
    let key = sk(18);
    for nonce in 1..=MAX_OPEN_INTENTS as u64 {
        // Nothing is deposited, so every intent stays open.
        submit(
            &mut c,
            &key,
            &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
            nonce,
            EXPIRES,
        );
    }
    let owner = owner_of_wallet(&key);
    let listed = c.get_intents_of_owner(owner.clone(), 0, 100);
    assert_eq!(listed.len(), MAX_OPEN_INTENTS);
    assert_eq!(listed[0].id, 0, "submission order");
    let page = c.get_intents_of_owner(owner, 6, 1);
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, 6);
}

#[test]
#[should_panic(expected = "already has 8 open intents")]
fn a_ninth_open_intent_is_refused() {
    let (mut c, pool) = with_pool();
    let key = sk(19);
    for nonce in 1..=(MAX_OPEN_INTENTS as u64 + 1) {
        submit(
            &mut c,
            &key,
            &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
            nonce,
            EXPIRES,
        );
    }
}

#[test]
fn the_wallet_can_cancel_its_intent() {
    let (mut c, pool) = with_pool();
    let key = sk(20);
    let id = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
        1,
        EXPIRES,
    );

    let msg = cancel_intent_message(&acct(SELF), &evm_wallet(&key), 2, EXPIRES, id);
    ctx(BOB, NOW);
    c.cancel_intent(wallet_of(&key), id, 2, EXPIRES, sign_hex(&key, &msg));
    let view = c.get_intent(id).unwrap();
    assert_eq!(view.status, "Failed");
    assert_eq!(view.reason.as_deref(), Some("cancelled by owner"));

    // A cancelled intent does not run on a later deposit.
    deposit_eth(&mut c, &key, DEPOSIT, NOW, 7);
    assert_eq!(status_of(&c, id), "Failed");
    assert_eq!(eth_of_owner(&c, &owner_of_wallet(&key)), DEPOSIT);
}

#[test]
#[should_panic(expected = "the wallet did not submit this intent")]
fn another_wallet_cannot_cancel_the_intent() {
    let (mut c, pool) = with_pool();
    let key = sk(21);
    let other = sk(22);
    let id = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
        1,
        EXPIRES,
    );
    let msg = cancel_intent_message(&acct(SELF), &evm_wallet(&other), 1, EXPIRES, id);
    ctx(BOB, NOW);
    c.cancel_intent(wallet_of(&other), id, 1, EXPIRES, sign_hex(&other, &msg));
}

#[test]
fn the_owning_near_account_can_cancel_a_linked_wallets_intent() {
    let (mut c, pool) = with_pool();
    let key = sk(23);
    // The wallet is linked to ALICE, so its intents belong to ALICE's owner.
    let req = LinkRequest {
        wallet: evm_wallet(&key),
        account: acct(ALICE),
        contract: acct(SELF),
        nonce: 1,
        expires: EXPIRES,
    };
    ctx(ALICE, NOW);
    c.link_wallet(wallet_of(&key), 1, EXPIRES, sign_hex(&key, &req.message()));

    let id = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
        2,
        EXPIRES,
    );
    assert_eq!(
        c.get_intent(id).unwrap().owner_hex,
        owner_hex(&owner_of_account(&acct(ALICE)))
    );

    ctx(ALICE, NOW);
    c.cancel_intent_near(id);
    assert_eq!(status_of(&c, id), "Failed");
}

#[test]
#[should_panic(expected = "the caller does not own this intent")]
fn a_near_account_that_does_not_own_the_intent_cannot_cancel_it() {
    let (mut c, pool) = with_pool();
    let key = sk(24);
    let id = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
        1,
        EXPIRES,
    );
    ctx(ALICE, NOW);
    c.cancel_intent_near(id);
}

#[test]
fn an_ed25519_wallet_signs_an_intent_that_runs_once_its_owner_is_funded() {
    use ed25519_dalek::{Signer, SigningKey};

    let mut c = setup();
    let pool = usdc_eth_pool(&mut c);
    let key = SigningKey::from_bytes(&[42u8; 32]);
    let wallet = WalletId::Ed25519(key.verifying_key().to_bytes());
    let amount = 1_000_000u128;
    let intent = Intent::Swap {
        token_in: USDC.into(),
        amount_in: U128(amount),
        path: vec![hop(pool, USDC)],
        min_amount_out: U128(1),
        to: Receiver::Internal,
    };
    let json = serde_json::to_string(&intent).unwrap();
    let msg = intent_message(&acct(SELF), &wallet, 1, EXPIRES, &json);
    let sig = key.sign(msg.as_bytes());
    ctx(BOB, NOW);
    let id = c.submit_intent(wallet.to_string(), intent, 1, EXPIRES, hex(&sig.to_bytes()));
    assert_eq!(status_of(&c, id), "Open");

    // A NEP-141 deposit that names the wallet credits its owner. No relayer report is involved,
    // so the intent waits until it is retried.
    let owner = owner_hex(&engine_id_for(&wallet));
    let refund = deliver_msg(
        &mut c,
        USDC,
        BOB,
        amount,
        &format!("{{\"owner\":\"{wallet}\"}}"),
    );
    assert_eq!(refund, 0);
    assert_eq!(status_of(&c, id), "Open");

    ctx(BOB, NOW);
    let view = c.execute_intent(id);
    assert_eq!(view.status, "Executed");
    assert_eq!(c.get_balance_of_owner(owner.clone(), USDC.into()).0, 0);
    let quote_out = match view.result {
        Some(crate::intents::IntentResult::Swap { amount_out, .. }) => amount_out.0,
        _ => panic!("expected a swap result"),
    };
    assert!(quote_out > 0);
    assert_eq!(c.get_balance_of_owner(owner, ETH.into()).0, quote_out);
}

#[test]
#[should_panic(expected = "path must start with token_in")]
fn an_intent_path_must_start_with_its_token() {
    let (mut c, pool) = with_pool();
    let key = sk(25);
    let intent = Intent::Swap {
        token_in: ETH.into(),
        amount_in: U128(DEPOSIT),
        path: vec![hop(pool, USDC_EVM)],
        min_amount_out: U128(1),
        to: Receiver::Internal,
    };
    submit(&mut c, &key, &intent, 1, EXPIRES);
}

#[test]
fn intents_are_listed_in_submission_order() {
    let (mut c, pool) = with_pool();
    let key = sk(26);
    let a = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
        1,
        EXPIRES,
    );
    let b = submit(
        &mut c,
        &key,
        &swap_intent(ETH, DEPOSIT, pool, 1, "internal"),
        2,
        EXPIRES,
    );
    let listed = c.get_intents_of_owner(owner_of_wallet(&key), 0, 10);
    assert_eq!(listed.iter().map(|v| v.id).collect::<Vec<_>>(), vec![a, b]);
    assert_eq!(listed[0].wallet, wallet_of(&key));
}

/// Registers the EVM assets and a relayer, and makes the ETH/USDC pool with its liquidity.
fn with_pool() -> (Contract, u64) {
    let mut c = setup();
    let pool = evm_pool(&mut c);
    ctx(crate::oneclick_tests::OWNER, NOW);
    c.add_relayer(acct(RELAYER));
    (c, pool)
}

/// The canonical JSON that intent signatures cover. The relayer must produce exactly this.
#[test]
fn intent_json_is_the_canonical_form() {
    let swap = Intent::Swap {
        token_in: ETH.into(),
        amount_in: U128(1_000),
        path: vec![hop(0, ETH)],
        min_amount_out: U128(1),
        to: Receiver::Evm([0x22; 20]),
    };
    assert_eq!(
        serde_json::to_string(&swap).unwrap(),
        "{\"Swap\":{\"token_in\":\"base-sepolia:eth\",\"amount_in\":\"1000\",\"path\":[{\"pool_id\":0,\"token_in\":\"base-sepolia:eth\"}],\"min_amount_out\":\"1\",\"to\":\"evm:0x2222222222222222222222222222222222222222\"}}"
    );
    let add = Intent::AddLiquidity {
        token_in: USDC.into(),
        amount_in: U128(5),
        pool_id: 3,
        tick_lower: -300,
        tick_upper: 300,
        min_liquidity: U128(0),
    };
    assert_eq!(
        serde_json::to_string(&add).unwrap(),
        "{\"AddLiquidity\":{\"token_in\":\"usdc.fakes.testnet\",\"amount_in\":\"5\",\"pool_id\":3,\"tick_lower\":-300,\"tick_upper\":300,\"min_liquidity\":\"0\"}}"
    );
}
