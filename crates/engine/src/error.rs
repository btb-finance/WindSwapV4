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
    /// `Reward`: caller is not the authorized voter, or not approved or owner of the veNFT (`NotAuthorized`).
    NotAuthorized,
    /// `FeesVotingReward.notifyRewardAmount`: sender is not the gauge mapped to this contract (`NotGauge`).
    NotGauge,
    /// `FeesVotingReward.notifyRewardAmount`: token is not a reward of this contract (`InvalidReward`).
    InvalidReward,
    /// `BribeVotingReward.notifyRewardAmount`: a new reward token is not whitelisted (`NotWhitelisted`).
    NotWhitelisted,
}

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

pub type Result<T> = core::result::Result<T, MathError>;
