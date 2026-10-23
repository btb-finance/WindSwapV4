use core::fmt;

/// Errors returned where the Solidity code would revert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathError {
    /// Division by zero (`require(denominator > 0)` or EVM division panic).
    DivisionByZero,
    /// Result does not fit in the target type.
    Overflow,
    /// Subtraction went below zero where Solidity checks it.
    Underflow,
    /// `TickMath`: tick outside `[MIN_TICK, MAX_TICK]` (`"T"`).
    TickOutOfRange,
    /// `TickMath`: sqrt price outside `[MIN_SQRT_RATIO, MAX_SQRT_RATIO)` (`"R"`).
    SqrtPriceOutOfRange,
    /// `LiquidityMath`: liquidity subtraction underflow (`"LS"`).
    LiquiditySub,
    /// `LiquidityMath`: liquidity addition overflow (`"LA"`).
    LiquidityAdd,
    /// Zero sqrt price or zero liquidity passed where it is not allowed.
    InvalidInput,
    /// Price would move past the allowed bound.
    PriceOutOfBounds,
    /// `Tick.update`: liquidity gross above max liquidity per tick (`"LO"`).
    LiquidityOverflow,
    /// `TickBitmap.flipTick`: tick is not a multiple of the tick spacing.
    TickMisaligned,
    /// `Position.update`: poke on a position with zero liquidity (`"NP"`).
    NoPositionLiquidity,
    /// `Oracle`: oracle not initialized / zero cardinality (`"I"`).
    OracleNotInitialized,
    /// `Oracle`: requested time is older than the oldest observation (`"OLD"`).
    ObservationTooOld,
    /// `CLPool.checkTicks`: invalid tick range (`"TLU"`, `"TLM"`, `"TUM"`).
    InvalidTickRange,
    /// `CLPool`: pool not initialized (`"LOK"` before `initialize`).
    PoolNotInitialized,
    /// `CLPool.initialize`: pool already initialized.
    PoolAlreadyInitialized,
    /// `CLPool`: zero amount (`"AS"`, `require(amount > 0)`).
    ZeroAmount,
    /// `CLPool.swap`: invalid sqrt price limit (`"SPL"`).
    InvalidPriceLimit,
    /// `CLPool.snapshotCumulativesInside`: tick not initialized.
    TickNotInitialized,
    /// `CLPool.setGaugeAndPositionManager`: gauge already set.
    GaugeAlreadySet,
    /// `SwapRouter`: exact-input route delivered less than `amountOutMinimum` (`"Too little received"`).
    TooLittleReceived,
    /// `SwapRouter`: exact-output route needed more than `amountInMaximum` (`"Too much requested"`).
    TooMuchRequested,
    /// `Minter.nudge`: tail emissions not active yet, `weekly >= TAIL_START` (`TailEmissionsInactive`).
    TailEmissionsInactive,
    /// `Minter.nudge`: the current period was already nudged (`AlreadyNudged`).
    AlreadyNudged,
    /// `Minter.setTeamRate`: rate above `MAXIMUM_TEAM_RATE` (`RateTooHigh`).
    RateTooHigh,
    /// `VotingEscrow`: caller is not the token owner (`NotApprovedOrOwner`; no approvals are modelled).
    NotApprovedOrOwner,
    /// `VotingEscrow.transferFrom`: `from` is not the current owner (`NotOwner`).
    NotOwner,
    /// `VotingEscrow`: the lock has expired (`LockExpired`).
    LockExpired,
    /// `VotingEscrow.withdraw`: the lock has not expired yet (`LockNotExpired`).
    LockNotExpired,
    /// `VotingEscrow`: the unlock time is not after `now` / the current lock end (`LockDurationNotInFuture`).
    LockDurationNotInFuture,
    /// `VotingEscrow`: the unlock time is more than `MAXTIME` after `now` (`LockDurationTooLong`).
    LockDurationTooLong,
    /// `VotingEscrow`: the token has no locked amount (`NoLockFound`).
    NoLockFound,
    /// `VotingEscrow`: the operation does not apply to a permanent lock (`PermanentLock`).
    PermanentLock,
    /// `VotingEscrow.unlockPermanent`: the token is not permanently locked (`NotPermanentLock`).
    NotPermanentLock,
    /// `VotingEscrow.merge`: `from == to` (`SameNFT`).
    SameNft,
    /// `VotingEscrow.split`: the token has no owner (`SplitNoOwner`).
    SplitNoOwner,
    /// `VotingEscrow.split`: neither the owner nor address zero is in `canSplit` (`SplitNotAllowed`).
    SplitNotAllowed,
    /// `VotingEscrow.split`: the split amount is not below the lock amount (`AmountTooBig`).
    AmountTooBig,
    /// `VotingEscrow`: the token is flagged as voted (`AlreadyVoted`).
    AlreadyVoted,
    /// `VotingEscrow.unlockPermanent`: the token was transferred in this block (`OwnershipChange`).
    OwnershipChange,
    /// `VotingEscrow.toggleSplit`: caller is not `team` (`NotTeam`).
    NotTeam,
    /// `VotingEscrow.voting`: caller is not `voter` (`NotVoter`).
    NotVoter,
    /// `SafeCastLibrary.toInt128`: value above the `int128` maximum (`SafeCastOverflow`).
    SafeCastOverflow,
    /// `SafeCastLibrary.toUint256`: negative `int128` value (`SafeCastUnderflow`).
    SafeCastUnderflow,
    /// `Reward`: caller is not the authorized voter, or not approved or owner of the veNFT (`NotAuthorized`).
    NotAuthorized,
    /// `VotingEscrow`: the token is not a normal veNFT (`NotNormalNFT`).
    NotNormalNft,
    /// `VotingEscrow`: the token is neither managed nor normal (`NotManagedOrNormalNFT`).
    NotManagedOrNormalNft,
    /// `VotingEscrow`: the token is not a managed veNFT (`NotManagedNFT`).
    NotManagedNft,
    /// `VotingEscrow.withdrawManaged`: the token is not locked into a managed veNFT (`NotLockedNFT`).
    NotLockedNft,
    /// `VotingEscrow.withdrawManaged`: the token has no managed veNFT (`InvalidManagedNFTId`).
    InvalidManagedNftId,
    /// `VotingEscrow.createManagedLockFor`: caller is neither the allowed manager nor the governor.
    NotGovernorOrManager,
    /// `VotingEscrow.setManagedState`: caller is neither the emergency council nor the governor.
    NotEmergencyCouncilOrGovernor,
    /// `VotingEscrow.setAllowedManager`: caller is not the governor (`NotGovernor`).
    NotGovernor,
    /// `VotingEscrow`: the new value equals the current one (`SameAddress`).
    SameAddress,
    /// `VotingEscrow.setAllowedManager`: the zero address (`ZeroAddress`).
    ZeroAddress,
    /// `VotingEscrow.setManagedState`: the state is already set (`SameState`).
    SameState,
    /// `VotingEscrow.depositManaged`: the token has no voting balance (`ZeroBalance`).
    ZeroBalance,
    /// `VotingEscrow.depositFor`: a managed veNFT is only fed by the distributor (`NotDistributor`).
    NotDistributor,
    /// `VotingEscrow.delegate`: the delegatee does not exist (`NonExistentToken`).
    NonExistentToken,
    /// `VotingEscrow.delegateBySig`: `s` is in the upper half of the curve order (`InvalidSignatureS`).
    InvalidSignatureS,
    /// `VotingEscrow.delegateBySig`: the signature does not recover to an address (`InvalidSignature`).
    InvalidSignature,
    /// `VotingEscrow.delegateBySig`: the nonce is not the signer's next nonce (`InvalidNonce`).
    InvalidNonce,
    /// `VotingEscrow.delegateBySig`: `block.timestamp > expiry` (`SignatureExpired`).
    SignatureExpired,
    /// `LockedManagedReward`: caller is not the escrow (`NotVotingEscrow`).
    NotVotingEscrow,
    /// `LockedManagedReward.getReward`: not exactly one token (`NotSingleToken`).
    NotSingleToken,
    /// `LockedManagedReward`: token is not the escrow token (`NotEscrowToken`).
    NotEscrowToken,
    /// `FeesVotingReward.notifyRewardAmount`: sender is not the gauge mapped to this contract (`NotGauge`).
    NotGauge,
    /// `FeesVotingReward.notifyRewardAmount`: token is not a reward of this contract (`InvalidReward`).
    InvalidReward,
    /// `BribeVotingReward.notifyRewardAmount`: a new reward token is not whitelisted (`NotWhitelisted`).
    NotWhitelisted,
    /// `Voter`: the token already voted or deposited in this epoch (`AlreadyVotedOrDeposited`).
    AlreadyVotedOrDeposited,
    /// `Voter`: inside the first hour of the epoch, when distribution happens (`DistributeWindow`).
    DistributeWindow,
    /// `Voter.vote`: pool and weight arrays have different lengths (`UnequalLengths`).
    UnequalLengths,
    /// `Voter.vote`: more pools than `maxVotingNum` (`TooManyPools`).
    TooManyPools,
    /// `Voter.vote`: after the vote window and the token is not whitelisted (`NotWhitelistedNFT`).
    NotWhitelistedNft,
    /// `Voter`: caller is not the governor (`NotGovernor`).
    NotGovernor,
    /// `Voter`: caller is not the emergency council (`NotEmergencyCouncil`).
    NotEmergencyCouncil,
    /// `Voter` / `RewardsDistributor`: caller is not the minter (`NotMinter`).
    NotMinter,
    /// `Voter.setMaxVotingNum`: below the minimum of 10 (`MaximumVotingNumberTooLow`).
    MaximumVotingNumberTooLow,
    /// `Voter.setMaxVotingNum`: the value is already set (`SameValue`).
    SameValue,
    /// `Voter._vote`: the pool has no gauge (`GaugeDoesNotExist`).
    GaugeDoesNotExist,
    /// `Voter._vote`: the gauge is killed (`GaugeNotAlive`).
    GaugeNotAlive,
    /// `Voter._vote`: the pool already has a vote from this token (`NonZeroVotes`).
    NonZeroVotes,
    /// `Voter._vote`: the pool's vote weight rounds to zero (`ZeroBalance`).
    ZeroBalance,
    /// `Voter.createGauge`: the pool factory is not approved in the registry (`FactoryPathNotApproved`).
    FactoryPathNotApproved,
    /// `Voter.createGauge`: the pool already has a gauge (`GaugeExists`).
    GaugeExists,
    /// `Voter.createGauge`: a non-governor used an address that is not a pool (`NotAPool`).
    NotAPool,
    /// `Voter.createGauge`: a non-governor used a pool with a token that is not whitelisted (`NotWhitelistedToken`).
    NotWhitelistedToken,
    /// `Voter.killGauge`: the gauge is already killed (`GaugeAlreadyKilled`).
    GaugeAlreadyKilled,
    /// `Voter.reviveGauge`: the gauge is already alive (`GaugeAlreadyRevived`).
    GaugeAlreadyRevived,
    /// `Voter.distribute` / `updateFor`: pool index past the end of `pools` (`Panic(0x32)`).
    IndexOutOfBounds,
    /// `Voter.setEmergencyCouncil`: the zero address (`ZeroAddress`).
    ZeroAddress,
    /// `RewardsDistributor.claim`: the minter's active period is behind the current week (`UpdatePeriod`).
    UpdatePeriod,
    /// `RewardsDistributor`: a payout to an owner of address zero (OZ `ERC20: transfer to the zero address`).
    TransferToZeroAddress,
}

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

pub type Result<T> = core::result::Result<T, MathError>;
