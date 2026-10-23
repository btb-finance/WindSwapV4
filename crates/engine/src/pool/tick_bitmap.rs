//! Port of `TickBitmap.sol`: a packed mapping from tick index to its initialized state.
//!
//! Each `i16` word key holds 256 ticks. A missing word is equivalent to a word of zeros.

use std::collections::BTreeMap;

use alloy_primitives::U256;

use crate::error::{MathError, Result};

/// `mapping(int16 => uint256)`. Missing keys read as zero.
pub type TickBitmap = BTreeMap<i16, U256>;

/// Word and bit position of `tick` in the bitmap.
///
/// Matches `position(int24 tick)`: `int16(tick >> 8)` is an arithmetic shift truncated to
/// 16 bits, and `uint8(tick % 256)` keeps the low 8 bits of the (possibly negative) tick.
pub fn position(tick: i32) -> (i16, u8) {
    ((tick >> 8) as i16, tick as u8)
}

/// Flips the initialized state of `tick`. Reverts with `TickMisaligned` if `tick` is not a
/// multiple of `tick_spacing`.
///
/// `tick_spacing` must be non-zero; zero reverts in Solidity (modulo by zero) and is
/// reported as `DivisionByZero`.
pub fn flip_tick(bitmap: &mut TickBitmap, tick: i32, tick_spacing: i32) -> Result<()> {
    if tick_spacing == 0 {
        return Err(MathError::DivisionByZero);
    }
    if tick % tick_spacing != 0 {
        return Err(MathError::TickMisaligned);
    }
    let (word_pos, bit_pos) = position(tick / tick_spacing);
    let word = bitmap.entry(word_pos).or_insert(U256::ZERO);
    *word ^= U256::from(1) << usize::from(bit_pos);
    if word.is_zero() {
        bitmap.remove(&word_pos);
    }
    Ok(())
}

/// Returns the next initialized tick in the same word as `tick` (or the boundary of that word
/// if none is initialized), searching left (`lte = true`, less than or equal to `tick`) or
/// right (`lte = false`, strictly greater than `tick`).
///
/// The returned flag is `true` only if the returned tick is initialized. The search covers at
/// most one word (256 ticks). `tick_spacing` must be non-zero.
pub fn next_initialized_tick_within_one_word(
    bitmap: &TickBitmap,
    tick: i32,
    tick_spacing: i32,
    lte: bool,
) -> (i32, bool) {
    let mut compressed = tick / tick_spacing;
    // Round towards negative infinity.
    if tick < 0 && tick % tick_spacing != 0 {
        compressed -= 1;
    }

    let word_at = |word_pos: i16| bitmap.get(&word_pos).copied().unwrap_or(U256::ZERO);

    if lte {
        let (word_pos, bit_pos) = position(compressed);
        // All the 1s at or to the right of `bit_pos`.
        let mask = U256::MAX >> (255 - usize::from(bit_pos));
        let masked = word_at(word_pos) & mask;
        if masked.is_zero() {
            // No initialized tick to the right of or at the current tick: return the rightmost.
            ((compressed - i32::from(bit_pos)) * tick_spacing, false)
        } else {
            let msb = (masked.bit_len() - 1) as u8;
            let next = (compressed - i32::from(bit_pos - msb)) * tick_spacing;
            (next, true)
        }
    } else {
        // Start from the word of the next tick: the current tick's state does not matter.
        let (word_pos, bit_pos) = position(compressed + 1);
        // All the 1s at or to the left of `bit_pos`.
        let mask = U256::MAX << usize::from(bit_pos);
        let masked = word_at(word_pos) & mask;
        if masked.is_zero() {
            // No initialized tick to the left of the current tick: return the leftmost.
            (
                (compressed + 1 + i32::from(255 - bit_pos)) * tick_spacing,
                false,
            )
        } else {
            let lsb = masked.trailing_zeros() as u8;
            let next = (compressed + 1 + i32::from(lsb - bit_pos)) * tick_spacing;
            (next, true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(bitmap: &mut TickBitmap, tick: i32, spacing: i32) {
        flip_tick(bitmap, tick, spacing).unwrap();
    }

    #[test]
    fn position_matches_solidity_casts() {
        assert_eq!(position(0), (0, 0));
        assert_eq!(position(255), (0, 255));
        assert_eq!(position(256), (1, 0));
        assert_eq!(position(-1), (-1, 255));
        assert_eq!(position(-256), (-1, 0));
        assert_eq!(position(-257), (-2, 255));
        assert_eq!(position(887272), (3465, 232));
        assert_eq!(position(-887272), (-3466, 24));
    }

    #[test]
    fn flip_twice_restores_empty_bitmap() {
        let mut bitmap = TickBitmap::new();
        set(&mut bitmap, 60, 60);
        // compressed = 1, so bit 1 of word 0 is set.
        assert_eq!(bitmap.get(&0), Some(&U256::from(2)));
        set(&mut bitmap, 60, 60);
        assert!(bitmap.is_empty());
    }

    #[test]
    fn flip_rejects_misaligned_tick_without_mutating() {
        let mut bitmap = TickBitmap::new();
        assert_eq!(
            flip_tick(&mut bitmap, 61, 60),
            Err(MathError::TickMisaligned)
        );
        assert!(bitmap.is_empty());
        assert_eq!(flip_tick(&mut bitmap, 0, 0), Err(MathError::DivisionByZero));
    }

    #[test]
    fn next_tick_left_and_right() {
        let mut bitmap = TickBitmap::new();
        set(&mut bitmap, 5, 1);
        // Left (lte): from 10 finds 5; from 4 finds nothing and returns the word's boundary.
        assert_eq!(
            next_initialized_tick_within_one_word(&bitmap, 10, 1, true),
            (5, true)
        );
        assert_eq!(
            next_initialized_tick_within_one_word(&bitmap, 4, 1, true),
            (0, false)
        );
        // Right (strictly greater): from 4 finds 5; from 5 finds nothing in the word.
        assert_eq!(
            next_initialized_tick_within_one_word(&bitmap, 4, 1, false),
            (5, true)
        );
        assert_eq!(
            next_initialized_tick_within_one_word(&bitmap, 5, 1, false),
            (255, false)
        );
    }

    #[test]
    fn next_tick_negative_and_misaligned() {
        let mut bitmap = TickBitmap::new();
        set(&mut bitmap, -7, 1);
        assert_eq!(
            next_initialized_tick_within_one_word(&bitmap, -1, 1, true),
            (-7, true)
        );

        // -20 with spacing 10 is compressed to -2. Tick -15 compresses to -2 as well
        // (rounded towards negative infinity).
        let mut bitmap = TickBitmap::new();
        set(&mut bitmap, -20, 10);
        assert_eq!(
            next_initialized_tick_within_one_word(&bitmap, -15, 10, true),
            (-20, true)
        );
        assert_eq!(
            next_initialized_tick_within_one_word(&bitmap, -15, 10, false),
            (-10, false)
        );
    }
}
