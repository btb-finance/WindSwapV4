//! Wind Swap v4 NEAR contract, v0 (testnet demo).
//!
//! Thin wrapper around `windswap-engine`. Every pool operation is delegated to the engine;
//! this crate only handles NEAR concerns: caller mapping, internal token balances, token
//! deposits and withdrawals, storage and panics. An engine `Err` becomes `env::panic_str`,
//! which makes NEAR roll back the whole call.
//!
//! Tokens:
//! - A token id is the NEP-141 token contract's `AccountId` as a string (e.g. `usdc.fakes.testnet`).
//! - Deposits: the owner whitelists tokens with `add_token`. A user sends tokens with
//!   `ft_transfer_call` to this contract, which lands in `ft_on_transfer` and credits the
//!   internal balance. Non-whitelisted tokens are refunded in full.
//! - Withdrawals: `withdraw` debits the internal balance, then calls `ft_transfer`. If the
//!   transfer fails, `on_withdraw` credits the balance back.
//!
//! Accounts (see `accounts.rs` for the formats):
//! - Internal balances and positions belong to an `Owner` (32 bytes). A NEAR account owns
//!   `sha256(account)`; a wallet with no NEAR account owns `sha256(wallet string)`.
//! - `link_wallet` links an EVM or Ed25519 wallet to the caller's account. `unlink_wallet`
//!   removes it. A linked wallet acts for the account's owner.
//! - `execute` runs a `SignedAction` that a wallet signed. Anyone may relay it and pay the
//!   NEAR gas. Deposits with `ft_transfer_call` and `msg = {"owner":"<wallet>"}` credit the
//!   wallet's owner instead of the sender.
//!
//! v0 limitations (intentional, for the testnet demo):
//! - `ft_on_transfer` reads `msg.owner`. A message with a `swap` or `add_liquidity` key runs
//!   that one-step flow (see `oneclick`), and a failed one is refunded. Any other message is a
//!   plain deposit.
//! - EVM intents (v8): a wallet signs an intent once, and it runs from the owner's balance when
//!   the relayer reports the deposit (see `intents`).
//! - EVM deposits and withdrawals (v5): the relayer reports deposits with `report_evm_deposit`
//!   and signs withdrawals with `process_withdrawal`. The owner registers EVM assets and chains.
//!   See the "EVM bridge" section below.
//! - `create_pool` accepts token ids that parse as account ids, or registered EVM asset ids.
//!   It does not require the tokens to be whitelisted.
//! - `test_credit` is an owner-only faucet, compiled only with the `testnet-faucet` feature
//!   (on by default). Build mainnet with `--no-default-features`.
//! - Each pool is one Borsh blob in `pools`. This is fine for v0 but should move to
//!   per-tick / per-position storage later, since the blob grows with every tick.
//! - Fees start from `FeeConfig::default()`. The owner changes them with `enable_tick_spacing`,
//!   `set_custom_swap_fee`, `set_custom_unstaked_fee` and `set_fee_whitelist`.
//! - No gauges: `unstaked_fee` is always computed with `gauge_alive = false`. So a custom
//!   unstaked fee is stored but does not change any swap output yet.
//! - Multi-hop routes (`swap_exact_input`, `swap_exact_output`, quotes) run on working copies
//!   of the touched pools. Only a successful swap writes them back.
//! - Chain Signatures: `request_evm_payout` has the MPC network sign an EIP-1559 transfer and
//!   returns the signed raw transaction. See `evm.rs` and docs/chain-signatures.md.
//!
//! EVM bridge (v5):
//! - Assets: the owner registers `(chain_id, asset)` with `register_evm_asset`. `asset` is
//!   `native` or an ERC-20 address. Each asset has an internal `token_id` (e.g.
//!   `base-sepolia:eth`) that balances, pools and withdrawals use. It is not a NEAR account.
//! - Chains: `set_evm_chain` stores the MPC signing path, the next nonce and the max fee cap.
//!   The nonce only goes up. The owner cannot lower it, so a nonce cannot be reused.
//! - Deposits: a relayer calls `report_evm_deposit` for each observed transfer. Each
//!   `(chain_id, tx_hash, log_index, asset)` is credited once (deposits credited before the asset
//!   was part of the key are still checked by their `(chain_id, tx_hash, log_index)`). The credit goes to the owner of `from`
//!   (its linked account, or its own id).
//! - Withdrawals: `withdraw_evm` (NEAR account) or the signed `WithdrawEvm` debits the balance
//!   and records a `Pending` withdrawal. A relayer calls `process_withdrawal`, which reserves
//!   the chain's next nonce and has the MPC sign the transfer. The callback stores `Signed`.
//!   The relayer broadcasts it and calls `mark_withdrawal_broadcast`.
//! - Wallet linking: `link_wallet` refuses a wallet whose own owner id holds balances or
//!   positions (`has_state`), so no balance can become unreachable after linking.

mod accounting;
mod evm;
mod intents;
mod oneclick;
mod permissionless;
mod tokenomics;
mod views;

use std::collections::BTreeMap;

use near_sdk::json_types::U128;
use near_sdk::store::{IterableMap, IterableSet, LazyOption, LookupMap, LookupSet};
use near_sdk::{
    assert_one_yocto, env, ext_contract, near, AccountId, BorshStorageKey, Gas, NearToken,
    PanicOnDefault, Promise, PromiseError, PromiseOrValue,
};
use windswap_engine::error::Result as EngineResult;
use windswap_engine::fees::FeeConfig;
use windswap_engine::pool::cl_pool::ClPool;
use windswap_engine::router::{Hop, Router};
use windswap_engine::{I256, U256};

use accounting::{Flow, HopFlow};
use accounts::{
    action_message, engine_id_for, owner_hex, owner_of_account, parse_owner_hex, parse_signature,
    verify_wallet_signature, LinkRegistry, LinkRequest, Owner, WalletId,
};
use tokenomics::{PendingEpoch, StakeKey};
use views::RangeKey;
use windswap_engine::gauge::{ClGauge, PositionManager};
use windswap_engine::minter::Minter;
use windswap_engine::rebase::RewardsDistributor;
use windswap_engine::ve::VotingEscrow;
use windswap_engine::voter::Voter;

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Pools,
    PoolTokens,
    /// v1-v3 balances, keyed by NEAR account. Read only by `migrate_balances`.
    Balances,
    AllowedTokens,
    // Appended in v4 so the prefixes above do not change.
    /// v4 balances, keyed by owner.
    OwnerBalances,
    /// Consumed `(wallet, nonce)` pairs of signed actions.
    ActionNonces,
    /// v4 pending withdrawals. Read only by `migrate`, which moves them to `Withdrawals`.
    PendingWithdrawals,
    // Appended in v5.
    Withdrawals,
    Relayers,
    EvmAssets,
    EvmAssetByToken,
    EvmChains,
    EvmDeposits,
    HasState,
    // Appended after the v5 prefixes above.
    /// Deposits keyed with the asset: `(chain_id, tx_hash, log_index, asset)`.
    EvmDepositsByAsset,
    // Appended in v6 (ve(3,3) tokenomics). Each engine state is one Borsh blob under its key.
    Ve,
    Voter,
    Minter,
    Distributor,
    /// One `ClGauge` per pool, keyed by pool id.
    Gauges,
    /// Staked positions, keyed by `StakeKey`.
    Stakes,
    /// The NFT manager of the staked positions.
    NftManager,
    // Appended in v7 (solvency accounting). See `accounting`.
    Liabilities,
    Accounted,
    PoolReserves,
    ReserveTotals,
    ReservesSeeded,
    PendingOwed,
    Unbacked,
    // Appended after v6 (the owner indexes, see `views`).
    PositionsByOwner,
    StakesByOwner,
    WithdrawalsByOwner,
    BalanceTokensByOwner,
    // Appended in v8 (one-step flows and intents). See `intents`.
    Intents,
    IntentsByOwner,
    OpenIntents,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner: AccountId,
    fee_config: FeeConfig,
    /// One Borsh blob per pool (v0). See the module docs.
    pools: IterableMap<u64, ClPool>,
    /// Token pair `(token0, token1)` for each pool id.
    pool_tokens: LookupMap<u64, (String, String)>,
    next_pool_id: u64,
    /// Balances of the deployed layout (v3), keyed by `(NEAR account, token id)`. Not used for
    /// new credits. `migrate_balances` moves each listed entry to `balances`.
    legacy_balances: LookupMap<(AccountId, String), u128>,
    /// Internal balances keyed by `(owner, token id)`. Missing key means zero.
    balances: LookupMap<(Owner, String), u128>,
    /// Tokens accepted by `ft_on_transfer`. Managed by the owner.
    allowed_tokens: IterableSet<AccountId>,
    /// The Chain Signatures MPC contract that `request_evm_payout` calls. Owner-settable.
    mpc_contract: AccountId,
    /// Wallet <-> owner links, and consumed link nonces.
    links: LinkRegistry,
    /// Consumed `(wallet, nonce)` pairs of signed actions. Never pruned.
    action_nonces: LookupSet<(WalletId, u64)>,
    /// EVM withdrawals, keyed by id. Ids are sequential and never removed.
    withdrawals: LookupMap<u64, Withdrawal>,
    next_withdrawal_id: u64,
    /// Accounts that may call `report_evm_deposit`, `process_withdrawal` and
    /// `mark_withdrawal_broadcast`.
    relayers: IterableSet<AccountId>,
    /// Registered EVM assets, keyed by `"{chain_id}:{asset}"`.
    evm_assets: IterableMap<String, EvmAsset>,
    /// Internal token id -> key of its EVM asset in `evm_assets`.
    evm_asset_by_token: LookupMap<String, String>,
    /// EVM chain settings (signing path, next nonce, fee cap), keyed by chain id.
    evm_chains: LookupMap<u64, EvmChain>,
    /// `(chain_id, tx_hash, log_index)` of every deposit credited before `evm_deposits_by_asset`
    /// existed. Read only: a report is a duplicate if its legacy key or its asset key is here.
    evm_deposits: LookupSet<(u64, [u8; 32], u32)>,
    /// `(chain_id, tx_hash, log_index, asset)` of every deposit credited since. The asset is in the
    /// key because a native transfer and an ERC-20 `Transfer` can both be log 0 of one tx.
    evm_deposits_by_asset: LookupSet<(u64, [u8; 32], u32, String)>,
    /// Owner ids that have ever been credited or had a position minted. A wallet whose own owner
    /// id is in this set cannot be linked. Set in `credit` and `mint_for`, and by
    /// `flag_wallet_state` for data from before v5.
    has_state: LookupSet<Owner>,
    // Appended in v6. The ve(3,3) state is one Borsh blob per engine object (see `tokenomics`).
    /// The WIND NEP-141 token, set once by the owner. Minted only by this contract.
    wind_token: Option<AccountId>,
    ve: LazyOption<VotingEscrow>,
    voter: LazyOption<Voter>,
    minter: LazyOption<Minter>,
    distributor: LazyOption<RewardsDistributor>,
    /// One gauge per pool that has one, keyed by pool id.
    gauges: LookupMap<u64, ClGauge>,
    /// Staked positions: `(owner, pool, tick_lower, tick_upper)` to the NFT id.
    stakes: LookupMap<StakeKey, U256>,
    /// The NFT manager that holds every staked position (the pools' `nft` account).
    nft_manager: LazyOption<PositionManager>,
    /// Set once the genesis WIND supply is minted, and while that mint is in flight.
    wind_genesis_done: bool,
    wind_genesis_pending: bool,
    /// Total WIND minted by `distribute`. The WIND supply the minter sees.
    wind_minted: u128,
    /// The epoch whose mint is in flight, if any. See `tokenomics`.
    pending_epoch: Option<PendingEpoch>,
    // Appended in v7 (solvency accounting). See `accounting` for what each total covers.
    /// Per token: sum of the internal balances that are counted (see `accounting`).
    liabilities: LookupMap<String, u128>,
    /// `(owner, token)` keys whose balance is already in `liabilities`. Used only if `needs_seed`.
    accounted: LookupSet<(Owner, String)>,
    /// True on a migrated contract: balances are counted on first touch, and the owner seeds
    /// the rest. False on a fresh deployment, which counts every change from the start.
    needs_seed: bool,
    /// Per `(pool, token)`: tokens held for the pool, its gauge and its NFT manager. Signed, so
    /// a missing seed shows up as a negative reserve instead of a panic in a swap.
    pool_reserves: LookupMap<(u64, String), i128>,
    /// Per token: the sum of `pool_reserves` over the pools.
    reserve_totals: LookupMap<String, i128>,
    /// `(pool, token)` pairs whose reserve was set by `seed_pool_reserves`.
    reserves_seeded: LookupSet<(u64, String)>,
    /// Per token: EVM withdrawals that are debited but not yet broadcast.
    pending_withdrawals: LookupMap<String, u128>,
    /// Per token: faucet credit (`test_credit`) that no deposit backs.
    unbacked: LookupMap<String, u128>,
    // Appended after v6. Owner indexes for the owner-addressed views (see `views`). They start
    // empty at `migrate` and are filled by the `backfill_*` calls.
    /// Position ranges per owner that have liquidity, owed fees or a stake.
    positions_by_owner: LookupMap<Owner, Vec<RangeKey>>,
    /// Staked position ranges per owner.
    stakes_by_owner: LookupMap<Owner, Vec<RangeKey>>,
    /// Withdrawal ids per owner, in creation order.
    withdrawals_by_owner: LookupMap<Owner, Vec<u64>>,
    /// Tokens with a nonzero internal balance, per owner.
    balance_tokens_by_owner: LookupMap<Owner, Vec<String>>,
    // Appended in v8 (one-step flows and intents, see `intents`).
    /// Intents by id.
    intents: LookupMap<u64, intents::StoredIntent>,
    next_intent_id: u64,
    /// Every intent id per owner, in submission order.
    intents_by_owner: LookupMap<Owner, Vec<u64>>,
    /// Open intent ids per owner, oldest first. At most `intents::MAX_OPEN_INTENTS`.
    open_intents: LookupMap<Owner, Vec<u64>>,
    // Appended in v8 (permissionless listing). See `permissionless`.
    permissionless: permissionless::Permissionless,
}

#[derive(Clone)]
#[near(serializers = [borsh])]
struct Withdrawal {
    owner: Owner,
    token_id: String,
    amount: u128,
    to_chain_id: u64,
    to_address: [u8; 20],
    status: WithdrawalStatus,
    /// Set while the MPC signature is in flight. The nonce is already reserved.
    signing_nonce: Option<u64>,
}

#[derive(Clone)]
#[near(serializers = [borsh])]
enum WithdrawalStatus {
    /// Waiting for `process_withdrawal`.
    Pending,
    /// Signed by the MPC network. `raw_tx` is the 0x-hex signed transaction.
    Signed { nonce: u64, raw_tx: String },
    /// Broadcast by the relayer. `tx_hash` is the 0x-hex transaction hash.
    Broadcast { nonce: u64, tx_hash: String },
    /// Not signed, and the nonce is not reused. Its balance was credited back. The nonce is a
    /// gap that the relayer must fill, for example with a self-transfer.
    Failed { nonce: u64 },
}

/// A registered EVM asset.
#[derive(Clone)]
#[near(serializers = [borsh])]
struct EvmAsset {
    chain_id: u64,
    /// `native` or a lower-case 0x ERC-20 address.
    asset: String,
    token_id: String,
    decimals: u8,
    per_deposit_cap: u128,
    daily_withdraw_cap: u128,
    /// UTC day (unix seconds / 86400) of the current withdrawal window.
    window_day: u64,
    /// Amount reserved by `process_withdrawal` in the window of `window_day`.
    window_withdrawn: u128,
}

/// EVM chain settings.
#[derive(Clone)]
#[near(serializers = [borsh])]
struct EvmChain {
    /// MPC derivation path. Its address is the chain's vault.
    path: String,
    next_nonce: u64,
    max_fee_per_gas_cap: u128,
}

#[near(serializers = [json])]
pub struct EvmAssetView {
    pub chain_id: u64,
    pub asset: String,
    pub token_id: String,
    pub decimals: u8,
    pub per_deposit_cap: U128,
    pub daily_withdraw_cap: U128,
    /// False for an auto-registered token until the owner verifies it.
    pub verified: bool,
    /// Informational symbol reported by the relayer. Empty for owner-registered assets.
    pub symbol: String,
}

#[near(serializers = [json])]
pub struct EvmChainView {
    pub chain_id: u64,
    pub path: String,
    pub next_nonce: u64,
    pub max_fee_per_gas_cap: U128,
}

/// JSON view of a withdrawal.
#[near(serializers = [json])]
pub struct WithdrawalView {
    pub id: u64,
    /// Owner id, 64 hex digits.
    pub owner_hex: String,
    pub token_id: String,
    pub amount: U128,
    pub to_chain_id: u64,
    /// 0x-prefixed EVM address.
    pub to_address: String,
    /// `Pending`, `Signed`, `Broadcast` or `Failed`.
    pub status: String,
    pub nonce: Option<u64>,
    pub raw_tx: Option<String>,
    pub tx_hash: Option<String>,
}

/// The default MPC contract on NEAR testnet. `set_mpc_contract` changes it.
const DEFAULT_MPC_CONTRACT: &str = "v1.signer-prod.testnet";

/// Gas for the `sign` call. The probe measured about 8.6 Tgas burnt over the whole receipt tree
/// for one signature, so this leaves room on the MPC side.
const SIGN_CALL_GAS_TGAS: u64 = 50;
/// Gas for `on_evm_signed`. It only parses the response and builds the transaction.
const CALLBACK_GAS_TGAS: u64 = 20;
/// `request_evm_payout` needs at least this much prepaid gas, so the sign call and the
/// callback both get their share.
const PAYOUT_MIN_PREPAID_GAS_TGAS: u64 = 100;
/// Gas limit of a native transfer.
const NATIVE_TRANSFER_GAS: u64 = 21_000;
/// Gas limit of an ERC-20 `transfer` call.
const ERC20_TRANSFER_GAS: u64 = 100_000;
/// The daily withdraw cap resets at each UTC day boundary.
const SECONDS_PER_DAY: u64 = 86_400;

#[near(serializers = [json])]
pub struct Slot0View {
    pub sqrt_price_x96: String,
    pub tick: i32,
    pub observation_index: u16,
    pub observation_cardinality: u16,
    pub observation_cardinality_next: u16,
}

#[near(serializers = [json])]
pub struct PoolView {
    pub pool_id: u64,
    pub token0: String,
    pub token1: String,
    pub initialized: bool,
    pub tick_spacing: i32,
    pub slot0: Slot0View,
    pub liquidity: U128,
    pub staked_liquidity: U128,
    pub fee_growth_global0_x128: String,
    pub fee_growth_global1_x128: String,
}

#[near(serializers = [json])]
pub struct PositionView {
    pub liquidity: U128,
    pub fee_growth_inside0_last_x128: String,
    pub fee_growth_inside1_last_x128: String,
    pub tokens_owed0: U128,
    pub tokens_owed1: U128,
}

/// One hop of a multi-hop route as passed by the caller. `token_in` is the token paid into
/// this hop. The output token is the other token of the pool.
#[derive(Clone)]
#[near(serializers = [json, borsh])]
pub struct HopInput {
    pub pool_id: u64,
    /// Token id: a NEP-141 account id or a registered internal token id (e.g. `base-sepolia:eth`).
    pub token_in: String,
}

/// An action a wallet signs and anyone may relay through `execute`.
///
/// The signed message contains `serde_json::to_string` of this enum, so its JSON must be
/// canonical: externally tagged, fields in declaration order, no extra whitespace. Amounts that
/// exceed 2^53 are strings (`String`, or `U128` which serializes as a string).
#[derive(Clone)]
#[near(serializers = [json])]
pub enum SignedAction {
    /// As `swap`.
    Swap {
        pool_id: u64,
        zero_for_one: bool,
        amount_specified: String,
        sqrt_price_limit_x96: String,
    },
    /// As `swap_exact_input`.
    SwapExactInput {
        path: Vec<HopInput>,
        amount_in: U128,
        min_amount_out: U128,
    },
    /// As `mint_position`.
    MintPosition {
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: U128,
    },
    /// As `burn_position`.
    BurnPosition {
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: U128,
    },
    /// As `collect`.
    Collect {
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    },
    /// Debits `amount` of `token` and records a pending withdrawal to an EVM address. Nothing is
    /// paid out on chain yet.
    WithdrawEvm {
        token: String,
        amount: U128,
        to_chain_id: u64,
        to_address: String,
    },
    /// As `create_lock`. `permanent` makes the lock permanent in the same step. It is left out
    /// of the JSON when false, so the message of a client that predates it is unchanged.
    CreateLock {
        amount: U128,
        duration: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        permanent: bool,
    },
    /// As `increase_amount`.
    IncreaseAmount { token_id: u64, amount: U128 },
    /// As `increase_unlock_time`.
    IncreaseUnlockTime { token_id: u64, duration: u64 },
    /// As `withdraw_lock`.
    WithdrawLock { token_id: u64 },
    /// As `merge`.
    Merge { from: u64, to: u64 },
    /// As `split`.
    Split { token_id: u64, amount: U128 },
    /// As `lock_permanent`.
    LockPermanent { token_id: u64 },
    /// As `unlock_permanent`.
    UnlockPermanent { token_id: u64 },
    /// As `delegate`.
    Delegate { delegator: u64, delegatee: u64 },
    /// As `vote`.
    Vote {
        token_id: u64,
        pools: Vec<u64>,
        weights: Vec<U128>,
    },
    /// As `reset`.
    Reset { token_id: u64 },
    /// As `poke`.
    Poke { token_id: u64 },
    /// As `stake_position`.
    StakePosition {
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    },
    /// As `unstake_position`.
    UnstakePosition {
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    },
    /// As `claim_gauge_rewards`.
    ClaimGaugeRewards {
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    },
    /// As `claim_fees`.
    ClaimFees { token_id: u64, gauges: Vec<u64> },
    /// As `claim_bribes`.
    ClaimBribes {
        token_id: u64,
        gauges: Vec<u64>,
        tokens: Vec<String>,
    },
    /// As `bribe`.
    Bribe {
        pool_id: u64,
        token: String,
        amount: U128,
    },
    /// As `claim_rebase`.
    ClaimRebase { token_id: u64 },
    /// As `distribute`. Runs the weekly epoch flip if it is due. Its result is `execute`'s
    /// promise, or `Amount` when no mint is needed.
    Distribute,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// What `execute` returns, one variant per `SignedAction`.
#[near(serializers = [json])]
pub enum ActionResult {
    Swap([String; 2]),
    SwapExactInput(U128),
    MintPosition([String; 2]),
    BurnPosition([String; 2]),
    Collect([U128; 2]),
    /// Id of the recorded pending withdrawal.
    WithdrawEvm(u64),
    /// Id of a new veNFT.
    TokenId(u64),
    /// An amount paid or returned.
    Amount(U128),
    /// The two new token ids of a split.
    Split((u64, u64)),
    /// Amounts paid per token.
    Paid(Vec<(String, U128)>),
    /// An action with no return value.
    Done,
}

/// The JSON `msg` of `ft_on_transfer`. Only `owner` is read.
#[near(serializers = [json])]
struct DepositMsg {
    owner: Option<String>,
    /// `{"lock": {"duration": <secs>}}`: lock the deposited WIND for `duration` seconds.
    lock: Option<LockMsg>,
}

/// The `lock` part of a `DepositMsg`.
#[near(serializers = [json])]
struct LockMsg {
    duration: u64,
}

/// A validated route: engine hops plus the tokens at both ends.
struct Route {
    hops: Vec<Hop>,
    token_in: String,
    token_out: String,
}

/// The trader used by the views. It is the all-zero engine account, so views quote the fees
/// a non-whitelisted account pays. Whitelisted accounts pay 0 on a swap, so a swap by them can
/// return more than the matching view.
const QUOTE_TRADER: [u8; 32] = [0u8; 32];

/// The NEP-141 interface of a token contract, used for withdrawals.
#[allow(dead_code)]
#[ext_contract(ext_ft)]
trait FungibleTokenCore {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
}

/// The Chain Signatures MPC contract. `sign` yields until the MPC nodes answer.
#[allow(dead_code)]
#[ext_contract(ext_mpc)]
trait ChainSignatures {
    fn sign(&mut self, request: SignRequest) -> MpcSignature;
}

/// This contract's own callbacks.
#[allow(dead_code)]
#[ext_contract(ext_self)]
trait SelfCallbacks {
    fn on_withdraw(&mut self, account: AccountId, token: AccountId, amount: U128);
    fn on_evm_signed(&mut self, tx: EvmTx);
    fn on_withdrawal_signed(&mut self, id: u64, day: u64, tx: EvmTx);
}

/// The `request` argument of the MPC `sign` call. Only secp256k1 (`domain_id` 0) is used here.
#[near(serializers = [json])]
pub struct SignRequest {
    pub path: String,
    pub payload_v2: Payload,
    pub domain_id: u64,
}

/// A 32-byte digest, hex without a prefix. `Ecdsa` is the only variant used here.
#[near(serializers = [json])]
pub enum Payload {
    Ecdsa(String),
    Eddsa(String),
}

/// The MPC response: `big_r` is a compressed point, `s` a scalar, both as hex.
#[near(serializers = [json])]
pub struct MpcSignature {
    pub big_r: AffinePoint,
    pub s: ScalarValue,
    pub recovery_id: u8,
    pub scheme: String,
}

#[near(serializers = [json])]
pub struct AffinePoint {
    pub affine_point: String,
}

#[near(serializers = [json])]
pub struct ScalarValue {
    pub scalar: String,
}

/// An EIP-1559 transfer as passed to `request_evm_payout`. It is also the argument of the
/// callback, so the callback can build the signed transaction from the same fields.
#[derive(Clone)]
#[near(serializers = [json])]
pub struct EvmTx {
    pub chain_id: u64,
    pub nonce: u64,
    /// The recipient, a 0x-prefixed 20-byte address.
    pub to: String,
    pub value_wei: U128,
    pub max_fee_per_gas: U128,
    pub max_priority_fee_per_gas: U128,
    pub gas_limit: u64,
    /// Calldata as 0x-hex. Empty (or `0x`) for a native transfer.
    pub data: String,
}

impl EvmTx {
    fn transfer(&self) -> Result<evm::Eip1559Transfer, String> {
        let transfer = evm::Eip1559Transfer {
            chain_id: self.chain_id,
            nonce: self.nonce,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas.0,
            max_fee_per_gas: self.max_fee_per_gas.0,
            gas_limit: self.gas_limit,
            to: evm::parse_address(&self.to)?,
            value: self.value_wei.0,
            data: evm::hex_decode(&self.data)?,
        };
        transfer.validate()?;
        Ok(transfer)
    }
}

/// Current block time in seconds, as the engine expects it.
fn now() -> u32 {
    u32::try_from(now_secs())
        .unwrap_or_else(|_| env::panic_str("block timestamp does not fit in u32"))
}

/// Current block time in unix seconds, for signature expiry.
fn now_secs() -> u64 {
    env::block_timestamp() / 1_000_000_000
}

/// Unwraps a result. On `Err` the call panics and NEAR reverts the state.
fn ok<T, E: std::fmt::Display>(result: Result<T, E>) -> T {
    result.unwrap_or_else(|e| env::panic_str(&format!("{e}")))
}

/// Checked `U256 -> u128`. Panics instead of truncating.
fn to_u128(x: U256) -> u128 {
    u128::try_from(x).unwrap_or_else(|_| env::panic_str("amount does not fit in u128"))
}

fn parse_u256(s: &str) -> U256 {
    s.parse::<U256>()
        .unwrap_or_else(|_| env::panic_str(&format!("invalid U256: {s}")))
}

fn parse_i256(s: &str) -> I256 {
    s.parse::<I256>()
        .unwrap_or_else(|_| env::panic_str(&format!("invalid I256: {s}")))
}

fn parse_wallet(s: &str) -> WalletId {
    ok(s.parse::<WalletId>())
}

/// The wallet named by a deposit `msg` of the form `{"owner":"<wallet>"}`. Any other message,
/// including one that is not JSON, names no owner. A wallet that does not parse panics.
fn parse_deposit_msg(msg: &str) -> Option<DepositMsg> {
    near_sdk::serde_json::from_str(msg).ok()
}

fn default_mpc_contract() -> AccountId {
    DEFAULT_MPC_CONTRACT
        .parse()
        .unwrap_or_else(|_| env::panic_str("invalid default MPC contract id"))
}

/// Normalises an EVM asset: `native`, or a 0x address in lower case.
fn normalize_asset(asset: &str) -> Result<String, String> {
    let lower = asset.to_ascii_lowercase();
    if lower == "native" {
        return Ok(lower);
    }
    evm::parse_address(&lower)
        .map_err(|_| "asset must be \"native\" or a 0x-prefixed 20-byte address".to_string())?;
    Ok(lower)
}

/// Key of an EVM asset in `evm_assets`.
fn evm_asset_key(chain_id: u64, asset: &str) -> String {
    format!("{chain_id}:{asset}")
}

/// A transaction hash: `0x` and 32 bytes of hex.
fn parse_tx_hash(s: &str) -> Result<[u8; 32], String> {
    if !s.starts_with("0x") {
        return Err("tx hash must start with 0x".into());
    }
    evm::hex_decode(s)?
        .try_into()
        .map_err(|_| "tx hash must be 32 bytes".to_string())
}

/// Checks the MPC response and assembles the signed raw transaction (0x-hex).
fn signed_raw_tx(tx: &EvmTx, sig: &MpcSignature) -> Result<String, String> {
    if sig.scheme != "Secp256k1" {
        return Err(format!("unexpected MPC scheme: {}", sig.scheme));
    }
    let transfer = tx.transfer()?;
    let big_r = evm::hex_decode(&sig.big_r.affine_point).map_err(|e| format!("big_r: {e}"))?;
    let r = evm::r_from_affine_point(&big_r)?;
    let s: [u8; 32] = evm::hex_decode(&sig.s.scalar)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| "s must be 32 bytes of hex".to_string())?;
    let raw = evm::assemble_signed(&transfer, &r, &s, sig.recovery_id)?;
    Ok(evm::hex_0x(&raw))
}

/// The JSON view of an EVM asset.
fn evm_asset_view(a: &EvmAsset, verified: bool, symbol: String) -> EvmAssetView {
    EvmAssetView {
        chain_id: a.chain_id,
        asset: a.asset.clone(),
        token_id: a.token_id.clone(),
        decimals: a.decimals,
        per_deposit_cap: U128(a.per_deposit_cap),
        daily_withdraw_cap: U128(a.daily_withdraw_cap),
        verified,
        symbol,
    }
}

/// The JSON view of a withdrawal.
fn withdrawal_view(id: u64, w: &Withdrawal) -> WithdrawalView {
    let (status, nonce, raw_tx, tx_hash): (&str, Option<u64>, Option<String>, Option<String>) =
        match &w.status {
            WithdrawalStatus::Pending => ("Pending", None, None, None),
            WithdrawalStatus::Signed { nonce, raw_tx } => {
                ("Signed", Some(*nonce), Some(raw_tx.clone()), None)
            }
            WithdrawalStatus::Broadcast { nonce, tx_hash } => {
                ("Broadcast", Some(*nonce), None, Some(tx_hash.clone()))
            }
            WithdrawalStatus::Failed { nonce } => ("Failed", Some(*nonce), None, None),
        };
    WithdrawalView {
        id,
        owner_hex: owner_hex(&w.owner),
        token_id: w.token_id.clone(),
        amount: U128(w.amount),
        to_chain_id: w.to_chain_id,
        to_address: evm::hex_0x(&w.to_address),
        status: status.to_string(),
        nonce,
        raw_tx,
        tx_hash,
    }
}

/// Panics unless the call carries enough prepaid gas for a sign call and its callback.
fn assert_payout_gas() {
    if env::prepaid_gas() < Gas::from_tgas(PAYOUT_MIN_PREPAID_GAS_TGAS) {
        env::panic_str(&format!(
            "attach at least {PAYOUT_MIN_PREPAID_GAS_TGAS} Tgas of prepaid gas"
        ));
    }
}

impl Contract {
    fn assert_owner(&self) {
        if env::predecessor_account_id() != self.owner {
            env::panic_str("only owner");
        }
    }

    fn assert_relayer(&self) {
        if !self.relayers.contains(&env::predecessor_account_id()) {
            env::panic_str("only relayer");
        }
    }

    /// The registered EVM asset of an internal token id.
    fn evm_asset_of(&self, token_id: &str) -> Option<EvmAsset> {
        let key = self.evm_asset_by_token.get(token_id)?;
        self.evm_assets.get(key).cloned()
    }

    /// Pool tokens are NEAR accounts (NEP-141) or registered EVM asset ids.
    fn validate_pool_token(&self, token: &str) {
        if token.parse::<AccountId>().is_ok() || self.evm_asset_by_token.contains_key(token) {
            return;
        }
        env::panic_str(&format!("invalid token id: {token}"));
    }

    fn pool_ref(&self, pool_id: u64) -> &ClPool {
        self.pools
            .get(&pool_id)
            .unwrap_or_else(|| env::panic_str(&format!("pool {pool_id} does not exist")))
    }

    fn pool_mut(&mut self, pool_id: u64) -> &mut ClPool {
        self.pools
            .get_mut(&pool_id)
            .unwrap_or_else(|| env::panic_str(&format!("pool {pool_id} does not exist")))
    }

    fn tokens_of(&self, pool_id: u64) -> (String, String) {
        self.pool_tokens
            .get(&pool_id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(&format!("pool {pool_id} does not exist")))
    }

    fn balance_of(&self, owner: &Owner, token: &str) -> u128 {
        self.balances
            .get(&(*owner, token.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// The only writer of a credited balance. Also updates `liabilities` (see `accounting`).
    fn credit(&mut self, owner: &Owner, token: &str, amount: u128) {
        if amount == 0 {
            return;
        }
        self.has_state.insert(*owner);
        let key = (*owner, token.to_string());
        let current = self.balances.get(&key).copied().unwrap_or(0);
        let next = current
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("balance overflow"));
        self.liability_add(&key, current, amount);
        self.balances.insert(key, next);
        if current == 0 {
            self.index_token_added(owner, token);
        }
    }

    /// The only writer of a debited balance. Also updates `liabilities` (see `accounting`).
    fn debit(&mut self, owner: &Owner, token: &str, amount: u128) {
        if amount == 0 {
            return;
        }
        let key = (*owner, token.to_string());
        let current = self.balances.get(&key).copied().unwrap_or(0);
        let next = current.checked_sub(amount).unwrap_or_else(|| {
            env::panic_str(&format!(
                "insufficient {token} balance: have {current}, need {amount}"
            ))
        });
        self.liability_sub(&key, current, amount);
        if next == 0 {
            self.balances.remove(&key);
            self.index_token_removed(owner, token);
        } else {
            self.balances.insert(key, next);
        }
    }

    /// Applies a signed pool delta for `owner`. Positive means the owner owes the pool
    /// (debit), negative means the pool owes the owner (credit).
    fn settle(&mut self, owner: &Owner, token: &str, delta: I256) {
        if delta.is_positive() {
            self.debit(owner, token, to_u128(delta.into_raw()));
        } else if delta.is_negative() {
            self.credit(owner, token, to_u128(delta.unsigned_abs()));
        }
    }

    /// Checks a path and turns it into engine hops. Panics on an empty path, a hop whose
    /// input token is not the previous hop's output, a token that is not in its pool, or an
    /// unknown pool.
    fn resolve_path(&self, path: &[HopInput]) -> Route {
        self.try_resolve_path(path)
            .unwrap_or_else(|e| env::panic_str(&e))
    }

    /// `resolve_path` that returns the reason instead of panicking. Changes nothing.
    fn try_resolve_path(&self, path: &[HopInput]) -> Result<Route, String> {
        if path.is_empty() {
            return Err("empty path".into());
        }
        let token_in = path[0].token_in.to_string();
        let mut current = token_in.clone();
        let mut hops = Vec::with_capacity(path.len());
        for (i, input) in path.iter().enumerate() {
            if input.token_in.as_str() != current.as_str() {
                return Err(format!("discontinuous path at hop {i}"));
            }
            let (token0, token1) = self
                .pool_tokens
                .get(&input.pool_id)
                .cloned()
                .ok_or_else(|| format!("pool {} does not exist", input.pool_id))?;
            let (zero_for_one, token_out) = if current == token0 {
                (true, token1)
            } else if current == token1 {
                (false, token0)
            } else {
                return Err(format!(
                    "token not in pool: {current} is not in pool {}",
                    input.pool_id
                ));
            };
            hops.push(Hop {
                pool_id: input.pool_id,
                zero_for_one,
            });
            current = token_out;
        }
        Ok(Route {
            hops,
            token_in,
            token_out: current,
        })
    }

    /// Working copies of the pools a route touches. The route runs on these copies, and only a
    /// successful swap writes them back.
    fn working_pools(&self, hops: &[Hop]) -> BTreeMap<u64, ClPool> {
        hops.iter()
            .map(|hop| (hop.pool_id, self.pool_ref(hop.pool_id).clone()))
            .collect()
    }

    /// Engine exact-input route on `pools`. Returns the engine error without panicking, so a
    /// caller can show that a failed route leaves the stored pools unchanged.
    ///
    /// The hops run one at a time through the engine's single-hop `Router::exact_input`, which
    /// is what the multi-hop call does internally. Each hop's amounts are returned, so the
    /// reserve of each pool can be updated. The output and the slippage check are the same.
    fn route_exact_input(
        &self,
        route: &Route,
        pools: &mut BTreeMap<u64, ClPool>,
        trader: [u8; 32],
        amount_in: u128,
        min_out: u128,
    ) -> EngineResult<Vec<HopFlow>> {
        let mut flows = Vec::with_capacity(route.hops.len());
        let mut amount = U256::from(amount_in);
        for hop in &route.hops {
            let out = Router {
                store: &mut *pools,
                fees: &self.fee_config,
                trader,
                time: now(),
            }
            .exact_input(std::slice::from_ref(hop), amount, U256::ZERO)?;
            flows.push(HopFlow {
                pool_id: hop.pool_id,
                zero_for_one: hop.zero_for_one,
                paid: amount,
                received: out,
            });
            amount = out;
        }
        if amount < U256::from(min_out) {
            return Err(windswap_engine::MathError::TooLittleReceived);
        }
        Ok(flows)
    }

    /// Engine exact-output route on `pools`. See `route_exact_input`. The hops run back to front,
    /// and each hop must deliver exactly what the next one needs, as in the multi-hop call.
    fn route_exact_output(
        &self,
        route: &Route,
        pools: &mut BTreeMap<u64, ClPool>,
        trader: [u8; 32],
        amount_out: u128,
        max_in: u128,
    ) -> EngineResult<Vec<HopFlow>> {
        let mut flows = Vec::with_capacity(route.hops.len());
        let mut needed = U256::from(amount_out);
        for hop in route.hops.iter().rev() {
            let paid = Router {
                store: &mut *pools,
                fees: &self.fee_config,
                trader,
                time: now(),
            }
            .exact_output(std::slice::from_ref(hop), needed, U256::MAX)?;
            flows.push(HopFlow {
                pool_id: hop.pool_id,
                zero_for_one: hop.zero_for_one,
                paid,
                received: needed,
            });
            needed = paid;
        }
        if needed > U256::from(max_in) {
            return Err(windswap_engine::MathError::TooMuchRequested);
        }
        flows.reverse();
        Ok(flows)
    }

    /// Applies the pool side of a route's hops: each hop's input enters its pool, and its output
    /// leaves it. Intermediate tokens net to zero across the route.
    fn route_reserves(&mut self, flows: &[HopFlow]) {
        for flow in flows {
            let (token0, token1) = self.tokens_of(flow.pool_id);
            let (token_in, token_out) = if flow.zero_for_one {
                (token0, token1)
            } else {
                (token1, token0)
            };
            self.reserve_in(Flow::Swap, flow.pool_id, &token_in, to_u128(flow.paid));
            self.reserve_out(Flow::Swap, flow.pool_id, &token_out, to_u128(flow.received));
        }
    }

    /// Writes the working copies of a successful route back to the pool storage.
    fn write_back(&mut self, pools: BTreeMap<u64, ClPool>) {
        for (id, pool) in pools {
            self.pools.insert(id, pool);
            self.touch_pool(id);
        }
    }

    // ---- Internal operations. Each takes the `Owner` it acts for, so the NEAR methods and
    // `execute` share one code path.

    fn swap_for(
        &mut self,
        owner: Owner,
        pool_id: u64,
        zero_for_one: bool,
        amount: I256,
        limit: U256,
    ) -> [String; 2] {
        let (token0, token1) = self.tokens_of(pool_id);

        let tick_spacing = self.pool_ref(pool_id).tick_spacing;
        let fee = self.fee_config.swap_fee(pool_id, tick_spacing, &owner);
        // No gauges in v0, so the unstaked fee is computed as if the gauge is not alive.
        let unstaked_fee = self.fee_config.unstaked_fee(pool_id, false);

        let result = {
            let pool = self.pool_mut(pool_id);
            ok(pool.swap(zero_for_one, amount, limit, fee, unstaked_fee, now()))
        };
        self.sync_pool(
            pool_id,
            1,
            to_u128(result.amount0.unsigned_abs()),
            to_u128(result.amount1.unsigned_abs()),
        );
        self.settle(&owner, &token0, result.amount0);
        self.settle(&owner, &token1, result.amount1);
        self.reserve_flow(Flow::Swap, pool_id, &token0, result.amount0);
        self.reserve_flow(Flow::Swap, pool_id, &token1, result.amount1);
        [result.amount0.to_string(), result.amount1.to_string()]
    }

    fn swap_exact_input_for(
        &mut self,
        owner: Owner,
        path: Vec<HopInput>,
        amount_in: u128,
        min_out: u128,
    ) -> u128 {
        let route = self.resolve_path(&path);
        let mut pools = self.working_pools(&route.hops);
        let flows = ok(self.route_exact_input(&route, &mut pools, owner, amount_in, min_out));
        let out = to_u128(flows.last().map(|f| f.received).unwrap_or_default());
        self.debit(&owner, &route.token_in, amount_in);
        self.credit(&owner, &route.token_out, out);
        self.route_reserves(&flows);
        self.write_back(pools);
        self.count_swaps(&flows);
        out
    }

    fn swap_exact_output_for(
        &mut self,
        owner: Owner,
        path: Vec<HopInput>,
        amount_out: u128,
        max_in: u128,
    ) -> u128 {
        let route = self.resolve_path(&path);
        let mut pools = self.working_pools(&route.hops);
        let flows = ok(self.route_exact_output(&route, &mut pools, owner, amount_out, max_in));
        let paid = to_u128(flows.first().map(|f| f.paid).unwrap_or_default());
        self.debit(&owner, &route.token_in, paid);
        self.credit(&owner, &route.token_out, amount_out);
        self.route_reserves(&flows);
        self.write_back(pools);
        self.count_swaps(&flows);
        paid
    }

    fn mint_for(
        &mut self,
        owner: Owner,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: u128,
    ) -> [String; 2] {
        let (token0, token1) = self.tokens_of(pool_id);
        self.has_state.insert(owner);
        let (amount0, amount1) = {
            let pool = self.pool_mut(pool_id);
            ok(pool.mint(owner, tick_lower, tick_upper, liquidity, now()))
        };
        self.touch_pool(pool_id);
        self.debit(&owner, &token0, to_u128(amount0));
        self.debit(&owner, &token1, to_u128(amount1));
        self.reserve_in(Flow::Mint, pool_id, &token0, to_u128(amount0));
        self.reserve_in(Flow::Mint, pool_id, &token1, to_u128(amount1));
        self.sync_position_index(owner, pool_id, tick_lower, tick_upper);
        [amount0.to_string(), amount1.to_string()]
    }

    /// Moves liquidity into owed amounts inside the pool. No token leaves the pool, so no
    /// reserve changes here (see `accounting`).
    fn burn_for(
        &mut self,
        owner: Owner,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: u128,
    ) -> [String; 2] {
        self.tokens_of(pool_id);
        let (amount0, amount1) = {
            let pool = self.pool_mut(pool_id);
            ok(pool.burn(owner, tick_lower, tick_upper, liquidity, now()))
        };
        self.touch_pool(pool_id);
        self.sync_position_index(owner, pool_id, tick_lower, tick_upper);
        [amount0.to_string(), amount1.to_string()]
    }

    fn collect_for(
        &mut self,
        owner: Owner,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
    ) -> [U128; 2] {
        let (token0, token1) = self.tokens_of(pool_id);
        let (amount0, amount1) = {
            let pool = self.pool_mut(pool_id);
            ok(pool.collect(owner, tick_lower, tick_upper, u128::MAX, u128::MAX))
        };
        self.touch_pool(pool_id);
        self.credit(&owner, &token0, amount0);
        self.credit(&owner, &token1, amount1);
        self.reserve_out(Flow::Collect, pool_id, &token0, amount0);
        self.reserve_out(Flow::Collect, pool_id, &token1, amount1);
        self.sync_position_index(owner, pool_id, tick_lower, tick_upper);
        [U128(amount0), U128(amount1)]
    }

    /// Debits `owner` and records a pending withdrawal of the EVM asset `token_id`. Returns
    /// its id.
    fn withdraw_evm_for(
        &mut self,
        owner: Owner,
        token_id: &str,
        amount: u128,
        to_address: &str,
    ) -> u64 {
        let asset = self.evm_asset_of(token_id).unwrap_or_else(|| {
            env::panic_str(&format!("{token_id} is not a registered EVM asset"))
        });
        if amount == 0 {
            env::panic_str("amount must be positive");
        }
        let to_address = evm::parse_address(to_address).unwrap_or_else(|e| env::panic_str(&e));

        self.debit(&owner, token_id, amount);
        self.pending_add(token_id, amount);
        let id = self.next_withdrawal_id;
        self.next_withdrawal_id = id
            .checked_add(1)
            .unwrap_or_else(|| env::panic_str("withdrawal id overflow"));
        self.withdrawals.insert(
            id,
            Withdrawal {
                owner,
                token_id: token_id.to_string(),
                amount,
                to_chain_id: asset.chain_id,
                to_address,
                status: WithdrawalStatus::Pending,
                signing_nonce: None,
            },
        );
        self.index_withdrawal(owner, id);
        env::log_str(&format!(
            "withdrawal {id} recorded: {amount} {token_id} to chain {}",
            asset.chain_id
        ));
        id
    }

    /// Runs a verified signed action for `owner`.
    fn dispatch(&mut self, owner: Owner, action: SignedAction) -> ActionResult {
        match action {
            SignedAction::Swap {
                pool_id,
                zero_for_one,
                amount_specified,
                sqrt_price_limit_x96,
            } => {
                let amount = parse_i256(&amount_specified);
                let limit = parse_u256(&sqrt_price_limit_x96);
                ActionResult::Swap(self.swap_for(owner, pool_id, zero_for_one, amount, limit))
            }
            SignedAction::SwapExactInput {
                path,
                amount_in,
                min_amount_out,
            } => ActionResult::SwapExactInput(U128(self.swap_exact_input_for(
                owner,
                path,
                amount_in.0,
                min_amount_out.0,
            ))),
            SignedAction::MintPosition {
                pool_id,
                tick_lower,
                tick_upper,
                liquidity,
            } => ActionResult::MintPosition(self.mint_for(
                owner,
                pool_id,
                tick_lower,
                tick_upper,
                liquidity.0,
            )),
            SignedAction::BurnPosition {
                pool_id,
                tick_lower,
                tick_upper,
                liquidity,
            } => ActionResult::BurnPosition(self.burn_for(
                owner,
                pool_id,
                tick_lower,
                tick_upper,
                liquidity.0,
            )),
            SignedAction::Collect {
                pool_id,
                tick_lower,
                tick_upper,
            } => ActionResult::Collect(self.collect_for(owner, pool_id, tick_lower, tick_upper)),
            SignedAction::WithdrawEvm {
                token,
                amount,
                to_chain_id,
                to_address,
            } => {
                let asset = self.evm_asset_of(&token).unwrap_or_else(|| {
                    env::panic_str(&format!("{token} is not a registered EVM asset"))
                });
                if asset.chain_id != to_chain_id {
                    env::panic_str(&format!(
                        "{token} is on chain {}, not {to_chain_id}",
                        asset.chain_id
                    ));
                }
                ActionResult::WithdrawEvm(self.withdraw_evm_for(
                    owner,
                    &token,
                    amount.0,
                    &to_address,
                ))
            }
            SignedAction::CreateLock {
                amount,
                duration,
                permanent,
            } => ActionResult::TokenId(self.lock_create_with(owner, amount.0, duration, permanent)),
            SignedAction::IncreaseAmount { token_id, amount } => {
                self.lock_increase_amount(owner, token_id, amount.0);
                ActionResult::Done
            }
            SignedAction::IncreaseUnlockTime { token_id, duration } => {
                self.lock_increase_unlock_time(owner, token_id, duration);
                ActionResult::Done
            }
            SignedAction::WithdrawLock { token_id } => {
                ActionResult::Amount(U128(self.lock_withdraw(owner, token_id)))
            }
            SignedAction::Merge { from, to } => {
                self.lock_merge(owner, from, to);
                ActionResult::Done
            }
            SignedAction::Split { token_id, amount } => {
                ActionResult::Split(self.lock_split(owner, token_id, amount.0))
            }
            SignedAction::LockPermanent { token_id } => {
                self.lock_set_permanent(owner, token_id, true);
                ActionResult::Done
            }
            SignedAction::UnlockPermanent { token_id } => {
                self.lock_set_permanent(owner, token_id, false);
                ActionResult::Done
            }
            SignedAction::Delegate {
                delegator,
                delegatee,
            } => {
                self.lock_delegate(owner, delegator, delegatee);
                ActionResult::Done
            }
            SignedAction::Vote {
                token_id,
                pools,
                weights,
            } => {
                let weights: Vec<U256> = weights.iter().map(|w| U256::from(w.0)).collect();
                self.vote_for(owner, token_id, &pools, &weights);
                ActionResult::Done
            }
            SignedAction::Reset { token_id } => {
                self.reset_for(owner, token_id);
                ActionResult::Done
            }
            SignedAction::Poke { token_id } => {
                self.poke_token(token_id);
                ActionResult::Done
            }
            SignedAction::StakePosition {
                pool_id,
                tick_lower,
                tick_upper,
            } => {
                self.stake_for(owner, pool_id, tick_lower, tick_upper);
                ActionResult::Done
            }
            SignedAction::UnstakePosition {
                pool_id,
                tick_lower,
                tick_upper,
            } => {
                self.unstake_for(owner, pool_id, tick_lower, tick_upper);
                ActionResult::Done
            }
            SignedAction::ClaimGaugeRewards {
                pool_id,
                tick_lower,
                tick_upper,
            } => ActionResult::Amount(U128(
                self.claim_gauge_for(owner, pool_id, tick_lower, tick_upper),
            )),
            SignedAction::ClaimFees { token_id, gauges } => ActionResult::Paid(
                self.claim_fees_for(owner, token_id, &gauges)
                    .into_iter()
                    .map(|(t, a)| (t, U128(a)))
                    .collect(),
            ),
            SignedAction::ClaimBribes {
                token_id,
                gauges,
                tokens,
            } => ActionResult::Paid(
                self.claim_bribes_for(owner, token_id, &gauges, &tokens)
                    .into_iter()
                    .map(|(t, a)| (t, U128(a)))
                    .collect(),
            ),
            SignedAction::Bribe {
                pool_id,
                token,
                amount,
            } => {
                self.bribe_for(owner, pool_id, &token, amount.0);
                ActionResult::Done
            }
            SignedAction::ClaimRebase { token_id } => {
                ActionResult::Amount(U128(self.claim_rebase_for(token_id)))
            }
            // `execute` runs `distribute` itself, since it may return a promise.
            SignedAction::Distribute => env::panic_str("distribute is run by execute"),
        }
    }
}

#[near]
impl Contract {
    #[init]
    pub fn new(owner: AccountId) -> Self {
        let (ve, voter, minter, distributor) =
            tokenomics::fresh_state(&owner, now_secs(), env::block_height());
        let nft_manager = PositionManager::new(tokenomics::nft_manager_account());
        Self {
            owner,
            fee_config: FeeConfig::default(),
            pools: IterableMap::new(StorageKey::Pools),
            pool_tokens: LookupMap::new(StorageKey::PoolTokens),
            next_pool_id: 0,
            legacy_balances: LookupMap::new(StorageKey::Balances),
            balances: LookupMap::new(StorageKey::OwnerBalances),
            allowed_tokens: IterableSet::new(StorageKey::AllowedTokens),
            mpc_contract: default_mpc_contract(),
            links: LinkRegistry::new("links"),
            action_nonces: LookupSet::new(StorageKey::ActionNonces),
            withdrawals: LookupMap::new(StorageKey::Withdrawals),
            next_withdrawal_id: 0,
            relayers: IterableSet::new(StorageKey::Relayers),
            evm_assets: IterableMap::new(StorageKey::EvmAssets),
            evm_asset_by_token: LookupMap::new(StorageKey::EvmAssetByToken),
            evm_chains: LookupMap::new(StorageKey::EvmChains),
            evm_deposits: LookupSet::new(StorageKey::EvmDeposits),
            evm_deposits_by_asset: LookupSet::new(StorageKey::EvmDepositsByAsset),
            has_state: LookupSet::new(StorageKey::HasState),
            wind_token: None,
            ve: LazyOption::new(StorageKey::Ve, Some(ve)),
            voter: LazyOption::new(StorageKey::Voter, Some(voter)),
            minter: LazyOption::new(StorageKey::Minter, Some(minter)),
            distributor: LazyOption::new(StorageKey::Distributor, Some(distributor)),
            gauges: LookupMap::new(StorageKey::Gauges),
            stakes: LookupMap::new(StorageKey::Stakes),
            nft_manager: LazyOption::new(StorageKey::NftManager, Some(nft_manager)),
            wind_genesis_done: false,
            wind_genesis_pending: false,
            wind_minted: 0,
            pending_epoch: None,
            liabilities: LookupMap::new(StorageKey::Liabilities),
            accounted: LookupSet::new(StorageKey::Accounted),
            needs_seed: false,
            pool_reserves: LookupMap::new(StorageKey::PoolReserves),
            reserve_totals: LookupMap::new(StorageKey::ReserveTotals),
            reserves_seeded: LookupSet::new(StorageKey::ReservesSeeded),
            pending_withdrawals: LookupMap::new(StorageKey::PendingOwed),
            unbacked: LookupMap::new(StorageKey::Unbacked),
            positions_by_owner: LookupMap::new(StorageKey::PositionsByOwner),
            stakes_by_owner: LookupMap::new(StorageKey::StakesByOwner),
            withdrawals_by_owner: LookupMap::new(StorageKey::WithdrawalsByOwner),
            balance_tokens_by_owner: LookupMap::new(StorageKey::BalanceTokensByOwner),
            intents: LookupMap::new(StorageKey::Intents),
            next_intent_id: 0,
            intents_by_owner: LookupMap::new(StorageKey::IntentsByOwner),
            open_intents: LookupMap::new(StorageKey::OpenIntents),
            permissionless: permissionless::Permissionless::new(),
        }
    }

    /// Upgrade from the layout on `main` (v8: one-step flows and intents) to the permissionless
    /// layout. Run once, as the init call of the redeploy:
    /// `near contract deploy <acct> use-file <wasm> with-init-call migrate json-args '{}'`.
    ///
    /// Every field is kept as it is. The pool index and the pool summaries are backfilled from the
    /// existing pools, and the default caps start unset. Only the contract account can call it
    /// (`#[private]`). See `permissionless::migrate_from_v8`.
    #[private]
    #[init(ignore_state)]
    pub fn migrate() -> Self {
        permissionless::migrate_from_v8()
    }

    /// Owner only. Moves the v3 balances of the listed `(account, token)` pairs to their owner
    /// keys. Entries with no legacy balance are skipped, so calling this again is safe. Returns
    /// the number of entries moved.
    ///
    /// The list is supplied by the owner because the old map cannot be iterated. Check
    /// `get_legacy_balance` for each entry to see what is still unmigrated.
    pub fn migrate_balances(&mut self, entries: Vec<(AccountId, String)>) -> u64 {
        self.assert_owner();
        let mut moved = 0;
        for (account, token) in entries {
            if let Some(amount) = self
                .legacy_balances
                .remove(&(account.clone(), token.clone()))
            {
                self.credit(&owner_of_account(&account), &token, amount);
                moved += 1;
            }
        }
        moved
    }

    /// A balance still held under the v3 key of `account_id`, before `migrate_balances`.
    pub fn get_legacy_balance(&self, account_id: AccountId, token: String) -> U128 {
        U128(
            self.legacy_balances
                .get(&(account_id, token))
                .copied()
                .unwrap_or(0),
        )
    }

    /// Owner only. Sets the Chain Signatures MPC contract used by `request_evm_payout`.
    pub fn set_mpc_contract(&mut self, mpc_contract: AccountId) {
        self.assert_owner();
        self.mpc_contract = mpc_contract;
    }

    /// The Chain Signatures MPC contract used by `request_evm_payout`.
    pub fn get_mpc_contract(&self) -> AccountId {
        self.mpc_contract.clone()
    }

    /// Owner only. Asks the MPC network to sign an EIP-1559 transfer from this contract's
    /// derived EVM address (path `path`, secp256k1, domain 0), and returns the signed raw
    /// transaction `0x02 || rlp(...)` as a 0x-hex string.
    ///
    /// The caller must attach exactly 1 yoctoNEAR, as for `withdraw`. The `sign` call attaches
    /// 1 yoctoNEAR from the contract's own balance, which the MPC contract requires. The caller
    /// must attach at least 100 Tgas, of which 50 go to `sign` and 20 to the callback.
    ///
    /// The signing key is derived from `(path, predecessor = this contract)`. The EVM address
    /// is `keccak256(pubkey)[12..]` of `derived_public_key` on the MPC contract. Nothing in this
    /// contract checks that the address holds funds or that `nonce` is the next one.
    // The eight arguments are the request fields of the spec, so they stay separate.
    #[allow(clippy::too_many_arguments)]
    #[payable]
    pub fn request_evm_payout(
        &mut self,
        chain_id: u64,
        nonce: u64,
        to: String,
        value_wei: U128,
        max_fee_per_gas: U128,
        max_priority_fee_per_gas: U128,
        gas_limit: u64,
        path: String,
    ) -> Promise {
        self.assert_owner();
        assert_one_yocto();
        assert_payout_gas();
        if path.is_empty() {
            env::panic_str("path must not be empty");
        }
        let tx = EvmTx {
            chain_id,
            nonce,
            to,
            value_wei,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            gas_limit,
            data: String::new(),
        };
        let transfer = tx.transfer().unwrap_or_else(|e| env::panic_str(&e));
        let digest = transfer.signing_hash();

        let request = SignRequest {
            path,
            payload_v2: Payload::Ecdsa(evm::hex_plain(&digest)),
            domain_id: 0,
        };
        ext_mpc::ext(self.mpc_contract.clone())
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(Gas::from_tgas(SIGN_CALL_GAS_TGAS))
            .sign(request)
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(CALLBACK_GAS_TGAS))
                    .on_evm_signed(tx),
            )
    }

    /// Callback for `request_evm_payout`. Builds the signed raw transaction from the MPC
    /// response and returns it as 0x-hex. Panics if the MPC call failed or the response is
    /// not a valid secp256k1 signature.
    #[private]
    pub fn on_evm_signed(
        &self,
        tx: EvmTx,
        #[callback_result] sig: Result<MpcSignature, PromiseError>,
    ) -> String {
        let sig = sig.unwrap_or_else(|e| env::panic_str(&format!("MPC sign failed: {e:?}")));
        signed_raw_tx(&tx, &sig).unwrap_or_else(|e| env::panic_str(&e))
    }

    /// Owner only. Adds `token` (a NEP-141 contract account) to the deposit whitelist.
    pub fn add_token(&mut self, token: AccountId) {
        self.assert_owner();
        if !self.allowed_tokens.insert(token) {
            env::panic_str("token already allowed");
        }
    }

    /// Owner only. Removes `token` from the deposit whitelist. Existing balances of the token
    /// can still be withdrawn. New deposits of it are refunded.
    pub fn remove_token(&mut self, token: AccountId) {
        self.assert_owner();
        if !self.allowed_tokens.remove(&token) {
            env::panic_str("token not allowed");
        }
    }

    /// Tokens currently accepted for deposit.
    pub fn get_tokens(&self) -> Vec<AccountId> {
        self.allowed_tokens.iter().cloned().collect()
    }

    /// NEP-141 receiver hook, called by a token contract after `ft_transfer_call` to this
    /// account. The predecessor is the token contract and `sender_id` is the account that
    /// sent the tokens.
    ///
    /// For a whitelisted token, credits an owner with `amount` and returns 0 (nothing unused).
    /// For any other token, returns `amount` so the token contract refunds it.
    ///
    /// The owner credited is:
    /// - the owner of the wallet in `msg`, when `msg` is `{"owner":"<wallet>"}`. Anyone can
    ///   deposit for any wallet this way, and the sender gets nothing. The wallet string is the
    ///   canonical form (`evm:0x…`, `ed25519:…`, `near:…`). A wallet that does not parse panics,
    ///   which makes the token contract refund the deposit.
    /// - otherwise, `sender_id`. Any other `msg` is ignored.
    pub fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        let token = env::predecessor_account_id();
        if !self.allowed_tokens.contains(&token) {
            return PromiseOrValue::Value(amount);
        }
        // One-step flows (see `oneclick`). A plan that fails writes nothing and refunds the whole
        // transfer, so a bad swap or zap never costs the sender anything.
        if let Some(step) = oneclick::parse_one_step(&msg) {
            let outcome =
                step.and_then(|step| self.one_step(&sender_id, token.as_str(), amount.0, step));
            return match outcome {
                Ok(()) => PromiseOrValue::Value(U128(0)),
                Err(reason) => {
                    env::log_str(&format!("one-step transfer refunded: {reason}"));
                    PromiseOrValue::Value(amount)
                }
            };
        }
        let parsed = parse_deposit_msg(&msg);
        let owner = match parsed.as_ref().and_then(|p| p.owner.as_ref()) {
            Some(wallet) => self.links.resolve(&parse_wallet(wallet)),
            None => owner_of_account(&sender_id),
        };
        let lock = parsed.and_then(|p| p.lock);
        if let (Some(lock), true) = (lock, self.wind_token.as_ref() == Some(&token)) {
            // A WIND deposit with a lock message becomes a veNFT of the owner. The lock pulls the
            // deposit from the owner's balance, so the amount must be credited first.
            self.credit(&owner, token.as_str(), amount.0);
            self.lock_create(owner, amount.0, lock.duration);
            return PromiseOrValue::Value(U128(0));
        }
        self.credit(&owner, token.as_str(), amount.0);
        PromiseOrValue::Value(U128(0))
    }

    /// Withdraws `amount` of `token` from the caller's internal balance to the caller's
    /// account on the token contract. Requires exactly 1 yoctoNEAR attached.
    ///
    /// The balance is debited before the transfer. `on_withdraw` credits it back if the
    /// transfer fails. Withdrawals do not check the whitelist, so a delisted token's balance
    /// can still be withdrawn.
    #[payable]
    pub fn withdraw(&mut self, token: AccountId, amount: U128) -> Promise {
        assert_one_yocto();
        if amount.0 == 0 {
            env::panic_str("amount must be positive");
        }
        let account = env::predecessor_account_id();
        self.debit(&owner_of_account(&account), token.as_str(), amount.0);
        ext_ft::ext(token.clone())
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(Gas::from_tgas(5))
            .ft_transfer(account.clone(), amount, None)
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(10))
                    .on_withdraw(account, token, amount),
            )
    }

    /// Callback for `withdraw`. Re-credits the balance if the `ft_transfer` failed.
    #[private]
    pub fn on_withdraw(&mut self, account: AccountId, token: AccountId, amount: U128) {
        // `ft_transfer` returns nothing, so the only error here is a failed promise.
        if env::promise_result_checked(0, usize::MAX).is_err() {
            self.credit(&owner_of_account(&account), token.as_str(), amount.0);
        }
    }

    /// Testnet-only faucet: credits an internal balance. Owner only. Compiled only with the
    /// `testnet-faucet` feature (on by default). Mainnet builds must use `--no-default-features`.
    #[cfg(feature = "testnet-faucet")]
    pub fn test_credit(&mut self, account_id: AccountId, token: String, amount: U128) {
        self.assert_owner();
        self.credit(&owner_of_account(&account_id), &token, amount.0);
        self.unbacked_add(&token, amount.0);
    }

    /// Adds liquidity for the caller. Debits the caller's internal balances for the token
    /// amounts the position requires. Returns `[amount0, amount1]`.
    pub fn mint_position(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: U128,
    ) -> [String; 2] {
        let caller = owner_of_account(&env::predecessor_account_id());
        self.mint_for(caller, pool_id, tick_lower, tick_upper, liquidity.0)
    }

    /// Removes liquidity from the caller's position. The withdrawn amounts become owed to the
    /// position and are paid out by `collect`. Returns `[amount0, amount1]`.
    pub fn burn_position(
        &mut self,
        pool_id: u64,
        tick_lower: i32,
        tick_upper: i32,
        liquidity: U128,
    ) -> [String; 2] {
        let caller = owner_of_account(&env::predecessor_account_id());
        self.burn_for(caller, pool_id, tick_lower, tick_upper, liquidity.0)
    }

    /// Collects everything owed to the caller's position and credits the caller's internal
    /// balances. Returns `[amount0, amount1]`.
    pub fn collect(&mut self, pool_id: u64, tick_lower: i32, tick_upper: i32) -> [U128; 2] {
        let caller = owner_of_account(&env::predecessor_account_id());
        self.collect_for(caller, pool_id, tick_lower, tick_upper)
    }

    /// Swaps for the caller. `amount_specified > 0` is exact input, `< 0` is exact output.
    /// Debits the input token and credits the output token. Returns the signed pool deltas
    /// `[amount0, amount1]` (positive = paid by the caller, negative = paid to the caller).
    pub fn swap(
        &mut self,
        pool_id: u64,
        zero_for_one: bool,
        amount_specified: String,
        sqrt_price_limit_x96: String,
    ) -> [String; 2] {
        let caller = owner_of_account(&env::predecessor_account_id());
        let amount = parse_i256(&amount_specified);
        let limit = parse_u256(&sqrt_price_limit_x96);
        self.swap_for(caller, pool_id, zero_for_one, amount, limit)
    }

    /// Pool state (no secrets, all numbers as strings where they exceed JS integer range).
    pub fn get_pool(&self, pool_id: u64) -> PoolView {
        let (token0, token1) = self.tokens_of(pool_id);
        let pool = self.pool_ref(pool_id);
        PoolView {
            pool_id,
            token0,
            token1,
            initialized: pool.initialized,
            tick_spacing: pool.tick_spacing,
            slot0: Slot0View {
                sqrt_price_x96: pool.slot0.sqrt_price_x96.to_string(),
                tick: pool.slot0.tick,
                observation_index: pool.slot0.observation_index,
                observation_cardinality: pool.slot0.observation_cardinality,
                observation_cardinality_next: pool.slot0.observation_cardinality_next,
            },
            liquidity: U128(pool.liquidity),
            staked_liquidity: U128(pool.staked_liquidity),
            fee_growth_global0_x128: pool.fee_growth_global0_x128.to_string(),
            fee_growth_global1_x128: pool.fee_growth_global1_x128.to_string(),
        }
    }

    /// Position of `account_id` in `[tick_lower, tick_upper)`.
    pub fn get_position(
        &self,
        pool_id: u64,
        account_id: AccountId,
        tick_lower: i32,
        tick_upper: i32,
    ) -> PositionView {
        let position =
            self.pool_ref(pool_id)
                .position(&owner_of_account(&account_id), tick_lower, tick_upper);
        PositionView {
            liquidity: U128(position.liquidity),
            fee_growth_inside0_last_x128: position.fee_growth_inside0_last_x128.to_string(),
            fee_growth_inside1_last_x128: position.fee_growth_inside1_last_x128.to_string(),
            tokens_owed0: U128(position.tokens_owed0),
            tokens_owed1: U128(position.tokens_owed1),
        }
    }

    /// Multi-hop exact-input swap for the caller. Spends exactly `amount_in` of the first hop's
    /// input token and credits the caller with the output of the last hop. Panics if that
    /// output is below `min_amount_out`, or if the caller's balance is too small. Returns the
    /// output amount.
    pub fn swap_exact_input(
        &mut self,
        path: Vec<HopInput>,
        amount_in: U128,
        min_amount_out: U128,
    ) -> U128 {
        let caller = owner_of_account(&env::predecessor_account_id());
        U128(self.swap_exact_input_for(caller, path, amount_in.0, min_amount_out.0))
    }

    /// Multi-hop exact-output swap for the caller. Credits exactly `amount_out` of the last
    /// hop's output token and debits the caller for the input the route needs. Panics if that
    /// input is above `max_amount_in`, or if the caller's balance is too small. Returns the
    /// input amount actually debited.
    pub fn swap_exact_output(
        &mut self,
        path: Vec<HopInput>,
        amount_out: U128,
        max_amount_in: U128,
    ) -> U128 {
        let caller = owner_of_account(&env::predecessor_account_id());
        U128(self.swap_exact_output_for(caller, path, amount_out.0, max_amount_in.0))
    }

    /// Output of `swap_exact_input` for `path` and `amount_in`, at the current block time.
    /// Uses the fees of `QUOTE_TRADER` (see that constant). Changes no state.
    pub fn quote_exact_input(&self, path: Vec<HopInput>, amount_in: U128) -> U128 {
        let route = self.resolve_path(&path);
        let mut pools = self.working_pools(&route.hops);
        let flows = ok(self.route_exact_input(&route, &mut pools, QUOTE_TRADER, amount_in.0, 0));
        U128(to_u128(
            flows.last().map(|f| f.received).unwrap_or_default(),
        ))
    }

    /// Input that `swap_exact_output` would debit for `path` and `amount_out`, at the current
    /// block time. Uses the fees of `QUOTE_TRADER`. Changes no state.
    pub fn quote_exact_output(&self, path: Vec<HopInput>, amount_out: U128) -> U128 {
        let route = self.resolve_path(&path);
        let mut pools = self.working_pools(&route.hops);
        let flows =
            ok(self.route_exact_output(&route, &mut pools, QUOTE_TRADER, amount_out.0, u128::MAX));
        U128(to_u128(flows.first().map(|f| f.paid).unwrap_or_default()))
    }

    /// Owner only. Enables a tick spacing with its swap fee, in hundredths of a bip. Pools
    /// can then be created with that spacing.
    pub fn enable_tick_spacing(&mut self, tick_spacing: i32, fee: u32) {
        self.assert_owner();
        ok(self.fee_config.enable_tick_spacing(tick_spacing, fee));
        for id in 0..self.next_pool_id {
            if self.pool_ref(id).tick_spacing == tick_spacing {
                self.touch_pool(id);
            }
        }
    }

    /// Owner only. Sets the swap fee of one pool, in hundredths of a bip. `0` clears it. The
    /// value `420` means a fee of 0%.
    pub fn set_custom_swap_fee(&mut self, pool_id: u64, fee: u32) {
        self.assert_owner();
        self.pool_ref(pool_id);
        ok(self.fee_config.set_custom_swap_fee(pool_id, fee));
        self.touch_pool(pool_id);
    }

    /// Owner only. Sets the unstaked fee of one pool, in hundredths of a bip. `0` clears it.
    /// The value `420` means a fee of 0%. No gauges exist yet, so this does not affect swaps.
    pub fn set_custom_unstaked_fee(&mut self, pool_id: u64, fee: u32) {
        self.assert_owner();
        self.pool_ref(pool_id);
        ok(self.fee_config.set_custom_unstaked_fee(pool_id, fee));
    }

    /// Owner only. Adds or removes `account_id` from the swap fee whitelist. A whitelisted
    /// account pays a swap fee of 0 on every pool.
    pub fn set_fee_whitelist(&mut self, account_id: AccountId, whitelisted: bool) {
        self.assert_owner();
        self.fee_config
            .set_whitelist(owner_of_account(&account_id), whitelisted);
    }

    /// Swap fee that `account_id` pays on `pool_id`, in hundredths of a bip.
    pub fn get_fee(&self, pool_id: u64, account_id: AccountId) -> u32 {
        let tick_spacing = self.pool_ref(pool_id).tick_spacing;
        self.fee_config
            .swap_fee(pool_id, tick_spacing, &owner_of_account(&account_id))
    }

    /// Internal balance of `account_id` for `token`. Covers the NEAR account's own owner id; a
    /// linked wallet's balance is under the account's id (see `get_wallets`).
    pub fn get_balance(&self, account_id: AccountId, token: String) -> U128 {
        U128(self.balance_of(&owner_of_account(&account_id), &token))
    }

    /// Internal balance of an owner id (64 hex digits, see `resolve_wallet`) for `token`.
    pub fn get_balance_of_owner(&self, owner: String, token: String) -> U128 {
        let owner = parse_owner_hex(&owner)
            .unwrap_or_else(|| env::panic_str("owner must be 64 hex digits"));
        U128(self.balance_of(&owner, &token))
    }

    /// Links `wallet` (an `evm:` or `ed25519:` wallet string) to the caller's account.
    ///
    /// `signature` is the wallet's signature over the link message, built with the caller's
    /// account and this contract's account id (see `accounts.rs`). EVM: 65 bytes of hex.
    /// Ed25519: 64 bytes, as hex or base58. The signature must be fresh: `nonce` is unused for
    /// this wallet and `expires` (unix seconds) has not passed.
    pub fn link_wallet(&mut self, wallet: String, nonce: u64, expires: u64, signature: String) {
        let wallet = parse_wallet(&wallet);
        if self.has_state.contains(&engine_id_for(&wallet)) {
            env::panic_str(
                "wallet already holds balances or positions under its own id and cannot be linked",
            );
        }
        let sig = ok(parse_signature(&wallet, &signature));
        let req = LinkRequest {
            wallet,
            account: env::predecessor_account_id(),
            contract: env::current_account_id(),
            nonce,
            expires,
        };
        ok(self.links.link(&req, sig.as_wallet_signature(), now_secs()));
    }

    /// Unlinks `wallet` from the caller's account. Panics if the wallet is not linked to it.
    /// Balances already held under the account stay with the account.
    pub fn unlink_wallet(&mut self, wallet: String) {
        let wallet = parse_wallet(&wallet);
        ok(self.links.unlink(&env::predecessor_account_id(), &wallet));
    }

    /// Wallets linked to `account_id`, as canonical strings.
    pub fn get_wallets(&self, account_id: AccountId) -> Vec<String> {
        self.links
            .wallets_of(&account_id)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// The owner id (64 hex digits) that `wallet` acts for: its linked account's owner, or its
    /// own owner id if it is not linked.
    pub fn resolve_wallet(&self, wallet: String) -> String {
        owner_hex(&self.links.resolve(&parse_wallet(&wallet)))
    }

    /// Runs a signed action for the wallet that signed it. Anyone may call this, and the caller
    /// pays the NEAR gas. Nothing is charged to the caller's balance.
    ///
    /// The wallet must sign exactly the action message in `accounts.rs`:
    /// `Wind Swap v4 action on {this contract} by {wallet} nonce {nonce} expires {expires}: {action}`,
    /// where `{action}` is the canonical JSON of `action`. `nonce` must be unused for this wallet
    /// (any unused value works, so parallel actions are possible). `expires` is unix seconds and
    /// must not have passed. The wallet resolves to its owner (linked account, or its own id).
    /// A NEAR account cannot use this path: it calls the methods directly.
    pub fn execute(
        &mut self,
        wallet: String,
        action: SignedAction,
        nonce: u64,
        expires: u64,
        signature: String,
    ) -> PromiseOrValue<ActionResult> {
        let wallet = parse_wallet(&wallet);
        if now_secs() > expires {
            env::panic_str("action has expired");
        }
        let action_json = ok(near_sdk::serde_json::to_string(&action));
        let msg = action_message(
            &env::current_account_id(),
            &wallet,
            nonce,
            expires,
            &action_json,
        );
        let sig = ok(parse_signature(&wallet, &signature));
        if !verify_wallet_signature(&msg, &wallet, sig.as_wallet_signature()) {
            env::panic_str("wallet signature does not verify");
        }
        if !self.action_nonces.insert((wallet.clone(), nonce)) {
            env::panic_str("action nonce was already used for this wallet");
        }
        let owner = self.links.resolve(&wallet);
        if matches!(action, SignedAction::Distribute) {
            // `distribute` may return a promise (the epoch's mint), which `execute` passes on.
            return match self.distribute_epoch() {
                PromiseOrValue::Promise(promise) => PromiseOrValue::Promise(promise),
                PromiseOrValue::Value(amount) => {
                    PromiseOrValue::Value(ActionResult::Amount(amount))
                }
            };
        }
        PromiseOrValue::Value(self.dispatch(owner, action))
    }

    /// Withdraws `amount` of the EVM asset `token_id` to `to_address` (0x) on the asset's chain.
    /// Debits the caller's account now. The withdrawal stays `Pending` until a relayer processes
    /// it. Returns its id. Signed withdrawals use `execute` with `WithdrawEvm`.
    pub fn withdraw_evm(&mut self, token_id: String, amount: U128, to_address: String) -> u64 {
        let owner = owner_of_account(&env::predecessor_account_id());
        self.withdraw_evm_for(owner, &token_id, amount.0, &to_address)
    }

    /// Owner only. Adds `account_id` to the relayers.
    pub fn add_relayer(&mut self, account_id: AccountId) {
        self.assert_owner();
        if !self.relayers.insert(account_id) {
            env::panic_str("already a relayer");
        }
    }

    /// Owner only. Removes `account_id` from the relayers.
    pub fn remove_relayer(&mut self, account_id: AccountId) {
        self.assert_owner();
        if !self.relayers.remove(&account_id) {
            env::panic_str("not a relayer");
        }
    }

    /// The relayers.
    pub fn get_relayers(&self) -> Vec<AccountId> {
        self.relayers.iter().cloned().collect()
    }

    /// Owner only. Marks the owner id of each wallet as holding state, so `link_wallet` refuses
    /// it. Use this once after `migrate` for wallets that held balances under their own id before
    /// v5, because the migration cannot find them. Only flag wallets whose own id holds balances
    /// or positions, since a flagged wallet can never be linked. Returns how many were newly
    /// flagged.
    pub fn flag_wallet_state(&mut self, wallets: Vec<String>) -> u64 {
        self.assert_owner();
        let mut flagged = 0;
        for wallet in wallets {
            if self.has_state.insert(engine_id_for(&parse_wallet(&wallet))) {
                flagged += 1;
            }
        }
        flagged
    }

    /// Owner only. Registers an EVM asset under the internal id `token_id` (for example
    /// `base-sepolia:eth`). `asset` is `native` or an ERC-20 address. Calling it again with the
    /// same `(chain_id, asset, token_id)` updates `decimals` and the caps, and keeps the current
    /// withdrawal window. A `token_id` that is a NEAR account id is refused, and so is one already
    /// used by a different asset.
    pub fn register_evm_asset(
        &mut self,
        chain_id: u64,
        asset: String,
        token_id: String,
        decimals: u8,
        per_deposit_cap: U128,
        daily_withdraw_cap: U128,
    ) {
        self.assert_owner();
        if chain_id == 0 {
            env::panic_str("chain_id must be positive");
        }
        let asset = normalize_asset(&asset).unwrap_or_else(|e| env::panic_str(&e));
        if token_id.is_empty() || token_id.parse::<AccountId>().is_ok() {
            env::panic_str("token_id must be an internal id, not a NEAR account id");
        }
        let key = evm_asset_key(chain_id, &asset);
        if let Some(other) = self.evm_asset_by_token.get(token_id.as_str()) {
            if *other != key {
                env::panic_str(&format!("token id {token_id} is used by another asset"));
            }
        }
        let (window_day, window_withdrawn) = match self.evm_assets.get(&key) {
            Some(existing) => {
                if existing.token_id != token_id {
                    env::panic_str(&format!(
                        "asset is already registered under token id {}",
                        existing.token_id
                    ));
                }
                (existing.window_day, existing.window_withdrawn)
            }
            None => (0, 0),
        };
        self.evm_assets.insert(
            key.clone(),
            EvmAsset {
                chain_id,
                asset,
                token_id: token_id.clone(),
                decimals,
                per_deposit_cap: per_deposit_cap.0,
                daily_withdraw_cap: daily_withdraw_cap.0,
                window_day,
                window_withdrawn,
            },
        );
        self.evm_asset_by_token.insert(token_id, key);
    }

    /// A registered EVM asset.
    pub fn get_evm_asset(&self, chain_id: u64, asset: String) -> Option<EvmAssetView> {
        let asset = normalize_asset(&asset).ok()?;
        self.evm_assets
            .get(&evm_asset_key(chain_id, &asset))
            .map(|a| self.evm_asset_view_of(a))
    }

    /// All registered EVM assets.
    pub fn get_evm_assets(&self) -> Vec<EvmAssetView> {
        self.evm_assets
            .values()
            .map(|a| self.evm_asset_view_of(a))
            .collect()
    }

    /// Owner only. Sets the settings of an EVM chain: `path` (the MPC derivation path, whose
    /// address is the chain's vault), `next_nonce` (the nonce of the next withdrawal) and
    /// `max_fee_per_gas_cap` (the most `process_withdrawal` accepts). `next_nonce` can only go up:
    /// a lower value would reuse a nonce, so it panics.
    pub fn set_evm_chain(
        &mut self,
        chain_id: u64,
        path: String,
        next_nonce: u64,
        max_fee_per_gas_cap: U128,
    ) {
        self.assert_owner();
        if chain_id == 0 {
            env::panic_str("chain_id must be positive");
        }
        if path.is_empty() {
            env::panic_str("path must not be empty");
        }
        if max_fee_per_gas_cap.0 == 0 {
            env::panic_str("max_fee_per_gas_cap must be positive");
        }
        if let Some(existing) = self.evm_chains.get(&chain_id) {
            if next_nonce < existing.next_nonce {
                env::panic_str(&format!(
                    "next_nonce cannot go back from {} to {next_nonce}",
                    existing.next_nonce
                ));
            }
        }
        self.evm_chains.insert(
            chain_id,
            EvmChain {
                path,
                next_nonce,
                max_fee_per_gas_cap: max_fee_per_gas_cap.0,
            },
        );
    }

    /// The settings of an EVM chain.
    pub fn get_evm_chain(&self, chain_id: u64) -> Option<EvmChainView> {
        self.evm_chains.get(&chain_id).map(|c| EvmChainView {
            chain_id,
            path: c.path.clone(),
            next_nonce: c.next_nonce,
            max_fee_per_gas_cap: U128(c.max_fee_per_gas_cap),
        })
    }

    /// Relayer only. Credits a deposit that the relayer saw on `chain_id`: `amount` of `asset`
    /// sent by `from` in log `log_index` of transaction `tx_hash` (use 0 for a native transfer).
    ///
    /// Returns `false`, and changes nothing, if this `(chain_id, tx_hash, log_index, asset)` was
    /// already credited, or if `(chain_id, tx_hash, log_index)` was credited before the asset was
    /// part of the key. Panics if the asset is not registered, if `amount` is zero, or if it is above
    /// the asset's per-deposit cap. The credit goes to the owner that `from` resolves to: its
    /// linked account, or its own owner id if it is not linked.
    ///
    /// An unregistered ERC-20 on an enabled chain is registered by this call when `decimals`
    /// (0-36) is given, with the default caps and `verified: false`. `symbol` is informational.
    /// Without `decimals` the report returns `false` and logs why. See `permissionless`.
    // The arguments are the relayer's report fields, so they stay separate.
    #[allow(clippy::too_many_arguments)]
    pub fn report_evm_deposit(
        &mut self,
        chain_id: u64,
        tx_hash: String,
        log_index: u32,
        from: String,
        asset: String,
        amount: U128,
        decimals: Option<u8>,
        symbol: Option<String>,
    ) -> bool {
        self.assert_relayer();
        let hash = parse_tx_hash(&tx_hash).unwrap_or_else(|e| env::panic_str(&e));
        let from = evm::parse_address(&from).unwrap_or_else(|e| env::panic_str(&e));
        let asset = normalize_asset(&asset).unwrap_or_else(|e| env::panic_str(&e));
        // An unknown ERC-20 on an enabled chain is registered here (see `permissionless`).
        let Some(info) =
            self.asset_for_report(chain_id, &asset, &hash, log_index, decimals, symbol)
        else {
            return false;
        };
        // A report is a duplicate if its legacy key (no asset) or its asset key was credited.
        // A panic below reverts the insert too.
        if self.evm_deposits.contains(&(chain_id, hash, log_index))
            || !self
                .evm_deposits_by_asset
                .insert((chain_id, hash, log_index, asset.clone()))
        {
            return false;
        }
        if amount.0 == 0 {
            env::panic_str("amount must be positive");
        }
        if amount.0 > info.per_deposit_cap {
            env::panic_str(&format!(
                "deposit of {} is above the per-deposit cap of {}",
                amount.0, info.per_deposit_cap
            ));
        }
        let owner = self.links.resolve(&WalletId::Evm(from));
        self.credit(&owner, &info.token_id, amount.0);
        self.run_open_intent(owner, &info.token_id);
        env::log_str(&format!(
            "evm deposit: chain {chain_id} tx {} log {log_index}: {} {} from {}",
            evm::hex_0x(&hash),
            amount.0,
            info.token_id,
            evm::hex_0x(&from)
        ));
        true
    }

    /// Relayer only. Has the MPC network sign the transfer for withdrawal `id`. Takes 1 yocto
    /// and at least 100 Tgas.
    ///
    /// The withdrawal must be `Pending` and not already being signed. Its token must be a
    /// registered EVM asset. `max_fee_per_gas` must not exceed the chain's cap. The asset's daily
    /// withdraw cap must have room for `amount`. The window is the current UTC day.
    ///
    /// The chain's next nonce is reserved now. A native asset sends `amount` to the recipient
    /// with 21000 gas. An ERC-20 asset calls `transfer(recipient, amount)` on the token with
    /// 100000 gas. The signing path is the chain's, so the sender is the chain's vault address.
    /// The callback is `on_withdrawal_signed`.
    #[payable]
    pub fn process_withdrawal(
        &mut self,
        id: u64,
        max_fee_per_gas: U128,
        max_priority_fee_per_gas: U128,
    ) -> Promise {
        self.assert_relayer();
        assert_one_yocto();
        assert_payout_gas();

        let mut w = self
            .withdrawals
            .get(&id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(&format!("withdrawal {id} does not exist")));
        if !matches!(w.status, WithdrawalStatus::Pending) || w.signing_nonce.is_some() {
            env::panic_str(&format!("withdrawal {id} is not pending"));
        }
        let mut asset = self.evm_asset_of(&w.token_id).unwrap_or_else(|| {
            env::panic_str(&format!("{} is not a registered EVM asset", w.token_id))
        });
        let chain_id = asset.chain_id;
        let mut chain = self
            .evm_chains
            .get(&chain_id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(&format!("chain {chain_id} is not configured")));
        if max_fee_per_gas.0 > chain.max_fee_per_gas_cap {
            env::panic_str(&format!(
                "max_fee_per_gas {} is above the chain cap {}",
                max_fee_per_gas.0, chain.max_fee_per_gas_cap
            ));
        }

        let day = now_secs() / SECONDS_PER_DAY;
        if asset.window_day != day {
            asset.window_day = day;
            asset.window_withdrawn = 0;
        }
        let withdrawn = asset
            .window_withdrawn
            .checked_add(w.amount)
            .unwrap_or_else(|| env::panic_str("daily withdrawal total overflow"));
        if withdrawn > asset.daily_withdraw_cap {
            env::panic_str(&format!(
                "daily withdraw cap of {} exceeded: {} already reserved today",
                asset.daily_withdraw_cap, asset.window_withdrawn
            ));
        }
        asset.window_withdrawn = withdrawn;

        let nonce = chain.next_nonce;
        chain.next_nonce = nonce
            .checked_add(1)
            .unwrap_or_else(|| env::panic_str("nonce overflow"));

        let (to, value, data, gas_limit) = if asset.asset == "native" {
            (w.to_address, w.amount, Vec::new(), NATIVE_TRANSFER_GAS)
        } else {
            let token = evm::parse_address(&asset.asset).unwrap_or_else(|e| env::panic_str(&e));
            (
                token,
                0,
                evm::erc20_transfer_data(&w.to_address, w.amount),
                ERC20_TRANSFER_GAS,
            )
        };
        let tx = EvmTx {
            chain_id,
            nonce,
            to: evm::hex_0x(&to),
            value_wei: U128(value),
            max_fee_per_gas,
            max_priority_fee_per_gas,
            gas_limit,
            data: evm::hex_0x(&data),
        };
        let digest = tx
            .transfer()
            .unwrap_or_else(|e| env::panic_str(&e))
            .signing_hash();
        let request = SignRequest {
            path: chain.path.clone(),
            payload_v2: Payload::Ecdsa(evm::hex_plain(&digest)),
            domain_id: 0,
        };

        w.signing_nonce = Some(nonce);
        self.withdrawals.insert(id, w);
        self.evm_assets
            .insert(evm_asset_key(chain_id, &asset.asset), asset);
        self.evm_chains.insert(chain_id, chain);

        ext_mpc::ext(self.mpc_contract.clone())
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(Gas::from_tgas(SIGN_CALL_GAS_TGAS))
            .sign(request)
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(CALLBACK_GAS_TGAS))
                    .on_withdrawal_signed(id, day, tx),
            )
    }

    /// Callback for `process_withdrawal`. Returns `true` when the transaction was signed.
    ///
    /// On success the withdrawal becomes `Signed`. On failure, which includes a failed MPC call
    /// or a bad signature, it does not panic, so the undo is kept:
    /// - the daily reservation is released, if the window is still the day it was reserved on;
    /// - if the reserved nonce is still the chain's last one, the nonce is released and the
    ///   withdrawal goes back to `Pending`;
    /// - otherwise a later withdrawal already uses the next nonce. This withdrawal becomes
    ///   `Failed` and the nonce stays a gap. The relayer must fill the gap, for example with a
    ///   self-transfer. Its balance is credited back, since nothing was signed.
    #[private]
    pub fn on_withdrawal_signed(
        &mut self,
        id: u64,
        day: u64,
        tx: EvmTx,
        #[callback_result] sig: Result<MpcSignature, PromiseError>,
    ) -> bool {
        let mut w = self
            .withdrawals
            .get(&id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(&format!("withdrawal {id} does not exist")));
        w.signing_nonce = None;
        let nonce = tx.nonce;
        let signed = match sig {
            Ok(sig) => signed_raw_tx(&tx, &sig),
            Err(e) => Err(format!("MPC sign failed: {e:?}")),
        };
        match signed {
            Ok(raw_tx) => {
                w.status = WithdrawalStatus::Signed { nonce, raw_tx };
                self.withdrawals.insert(id, w);
                env::log_str(&format!("withdrawal {id} signed with nonce {nonce}"));
                true
            }
            Err(reason) => {
                let token_id = w.token_id.clone();
                let key = self
                    .evm_asset_by_token
                    .get(token_id.as_str())
                    .cloned()
                    .unwrap_or_default();
                if let Some(mut asset) = self.evm_assets.get(&key).cloned() {
                    if asset.window_day == day {
                        asset.window_withdrawn = asset.window_withdrawn.saturating_sub(w.amount);
                        self.evm_assets.insert(key, asset);
                    }
                }
                let mut chain = self
                    .evm_chains
                    .get(&tx.chain_id)
                    .cloned()
                    .unwrap_or_else(|| env::panic_str("chain is not configured"));
                if nonce.checked_add(1) == Some(chain.next_nonce) {
                    chain.next_nonce = nonce;
                    self.evm_chains.insert(tx.chain_id, chain);
                    env::log_str(&format!(
                        "withdrawal {id} not signed ({reason}); nonce {nonce} released"
                    ));
                } else {
                    w.status = WithdrawalStatus::Failed { nonce };
                    self.pending_sub(&token_id, w.amount);
                    self.credit(&w.owner, &token_id, w.amount);
                    env::log_str(&format!(
                        "withdrawal {id} failed ({reason}); nonce {nonce} is a gap, balance credited back"
                    ));
                }
                self.withdrawals.insert(id, w);
                false
            }
        }
    }

    /// Relayer only. Records that the signed transaction of withdrawal `id` was broadcast with
    /// hash `tx_hash`. The withdrawal must be `Signed`.
    pub fn mark_withdrawal_broadcast(&mut self, id: u64, tx_hash: String) {
        self.assert_relayer();
        let hash = parse_tx_hash(&tx_hash).unwrap_or_else(|e| env::panic_str(&e));
        let mut w = self
            .withdrawals
            .get(&id)
            .cloned()
            .unwrap_or_else(|| env::panic_str(&format!("withdrawal {id} does not exist")));
        let nonce = match w.status {
            WithdrawalStatus::Signed { nonce, .. } => nonce,
            _ => env::panic_str(&format!("withdrawal {id} is not signed")),
        };
        self.pending_sub(&w.token_id, w.amount);
        w.status = WithdrawalStatus::Broadcast {
            nonce,
            tx_hash: evm::hex_0x(&hash),
        };
        self.withdrawals.insert(id, w);
    }

    /// A withdrawal by id.
    pub fn get_withdrawal(&self, id: u64) -> Option<WithdrawalView> {
        self.withdrawals.get(&id).map(|w| withdrawal_view(id, w))
    }

    /// Up to `limit` withdrawals with id >= `from_id` that are `Pending` and not being signed,
    /// in id order. Scans ids from `from_id` upwards, so it is linear in the number of
    /// withdrawals after `from_id`.
    pub fn get_pending_withdrawals(&self, from_id: u64, limit: u64) -> Vec<WithdrawalView> {
        let mut out = Vec::new();
        for id in from_id..self.next_withdrawal_id {
            if out.len() as u64 >= limit {
                break;
            }
            if let Some(w) = self.withdrawals.get(&id) {
                if matches!(w.status, WithdrawalStatus::Pending) && w.signing_nonce.is_none() {
                    out.push(withdrawal_view(id, w));
                }
            }
        }
        out
    }

    /// Up to `limit` `Signed` withdrawals with id >= `from_id`, ordered by nonce, lowest first.
    /// The relayer broadcasts them in this order. It scans ids from `from_id` upwards, so it is
    /// linear in the number of withdrawals after `from_id`.
    pub fn get_signed_withdrawals(&self, from_id: u64, limit: u64) -> Vec<WithdrawalView> {
        let mut out: Vec<WithdrawalView> = (from_id..self.next_withdrawal_id)
            .filter_map(|id| {
                self.withdrawals
                    .get(&id)
                    .filter(|w| matches!(w.status, WithdrawalStatus::Signed { .. }))
                    .map(|w| withdrawal_view(id, w))
            })
            .collect();
        out.sort_by_key(|v| v.nonce);
        out.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        out
    }
}

#[cfg(test)]
mod accounting_tests;
pub mod accounts;
#[cfg(test)]
mod accounts_tests;
#[cfg(test)]
mod bridge_tests;
#[cfg(test)]
mod intents_tests;
#[cfg(test)]
mod oneclick_tests;
#[cfg(test)]
mod permissionless_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tokenomics_tests;

/// Test helper: the value of an `execute` result that must not be a promise.
#[cfg(test)]
trait ExpectValue<T> {
    fn value(self) -> T;
}

#[cfg(test)]
impl<T> ExpectValue<T> for PromiseOrValue<T> {
    fn value(self) -> T {
        match self {
            PromiseOrValue::Value(v) => v,
            PromiseOrValue::Promise(_) => panic!("expected a value, got a promise"),
        }
    }
}
#[cfg(test)]
mod wallet_tests;
