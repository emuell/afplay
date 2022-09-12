#![allow(dead_code, unused_macros)]

pub mod actor;
pub mod decoder;
pub mod fader;
pub mod resampler;

use lazy_static::lazy_static;
use std::sync::atomic::{AtomicUsize, Ordering};

// -------------------------------------------------------------------------------------------------

/// dB value, which is treated as zero volume factor  
const MINUS_INF_IN_DB: f32 = -200.0f32;

// -------------------------------------------------------------------------------------------------

/// Generates a unique usize number, by simply counting atomically upwards from 1.
pub fn unique_usize_id() -> usize {
    static FILE_ID_COUNTER: AtomicUsize = AtomicUsize::new(1);
    FILE_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

// -------------------------------------------------------------------------------------------------

/// Convert a linear volume factor to dB.
pub fn linear_to_db(value: f32) -> f32 {
    lazy_static! {
        static ref LIN_TO_DB_FACTOR: f32 = 20.0f32 / 10.0f32.ln();
    }
    if value == 1.0 {
        return 0.0; // avoid rounding errors at exactly 0 dB
    } else if value > 1e-12f32 {
        return value.ln() * *LIN_TO_DB_FACTOR;
    }
    MINUS_INF_IN_DB
}

// -------------------------------------------------------------------------------------------------

/// Convert volume in dB to a linear volume factor.
pub fn db_to_linear(value: f32) -> f32 {
    lazy_static! {
        static ref DB_TO_LIN_FACTOR: f32 = 10.0f32.ln() / 20.0f32;
    }
    if value == 0.0f32 {
        return 1.0f32; // avoid rounding errors at exactly 0 dB
    } else if value > MINUS_INF_IN_DB {
        return (value * *DB_TO_LIN_FACTOR).exp();
    }
    0.0f32
}

// -------------------------------------------------------------------------------------------------

/// Calculate playback speed from a MIDI note, using middle C (note number 60) as base note.
pub fn speed_from_note(note: u32) -> f64 {
    // Middle Note C6 = MIDI note 60
    pitch_from_note(note) / pitch_from_note(60)
}

// -------------------------------------------------------------------------------------------------

/// Calculate Hz from a MIDI note with equal tuning based on A4 = a' = 440 Hz.
pub fn pitch_from_note(note: u32) -> f64 {
    // A4 = MIDI note 69
    440.0 * 2.0_f64.powf((note as f64 - 69.0) / 12.0)
}

// -------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! assert_eq_with_epsilon {
        ($x:expr, $y:expr, $d:expr) => {
            if !($x - $y < $d || $y - $x < $d) {
                panic!();
            }
        };
    }

    #[test]
    fn lin_db_conversion() {
        assert_eq!(linear_to_db(1.0), 0.0);
        assert_eq!(linear_to_db(0.0), MINUS_INF_IN_DB);
        assert_eq!(db_to_linear(MINUS_INF_IN_DB), 0.0);
        assert_eq!(db_to_linear(0.0), 1.0);
        assert_eq_with_epsilon!(linear_to_db(db_to_linear(20.0)), 20.0, 0.0001);
        assert_eq_with_epsilon!(linear_to_db(db_to_linear(-20.0)), -20.0, 0.0001);
    }
}
