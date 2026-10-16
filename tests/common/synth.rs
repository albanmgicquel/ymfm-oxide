// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! Synthetic sample data for the memory images the tests build: ADPCM-A and
//! ADPCM-B encoders of the tests' own, and waveforms computed in IEEE basic
//! arithmetic alone, so the images come out the same everywhere and no
//! sample data comes from anywhere else.

use super::Random;

/// The ADPCM-B decoder's step scale, as the encoder models it.
pub const B_STEP_SCALE: [i32; 8] = [57, 57, 57, 57, 77, 102, 128, 153];

/// Encodes `samples` as ADPCM-B from the decoder's reset state, choosing
/// each nibble greedily to land nearest the target.
pub fn encode_adpcm_b(samples: &[i32]) -> Vec<u8> {
    let mut accumulator = 0i32;
    let mut step = 127i32;
    let mut nibbles = Vec::with_capacity(samples.len());
    for &target in samples {
        let mut best = (i64::MAX, 0u8, 0i32);
        for nibble in 0u8..16 {
            let mut delta = (2 * i32::from(nibble & 7) + 1) * step / 8;
            if nibble & 8 != 0 {
                delta = -delta;
            }
            let next = (accumulator + delta).clamp(-32768, 32767);
            let error = (i64::from(next) - i64::from(target)).abs();
            if error < best.0 {
                best = (error, nibble, next);
            }
        }
        nibbles.push(best.1);
        accumulator = best.2;
        step = (step * B_STEP_SCALE[usize::from(best.1 & 7)] / 64).clamp(127, 24576);
    }
    nibbles
        .chunks(2)
        .map(|pair| (pair[0] << 4) | pair.get(1).copied().unwrap_or(0))
        .collect()
}

/// The ADPCM-A decoder's step table and step index changes.
pub const A_STEPS: [i32; 49] = [
    16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130,
    143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796,
    876, 963, 1060, 1166, 1282, 1411, 1552,
];
pub const A_STEP_INC: [i32; 8] = [-1, -1, -1, -1, 2, 5, 7, 9];

/// Encodes `samples` (12-bit) as ADPCM-A from the decoder's reset state.
pub fn encode_adpcm_a(samples: &[i32]) -> Vec<u8> {
    let mut accumulator = 0i32;
    let mut index = 0i32;
    let mut nibbles = Vec::with_capacity(samples.len());
    for &target in samples {
        let mut best = (i64::MAX, 0u8, 0i32);
        for nibble in 0u8..16 {
            let mut delta = (2 * i32::from(nibble & 7) + 1) * A_STEPS[index as usize] / 8;
            if nibble & 8 != 0 {
                delta = -delta;
            }
            let next = (accumulator + delta) & 0xfff;
            let signed = (next << 20) >> 20;
            let error = (i64::from(signed) - i64::from(target)).abs();
            if error < best.0 {
                best = (error, nibble, next);
            }
        }
        nibbles.push(best.1);
        accumulator = best.2;
        index = (index + A_STEP_INC[usize::from(best.1 & 7)]).clamp(0, 48);
    }
    nibbles
        .chunks(2)
        .map(|pair| (pair[0] << 4) | pair.get(1).copied().unwrap_or(0))
        .collect()
}

/// The sine of `x`, from a Taylor series in IEEE basic arithmetic alone,
/// so the images come out the same on every platform's libm.
pub fn portable_sin(x: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    let mut x = x % TAU;
    if x > PI {
        x -= TAU;
    } else if x < -PI {
        x += TAU;
    }
    let mut term = x;
    let mut sum = x;
    for k in 1..15 {
        term *= -x * x / f64::from((2 * k) * (2 * k + 1));
        sum += term;
    }
    sum
}

/// A sine of `period` samples.
pub fn sine(count: usize, period: f64, amplitude: f64) -> Vec<i32> {
    (0..count)
        .map(|n| {
            (amplitude * portable_sin(std::f64::consts::TAU * n as f64 / period)).round() as i32
        })
        .collect()
}

/// A rising ramp that restarts every `period` samples.
pub fn ramp(count: usize, period: usize, amplitude: i32) -> Vec<i32> {
    (0..count)
        .map(|n| -amplitude + (2 * amplitude) * (n % period) as i32 / period as i32)
        .collect()
}

/// A square that swings between the extremes every `half` samples.
pub fn square(count: usize, half: usize, amplitude: i32) -> Vec<i32> {
    (0..count)
        .map(|n| {
            if (n / half).is_multiple_of(2) {
                amplitude
            } else {
                -amplitude
            }
        })
        .collect()
}

/// A sweep up from a slow sine to a fast one, decaying.
pub fn chirp(count: usize, amplitude: f64) -> Vec<i32> {
    let mut phase = 0.0f64;
    (0..count)
        .map(|n| {
            let t = n as f64 / count as f64;
            phase += std::f64::consts::TAU / (200.0 - 190.0 * t);
            (amplitude * (1.0 - 0.8 * t) * portable_sin(phase)).round() as i32
        })
        .collect()
}

/// Pseudo-random bytes.
pub fn noise(count: usize, seed: u64) -> Vec<u8> {
    let mut random = Random::new(seed);
    (0..count).map(|_| random.byte()).collect()
}
