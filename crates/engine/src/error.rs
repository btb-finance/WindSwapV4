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
}

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

pub type Result<T> = core::result::Result<T, MathError>;
