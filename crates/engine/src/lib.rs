//! Wind Swap v4 engine.
//!
//! A pure, deterministic Rust port of the Wind Swap protocol logic. The crate has
//! no I/O, no clock and no networking: every input (including time) is passed in,
//! so the same code can run in a server, inside a NEAR contract, or in a node.
//!
//! All math matches the Solidity contracts bit-for-bit. See `diff-tests/` for the
//! differential test vectors generated from the original Solidity libraries.

pub mod error;
pub mod fees;
pub mod gauge;
pub mod math;
pub mod minter;
pub mod pool;
pub mod rebase;
pub mod rewards;
pub mod router;
pub mod ve;
pub mod voter;

pub use alloy_primitives::{I256, U256};
pub use error::MathError;
