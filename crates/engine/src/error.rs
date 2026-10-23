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
}

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

pub type Result<T> = core::result::Result<T, MathError>;
