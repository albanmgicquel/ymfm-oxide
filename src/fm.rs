// This file is a Rust translation of ymfm's `ymfm.h`, `ymfm_fm.h` and
// `ymfm_fm.ipp` at commit 81aec25 (https://github.com/aaronsgiles/ymfm), with
// changes: it keeps only what the OPL family uses, so the OPM noise channel,
// SSG-EG, detune, the OPLL depress and OPQ reverb envelope stages, timers,
// status, IRQs and save states are gone; operators and channels refer to each
// other by index instead of by pointer; and the engine is specialized for the
// OPL registers rather than generic over a register class. Every computation
// that reaches a sample is kept as ymfm writes it, including its integer
// widths, so the output matches ymfm sample for sample.
//
// The original carries this notice, which applies to this translation:
//
// BSD 3-Clause License
//
// Copyright (c) 2021, Aaron Giles
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this
//    list of conditions and the following disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its
//    contributors may be used to endorse or promote products derived from
//    this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
// FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
// CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
// OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

//! The FM core: operators, channels and the engine that clocks them.
//!
//! Arithmetic here is deliberately written in ymfm's integer widths. Where the
//! C++ relies on an implicit conversion, an unsigned wrap or a truncation to
//! sixteen bits, the Rust spells the same operation out, and a comment says so
//! where the reason is not obvious.

use super::registers::{Registers, RHYTHM_CHANNEL};

/// Extracts `length` bits of `value` starting at bit `start`.
#[inline(always)]
pub(crate) const fn bitfield(value: u32, start: u32, length: u32) -> u32 {
    (value >> start) & ((1 << length) - 1)
}

/// Clamps `value` between `minval` and `maxval`.
#[inline(always)]
pub(crate) const fn clamp(value: i32, minval: i32, maxval: i32) -> i32 {
    if value < minval {
        minval
    } else if value > maxval {
        maxval
    } else {
        value
    }
}

/// Computes what a round trip through the 3.10 floating-point DAC format
/// leaves of a value: the low bits its exponent cannot carry are cleared.
pub(crate) fn roundtrip_fp(value: i32) -> i32 {
    if value < -32768 {
        return -32768;
    }
    if value > 32767 {
        return 32767;
    }
    // The number of leading sign bits after the sign.
    let scanvalue = value ^ (value >> 31);
    let exponent = 7 - (((scanvalue as u32) << 17).leading_zeros() as i32);
    let exponent = exponent.max(1) - 1;
    let mask = (1i32 << exponent) - 1;
    value & !mask
}

/// The envelope generator states the OPL uses. The values are ymfm's, because
/// they index the cached rate table.
pub(crate) const EG_DEPRESS: usize = 0;
pub(crate) const EG_ATTACK: usize = 1;
pub(crate) const EG_DECAY: usize = 2;
pub(crate) const EG_SUSTAIN: usize = 3;
pub(crate) const EG_RELEASE: usize = 4;
pub(crate) const EG_STATES: usize = 6;

/// The sources a key on can come from; the effective key is their union.
pub(crate) const KEYON_NORMAL: u32 = 0;
pub(crate) const KEYON_RHYTHM: u32 = 1;
pub(crate) const KEYON_CSM: u32 = 2;

/// The size of a full waveform.
pub(crate) const WAVEFORM_LENGTH: u32 = 0x400;

/// Returns the absolute value of sin(input), where 0 to 2 pi maps onto ten
/// bits, as a 4.8 fixed-point logarithmic attenuation.
pub(crate) const fn abs_sin_attenuation(input: u32) -> u32 {
    // Stored as 4.8 logarithmic values for a quarter phase, matching the
    // internal format of the OPN chip as extracted from the die.
    const SIN_TABLE: [u16; 256] = [
        0x859, 0x6c3, 0x607, 0x58b, 0x52e, 0x4e4, 0x4a6, 0x471, 0x443, 0x41a, 0x3f5, 0x3d3, 0x3b5,
        0x398, 0x37e, 0x365, 0x34e, 0x339, 0x324, 0x311, 0x2ff, 0x2ed, 0x2dc, 0x2cd, 0x2bd, 0x2af,
        0x2a0, 0x293, 0x286, 0x279, 0x26d, 0x261, 0x256, 0x24b, 0x240, 0x236, 0x22c, 0x222, 0x218,
        0x20f, 0x206, 0x1fd, 0x1f5, 0x1ec, 0x1e4, 0x1dc, 0x1d4, 0x1cd, 0x1c5, 0x1be, 0x1b7, 0x1b0,
        0x1a9, 0x1a2, 0x19b, 0x195, 0x18f, 0x188, 0x182, 0x17c, 0x177, 0x171, 0x16b, 0x166, 0x160,
        0x15b, 0x155, 0x150, 0x14b, 0x146, 0x141, 0x13c, 0x137, 0x133, 0x12e, 0x129, 0x125, 0x121,
        0x11c, 0x118, 0x114, 0x10f, 0x10b, 0x107, 0x103, 0x0ff, 0x0fb, 0x0f8, 0x0f4, 0x0f0, 0x0ec,
        0x0e9, 0x0e5, 0x0e2, 0x0de, 0x0db, 0x0d7, 0x0d4, 0x0d1, 0x0cd, 0x0ca, 0x0c7, 0x0c4, 0x0c1,
        0x0be, 0x0bb, 0x0b8, 0x0b5, 0x0b2, 0x0af, 0x0ac, 0x0a9, 0x0a7, 0x0a4, 0x0a1, 0x09f, 0x09c,
        0x099, 0x097, 0x094, 0x092, 0x08f, 0x08d, 0x08a, 0x088, 0x086, 0x083, 0x081, 0x07f, 0x07d,
        0x07a, 0x078, 0x076, 0x074, 0x072, 0x070, 0x06e, 0x06c, 0x06a, 0x068, 0x066, 0x064, 0x062,
        0x060, 0x05e, 0x05c, 0x05b, 0x059, 0x057, 0x055, 0x053, 0x052, 0x050, 0x04e, 0x04d, 0x04b,
        0x04a, 0x048, 0x046, 0x045, 0x043, 0x042, 0x040, 0x03f, 0x03e, 0x03c, 0x03b, 0x039, 0x038,
        0x037, 0x035, 0x034, 0x033, 0x031, 0x030, 0x02f, 0x02e, 0x02d, 0x02b, 0x02a, 0x029, 0x028,
        0x027, 0x026, 0x025, 0x024, 0x023, 0x022, 0x021, 0x020, 0x01f, 0x01e, 0x01d, 0x01c, 0x01b,
        0x01a, 0x019, 0x018, 0x017, 0x017, 0x016, 0x015, 0x014, 0x014, 0x013, 0x012, 0x011, 0x011,
        0x010, 0x00f, 0x00f, 0x00e, 0x00d, 0x00d, 0x00c, 0x00c, 0x00b, 0x00a, 0x00a, 0x009, 0x009,
        0x008, 0x008, 0x007, 0x007, 0x007, 0x006, 0x006, 0x005, 0x005, 0x005, 0x004, 0x004, 0x004,
        0x003, 0x003, 0x003, 0x002, 0x002, 0x002, 0x002, 0x001, 0x001, 0x001, 0x001, 0x001, 0x001,
        0x001, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000,
    ];
    // The second half of the quarter curve mirrors the first.
    let index = if bitfield(input, 8, 1) != 0 {
        !input
    } else {
        input
    };
    SIN_TABLE[(index & 0xff) as usize] as u32
}

/// Builds one entry of the power table: the 10-bit mantissa with its implied
/// leading bit folded in, shifted up by two so a right shift is all a lookup
/// needs.
const fn power(mantissa: u16) -> u16 {
    (mantissa | 0x400) << 2
}

/// Converts a 5.8 fixed-point logarithmic attenuation to a 13-bit linear
/// volume.
#[inline(always)]
pub(crate) fn attenuation_to_volume(input: u32) -> u32 {
    // Stored in reverse order, which saves inverting the input.
    const POWER_TABLE: [u16; 256] = {
        const MANTISSAS: [u16; 256] = [
            0x3fa, 0x3f5, 0x3ef, 0x3ea, 0x3e4, 0x3df, 0x3da, 0x3d4, 0x3cf, 0x3c9, 0x3c4, 0x3bf,
            0x3b9, 0x3b4, 0x3ae, 0x3a9, 0x3a4, 0x39f, 0x399, 0x394, 0x38f, 0x38a, 0x384, 0x37f,
            0x37a, 0x375, 0x370, 0x36a, 0x365, 0x360, 0x35b, 0x356, 0x351, 0x34c, 0x347, 0x342,
            0x33d, 0x338, 0x333, 0x32e, 0x329, 0x324, 0x31f, 0x31a, 0x315, 0x310, 0x30b, 0x306,
            0x302, 0x2fd, 0x2f8, 0x2f3, 0x2ee, 0x2e9, 0x2e5, 0x2e0, 0x2db, 0x2d6, 0x2d2, 0x2cd,
            0x2c8, 0x2c4, 0x2bf, 0x2ba, 0x2b5, 0x2b1, 0x2ac, 0x2a8, 0x2a3, 0x29e, 0x29a, 0x295,
            0x291, 0x28c, 0x288, 0x283, 0x27f, 0x27a, 0x276, 0x271, 0x26d, 0x268, 0x264, 0x25f,
            0x25b, 0x257, 0x252, 0x24e, 0x249, 0x245, 0x241, 0x23c, 0x238, 0x234, 0x230, 0x22b,
            0x227, 0x223, 0x21e, 0x21a, 0x216, 0x212, 0x20e, 0x209, 0x205, 0x201, 0x1fd, 0x1f9,
            0x1f5, 0x1f0, 0x1ec, 0x1e8, 0x1e4, 0x1e0, 0x1dc, 0x1d8, 0x1d4, 0x1d0, 0x1cc, 0x1c8,
            0x1c4, 0x1c0, 0x1bc, 0x1b8, 0x1b4, 0x1b0, 0x1ac, 0x1a8, 0x1a4, 0x1a0, 0x19c, 0x199,
            0x195, 0x191, 0x18d, 0x189, 0x185, 0x181, 0x17e, 0x17a, 0x176, 0x172, 0x16f, 0x16b,
            0x167, 0x163, 0x160, 0x15c, 0x158, 0x154, 0x151, 0x14d, 0x149, 0x146, 0x142, 0x13e,
            0x13b, 0x137, 0x134, 0x130, 0x12c, 0x129, 0x125, 0x122, 0x11e, 0x11b, 0x117, 0x114,
            0x110, 0x10c, 0x109, 0x106, 0x102, 0x0ff, 0x0fb, 0x0f8, 0x0f4, 0x0f1, 0x0ed, 0x0ea,
            0x0e7, 0x0e3, 0x0e0, 0x0dc, 0x0d9, 0x0d6, 0x0d2, 0x0cf, 0x0cc, 0x0c8, 0x0c5, 0x0c2,
            0x0be, 0x0bb, 0x0b8, 0x0b5, 0x0b1, 0x0ae, 0x0ab, 0x0a8, 0x0a4, 0x0a1, 0x09e, 0x09b,
            0x098, 0x094, 0x091, 0x08e, 0x08b, 0x088, 0x085, 0x082, 0x07e, 0x07b, 0x078, 0x075,
            0x072, 0x06f, 0x06c, 0x069, 0x066, 0x063, 0x060, 0x05d, 0x05a, 0x057, 0x054, 0x051,
            0x04e, 0x04b, 0x048, 0x045, 0x042, 0x03f, 0x03c, 0x039, 0x036, 0x033, 0x030, 0x02d,
            0x02a, 0x028, 0x025, 0x022, 0x01f, 0x01c, 0x019, 0x016, 0x014, 0x011, 0x00e, 0x00b,
            0x008, 0x006, 0x003, 0x000,
        ];
        let mut table = [0u16; 256];
        let mut index = 0;
        while index < 256 {
            table[index] = power(MANTISSAS[index]);
            index += 1;
        }
        table
    };
    // The fractional part from the table, shifted by the whole part.
    (POWER_TABLE[(input & 0xff) as usize] as u32) >> (input >> 8)
}

/// Returns the 4-bit attenuation increment for a 6-bit rate at one of eight
/// steps, or for an attack the fractional scale to decrease by.
#[inline(always)]
fn attenuation_increment(rate: u32, index: u32) -> u32 {
    const INCREMENT_TABLE: [u32; 64] = [
        0x00000000, 0x00000000, 0x10101010, 0x10101010, // 0-3    (0x00-0x03)
        0x10101010, 0x10101010, 0x11101110, 0x11101110, // 4-7    (0x04-0x07)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 8-11   (0x08-0x0B)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 12-15  (0x0C-0x0F)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 16-19  (0x10-0x13)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 20-23  (0x14-0x17)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 24-27  (0x18-0x1B)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 28-31  (0x1C-0x1F)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 32-35  (0x20-0x23)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 36-39  (0x24-0x27)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 40-43  (0x28-0x2B)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 44-47  (0x2C-0x2F)
        0x11111111, 0x21112111, 0x21212121, 0x22212221, // 48-51  (0x30-0x33)
        0x22222222, 0x42224222, 0x42424242, 0x44424442, // 52-55  (0x34-0x37)
        0x44444444, 0x84448444, 0x84848484, 0x88848884, // 56-59  (0x38-0x3B)
        0x88888888, 0x88888888, 0x88888888, 0x88888888, // 60-63  (0x3C-0x3F)
    ];
    bitfield(INCREMENT_TABLE[rate as usize], 4 * index, 4)
}

/// Data computed once when an operator is prepared and read on every sample
/// after it.
#[derive(Clone, Copy, Default)]
pub(crate) struct Cache {
    /// Selects the waveform table, which stands in for ymfm's table pointer.
    pub(crate) waveform: u8,
    /// The phase step, or `PHASE_STEP_DYNAMIC` while vibrato is on.
    pub(crate) phase_step: u32,
    /// Total level times eight, plus key scale level.
    pub(crate) total_level: u32,
    /// The raw block and frequency number the phase step is computed from.
    pub(crate) block_freq: u32,
    /// The frequency multiple, as an x.1 value.
    pub(crate) multiple: u32,
    /// The sustain level, shifted up to envelope values.
    pub(crate) eg_sustain: u32,
    /// The envelope rate for each state, key scale rate included.
    pub(crate) eg_rate: [u8; EG_STATES],
}

/// Marks a phase step that has to be recomputed every sample, because the PM
/// LFO moves it.
pub(crate) const PHASE_STEP_DYNAMIC: u32 = 1;

/// The attenuation past which an operator contributes nothing.
const EG_QUIET: u16 = 0x380;

/// One operator, the slot that turns a phase into an enveloped waveform.
#[derive(Clone, Copy)]
pub(crate) struct Operator {
    choffs: u32,
    opoffs: u32,
    /// The phase, in 10.10 format.
    phase: u32,
    /// The envelope attenuation, in 4.6 format.
    env_attenuation: u16,
    env_state: usize,
    key_state: u8,
    /// One bit per key-on source.
    keyon_live: u8,
    cache: Cache,
}

impl Operator {
    const fn new(opoffs: u32) -> Operator {
        Operator {
            choffs: 0,
            opoffs,
            phase: 0,
            env_attenuation: 0x3ff,
            env_state: EG_RELEASE,
            key_state: 0,
            keyon_live: 0,
            cache: Cache {
                waveform: 0,
                phase_step: 0,
                total_level: 0,
                block_freq: 0,
                multiple: 0,
                eg_sustain: 0,
                eg_rate: [0; EG_STATES],
            },
        }
    }

    fn reset(&mut self) {
        self.phase = 0;
        self.env_attenuation = 0x3ff;
        self.env_state = EG_RELEASE;
        self.key_state = 0;
        self.keyon_live = 0;
    }

    /// Returns the integral part of the phase.
    #[inline(always)]
    fn phase(&self) -> u32 {
        self.phase >> 10
    }

    /// Caches the registers and clocks the key state. `is_active` then
    /// reports whether the operator still sounds.
    fn prepare<const OPL3: bool>(&mut self, regs: &Registers<OPL3>) {
        regs.cache_operator_data(self.choffs, self.opoffs, &mut self.cache);
        self.clock_keystate(u32::from(self.keyon_live != 0));
        self.keyon_live &= !(1 << KEYON_CSM);
    }

    fn is_active(&self) -> bool {
        self.env_state != EG_RELEASE || self.env_attenuation < EG_QUIET
    }

    fn clock<const OPL3: bool>(
        &mut self,
        regs: &Registers<OPL3>,
        env_counter: u32,
        lfo_raw_pm: i32,
    ) {
        // The envelope runs when the low two bits of the x.2 counter are zero.
        if bitfield(env_counter, 0, 2) == 0 {
            self.clock_envelope(env_counter >> 2);
        }
        self.clock_phase(regs, lfo_raw_pm);
    }

    /// Computes the 14-bit signed output for a phase and an AM offset.
    #[inline(always)]
    fn compute_volume<const OPL3: bool>(
        &self,
        regs: &Registers<OPL3>,
        phase: u32,
        am_offset: u32,
    ) -> i32 {
        if self.env_attenuation > EG_QUIET {
            return 0;
        }
        let waveform = regs.waveform(self.cache.waveform);
        let sin_attenuation = waveform[(phase & (WAVEFORM_LENGTH - 1)) as usize] as u32;
        let env_attenuation = self.envelope_attenuation(regs, am_offset) << 2;
        let result = attenuation_to_volume((sin_attenuation & 0x7fff) + env_attenuation) as i32;
        if bitfield(sin_attenuation, 15, 1) != 0 {
            -result
        } else {
            result
        }
    }

    fn keyonoff(&mut self, on: u32, kind: u32) {
        self.keyon_live = (self.keyon_live & !(1u8 << kind)) | ((bitfield(on, 0, 1) as u8) << kind);
    }

    fn start_attack(&mut self) {
        if self.env_state == EG_ATTACK {
            return;
        }
        self.env_state = EG_ATTACK;
        // A key on resets the phase.
        self.phase = 0;
        // An attack rate of 62 or more goes to full volume at once.
        if self.cache.eg_rate[EG_ATTACK] >= 62 {
            self.env_attenuation = 0;
        }
    }

    fn start_release(&mut self) {
        if self.env_state >= EG_RELEASE {
            return;
        }
        self.env_state = EG_RELEASE;
    }

    fn clock_keystate(&mut self, keystate: u32) {
        if (keystate ^ u32::from(self.key_state)) != 0 {
            self.key_state = keystate as u8;
            if keystate != 0 {
                self.start_attack();
            } else {
                self.start_release();
            }
        }
    }

    fn clock_envelope(&mut self, env_counter: u32) {
        if self.env_state == EG_ATTACK && self.env_attenuation == 0 {
            self.env_state = EG_DECAY;
        }
        // Straight after the attack-to-decay step, so that a sustain level of
        // zero skips decay entirely.
        if self.env_state == EG_DECAY && u32::from(self.env_attenuation) >= self.cache.eg_sustain {
            self.env_state = EG_SUSTAIN;
        }
        let rate = u32::from(self.cache.eg_rate[self.env_state]);

        // Shift the counter so that it becomes a 5.11 fixed-point number.
        let rate_shift = rate >> 2;
        let env_counter = env_counter << rate_shift;
        if bitfield(env_counter, 0, 11) != 0 {
            return;
        }
        let relevant_bits = bitfield(
            env_counter,
            if rate_shift <= 11 { 11 } else { rate_shift },
            3,
        );
        let increment = attenuation_increment(rate, relevant_bits);

        if self.env_state == EG_ATTACK {
            // Attack rates of 62 and 63 do not move once the key is on.
            if rate < 62 {
                // ymfm writes `att += (~att * increment) >> 4` on a uint16_t,
                // which C evaluates in unsigned 32 bits and truncates back.
                let att = u32::from(self.env_attenuation);
                self.env_attenuation = att.wrapping_add((!att).wrapping_mul(increment) >> 4) as u16;
            }
        } else {
            self.env_attenuation = self.env_attenuation.wrapping_add(increment as u16);
            if self.env_attenuation >= 0x400 {
                self.env_attenuation = 0x3ff;
            }
        }
    }

    fn clock_phase<const OPL3: bool>(&mut self, regs: &Registers<OPL3>, lfo_raw_pm: i32) {
        let mut phase_step = self.cache.phase_step;
        if phase_step == PHASE_STEP_DYNAMIC {
            phase_step = regs.compute_phase_step(self.opoffs, &self.cache, lfo_raw_pm);
        }
        self.phase = self.phase.wrapping_add(phase_step);
    }

    fn envelope_attenuation<const OPL3: bool>(
        &self,
        regs: &Registers<OPL3>,
        am_offset: u32,
    ) -> u32 {
        let mut result = u32::from(self.env_attenuation);
        if regs.op_lfo_am_enable(self.opoffs) != 0 {
            result += am_offset;
        }
        result += self.cache.total_level;
        result.min(0x3ff)
    }
}

/// No operator in this slot of a channel.
const NO_OPERATOR: u8 = 0xff;

/// One channel: two or four operators and the feedback memory of the first.
#[derive(Clone, Copy)]
pub(crate) struct Channel {
    choffs: u32,
    feedback: [i16; 2],
    /// The next input to operator 1's feedback, set while computing output.
    feedback_in: i16,
    /// The operators by index into the engine's array, or `NO_OPERATOR`.
    ops: [u8; 4],
}

impl Channel {
    const fn new(choffs: u32) -> Channel {
        Channel {
            choffs,
            feedback: [0, 0],
            feedback_in: 0,
            ops: [NO_OPERATOR; 4],
        }
    }

    fn reset(&mut self) {
        self.feedback = [0, 0];
        self.feedback_in = 0;
    }

    fn is4op(&self) -> bool {
        self.ops[2] != NO_OPERATOR
    }
}

/// The 4-operator connections, OPM and OPN's eight plus OPL3's four as 8 to
/// 11. Each entry packs the operator 2, 3 and 4 inputs and which of operators
/// 1, 2 and 3 join the final sum; see `output_4op`.
const fn algorithm(
    op2in: u16,
    op3in: u16,
    op4in: u16,
    op1out: u16,
    op2out: u16,
    op3out: u16,
) -> u16 {
    op2in | (op3in << 1) | (op4in << 4) | (op1out << 7) | (op2out << 8) | (op3out << 9)
}

const ALGORITHM_OPS: [u16; 12] = [
    algorithm(1, 2, 3, 0, 0, 0), //  0: O1 -> O2 -> O3 -> O4 -> out (O4)
    algorithm(0, 5, 3, 0, 0, 0), //  1: (O1 + O2) -> O3 -> O4 -> out (O4)
    algorithm(0, 2, 6, 0, 0, 0), //  2: (O1 + (O2 -> O3)) -> O4 -> out (O4)
    algorithm(1, 0, 7, 0, 0, 0), //  3: ((O1 -> O2) + O3) -> O4 -> out (O4)
    algorithm(1, 0, 3, 0, 1, 0), //  4: ((O1 -> O2) + (O3 -> O4)) -> out (O2+O4)
    algorithm(1, 1, 1, 0, 1, 1), //  5: ((O1 -> O2) + (O1 -> O3) + (O1 -> O4)) -> out (O2+O3+O4)
    algorithm(1, 0, 0, 0, 1, 1), //  6: ((O1 -> O2) + O3 + O4) -> out (O2+O3+O4)
    algorithm(0, 0, 0, 1, 1, 1), //  7: (O1 + O2 + O3 + O4) -> out (O1+O2+O3+O4)
    algorithm(1, 2, 3, 0, 0, 0), //  8: O1 -> O2 -> O3 -> O4 -> out (O4)         [same as 0]
    algorithm(0, 2, 3, 1, 0, 0), //  9: (O1 + (O2 -> O3 -> O4)) -> out (O1+O4)   [unique]
    algorithm(1, 0, 3, 0, 1, 0), // 10: ((O1 -> O2) + (O3 -> O4)) -> out (O2+O4) [same as 4]
    algorithm(0, 2, 0, 1, 0, 1), // 11: (O1 + (O2 -> O3) + O4) -> out (O1+O3+O4) [unique]
];

/// The most channels and operators any OPL here has, which sizes the arrays
/// for both revisions.
pub(crate) const MAX_CHANNELS: usize = 18;
pub(crate) const MAX_OPERATORS: usize = 36;

/// The summed outputs of one sample: one on the OPL2, four on the OPL3.
pub(crate) type Output = [i32; 4];

/// A complete OPL FM core.
///
/// `OPL3` selects the YMF262 register map, 18 channels with dynamic 4-operator
/// pairing and four outputs, over the YM3812's nine channels and one output.
#[derive(Clone)]
pub(crate) struct Engine<const OPL3: bool> {
    /// The envelope counter, whose low two bits are a sub-counter.
    env_counter: u32,
    active_channels: u32,
    modified_channels: u32,
    prepare_count: u32,
    pub(crate) regs: Registers<OPL3>,
    channels: [Channel; MAX_CHANNELS],
    operators: [Operator; MAX_OPERATORS],
}

impl<const OPL3: bool> Engine<OPL3> {
    pub(crate) const CHANNELS: usize = Registers::<OPL3>::CHANNELS;
    pub(crate) const OPERATORS: usize = Self::CHANNELS * 2;
    const ALL_CHANNELS: u32 = (1 << Self::CHANNELS) - 1;
    /// Some OPLs modulate with the previous sample rather than the current.
    const MODULATOR_DELAY: bool = !OPL3;

    /// Creates an engine in the state ymfm's constructor leaves it.
    pub(crate) fn new() -> Engine<OPL3> {
        let mut channels = [Channel::new(0); MAX_CHANNELS];
        for (chnum, channel) in channels.iter_mut().enumerate().take(Self::CHANNELS) {
            *channel = Channel::new(Registers::<OPL3>::channel_offset(chnum as u32));
        }
        let mut operators = [Operator::new(0); MAX_OPERATORS];
        for (opnum, operator) in operators.iter_mut().enumerate().take(Self::OPERATORS) {
            *operator = Operator::new(Registers::<OPL3>::operator_offset(opnum as u32));
        }
        let mut engine = Engine {
            env_counter: 0,
            active_channels: Self::ALL_CHANNELS,
            modified_channels: Self::ALL_CHANNELS,
            prepare_count: 0,
            regs: Registers::new(),
            channels,
            operators,
        };
        engine.assign_operators();
        engine
    }

    /// Resets the registers, channels and operators. The LFOs, the noise
    /// generator and the envelope counter run on, as they do in ymfm.
    pub(crate) fn reset(&mut self) {
        self.regs.reset();
        // The mode register has side effects, so it is written explicitly.
        self.write(Registers::<OPL3>::REG_MODE as u16, 0);
        for channel in self.channels.iter_mut() {
            channel.reset();
        }
        for operator in self.operators.iter_mut() {
            operator.reset();
        }
    }

    /// Clocks every channel forward one sample.
    pub(crate) fn clock(&mut self, chanmask: u32) {
        // Prepare when something changed, and every 4096 samples to catch a
        // note that has finished its release. The count only moves when
        // nothing changed, as C's short-circuit `||` leaves it.
        let mut due = self.modified_channels != 0;
        if !due {
            due = self.prepare_count >= 4096;
            self.prepare_count += 1;
        }
        if due {
            if OPL3 {
                self.assign_operators();
            }
            self.active_channels = 0;
            for chnum in 0..Self::CHANNELS {
                if bitfield(chanmask, chnum as u32, 1) != 0 && self.prepare_channel(chnum) {
                    self.active_channels |= 1 << chnum;
                }
            }
            self.modified_channels = 0;
            self.prepare_count = 0;
        }

        // The OPL's envelope clock divider is one.
        self.env_counter = self.env_counter.wrapping_add(4);

        let lfo_raw_pm = self.regs.clock_noise_and_lfo();

        for chnum in 0..Self::CHANNELS {
            if bitfield(chanmask, chnum as u32, 1) != 0 {
                let channel = &mut self.channels[chnum];
                channel.feedback[0] = channel.feedback[1];
                channel.feedback[1] = channel.feedback_in;
                for slot in channel.ops {
                    if slot != NO_OPERATOR {
                        self.operators[slot as usize].clock(
                            &self.regs,
                            self.env_counter,
                            lfo_raw_pm,
                        );
                    }
                }
            }
        }
    }

    /// Prepares a channel's operators and reports whether any still sounds.
    fn prepare_channel(&mut self, chnum: usize) -> bool {
        let mut active = false;
        for slot in self.channels[chnum].ops {
            if slot != NO_OPERATOR {
                let operator = &mut self.operators[slot as usize];
                operator.prepare(&self.regs);
                active |= operator.is_active();
            }
        }
        active
    }

    /// Sums the channels in `chanmask` into `output`.
    pub(crate) fn output(&mut self, output: &mut Output, rshift: u32, clipmax: i32, chanmask: u32) {
        let chanmask = chanmask & self.active_channels;

        if self.regs.rhythm_enable() != 0 {
            let op13phase = self.operators[13].phase();
            let op17phase = self.operators[17].phase();
            let phase_select = (bitfield(op13phase, 2, 1) ^ bitfield(op13phase, 7, 1))
                | bitfield(op13phase, 3, 1)
                | (bitfield(op17phase, 5, 1) ^ bitfield(op17phase, 3, 1));
            for chnum in 0..Self::CHANNELS {
                if bitfield(chanmask, chnum as u32, 1) == 0 {
                    continue;
                }
                match chnum {
                    6 => self.output_rhythm_ch6(chnum, output, rshift),
                    7 => self.output_rhythm_ch7(chnum, phase_select, output, rshift, clipmax),
                    8 => self.output_rhythm_ch8(chnum, phase_select, output, rshift, clipmax),
                    _ if self.channels[chnum].is4op() => {
                        self.output_4op(chnum, output, rshift, clipmax)
                    }
                    _ => self.output_2op(chnum, output, rshift, clipmax),
                }
            }
        } else {
            for chnum in 0..Self::CHANNELS {
                if bitfield(chanmask, chnum as u32, 1) == 0 {
                    continue;
                }
                if self.channels[chnum].is4op() {
                    self.output_4op(chnum, output, rshift, clipmax);
                } else {
                    self.output_2op(chnum, output, rshift, clipmax);
                }
            }
        }
    }

    /// Writes one register, applying a key on or off it carries.
    pub(crate) fn write(&mut self, regnum: u16, data: u8) {
        if u32::from(regnum) == Registers::<OPL3>::REG_MODE {
            // ymfm routes this through the interface to order it against the
            // timers, which this translation does not have.
            self.modified_channels = Self::ALL_CHANNELS;
            self.regs.write(regnum, data);
            return;
        }

        self.modified_channels = Self::ALL_CHANNELS;

        if let Some((keyon_channel, keyon_opmask)) = self.regs.write(regnum, data) {
            if (keyon_channel as usize) < Self::CHANNELS {
                self.channel_keyonoff(keyon_channel as usize, keyon_opmask, KEYON_NORMAL);
            } else if keyon_channel == RHYTHM_CHANNEL {
                self.channel_keyonoff(
                    6,
                    if bitfield(keyon_opmask, 4, 1) != 0 {
                        3
                    } else {
                        0
                    },
                    KEYON_RHYTHM,
                );
                self.channel_keyonoff(
                    7,
                    bitfield(keyon_opmask, 0, 1) | (bitfield(keyon_opmask, 3, 1) << 1),
                    KEYON_RHYTHM,
                );
                self.channel_keyonoff(
                    8,
                    bitfield(keyon_opmask, 2, 1) | (bitfield(keyon_opmask, 1, 1) << 1),
                    KEYON_RHYTHM,
                );
            }
        }
    }

    fn channel_keyonoff(&mut self, chnum: usize, states: u32, kind: u32) {
        for (opnum, slot) in self.channels[chnum].ops.into_iter().enumerate() {
            if slot != NO_OPERATOR {
                self.operators[slot as usize].keyonoff(bitfield(states, opnum as u32, 1), kind);
            }
        }
    }

    /// Maps operators onto channels from the registers.
    fn assign_operators(&mut self) {
        let map = self.regs.operator_map();
        for (channel, packed) in self.channels.iter_mut().zip(map).take(Self::CHANNELS) {
            for index in 0..4 {
                let opnum = bitfield(packed, 8 * index, 8) as u8;
                channel.ops[index as usize] = opnum;
                if opnum != NO_OPERATOR {
                    self.operators[opnum as usize].choffs = channel.choffs;
                }
            }
        }
    }

    /// Returns operator 1's input from its own feedback.
    fn feedback_mod(&self, chnum: usize) -> i32 {
        let channel = &self.channels[chnum];
        let feedback = self.regs.ch_feedback(channel.choffs);
        if feedback == 0 {
            return 0;
        }
        (i32::from(channel.feedback[0]) + i32::from(channel.feedback[1])) >> (10 - feedback)
    }

    /// Computes operator 1 with its feedback, stores the next feedback input,
    /// and returns the value as the sixteen bits ymfm stores it in.
    fn operator1(&mut self, chnum: usize, am_offset: u32) -> i32 {
        let opmod = self.feedback_mod(chnum);
        let op = &self.operators[self.channels[chnum].ops[0] as usize];
        let value = op.compute_volume(&self.regs, op.phase().wrapping_add(opmod as u32), am_offset);
        // `op1value = m_feedback_in = ...` reads back the int16_t.
        self.channels[chnum].feedback_in = value as i16;
        i32::from(value as i16)
    }

    fn volume(&self, slot: u8, phase: u32, am_offset: u32) -> i32 {
        self.operators[slot as usize].compute_volume(&self.regs, phase, am_offset)
    }

    fn op_phase(&self, slot: u8) -> u32 {
        self.operators[slot as usize].phase()
    }

    fn add_to_output(&self, choffs: u32, output: &mut Output, value: i32) {
        if !OPL3 {
            output[0] += value;
            return;
        }
        if self.regs.ch_output_0(choffs) != 0 {
            output[0] += value;
        }
        if self.regs.ch_output_1(choffs) != 0 {
            output[1] += value;
        }
        if self.regs.ch_output_2(choffs) != 0 {
            output[2] += value;
        }
        if self.regs.ch_output_3(choffs) != 0 {
            output[3] += value;
        }
    }

    fn output_2op(&mut self, chnum: usize, output: &mut Output, rshift: u32, clipmax: i32) {
        let choffs = self.channels[chnum].choffs;
        let ops = self.channels[chnum].ops;
        let am_offset = self.regs.lfo_am_offset();
        let op1value = self.operator1(chnum, am_offset);

        // Skip the rest when no output is enabled; the feedback is done.
        if self.regs.ch_output_any(choffs) == 0 {
            return;
        }
        let modulator = if Self::MODULATOR_DELAY {
            i32::from(self.channels[chnum].feedback[1])
        } else {
            op1value
        };
        // Two-operator connections: 0 is O1 -> O2 -> out, 1 is (O1 + O2) -> out.
        let result = if bitfield(self.regs.ch_algorithm(choffs), 0, 1) == 0 {
            let opmod = modulator >> 1;
            self.volume(
                ops[1],
                self.op_phase(ops[1]).wrapping_add(opmod as u32),
                am_offset,
            ) >> rshift
        } else {
            let mut result = modulator >> rshift;
            result += self.volume(ops[1], self.op_phase(ops[1]), am_offset) >> rshift;
            clamp(result, -clipmax - 1, clipmax)
        };
        self.add_to_output(choffs, output, result);
    }

    fn output_4op(&mut self, chnum: usize, output: &mut Output, rshift: u32, clipmax: i32) {
        let choffs = self.channels[chnum].choffs;
        let ops = self.channels[chnum].ops;
        let am_offset = self.regs.lfo_am_offset();
        let op1value = self.operator1(chnum, am_offset);

        if self.regs.ch_output_any(choffs) == 0 {
            return;
        }

        // Operators are computed in order, each taking its input from this
        // table as it fills: 0 is zero, 1 to 3 are O1 to O3, 4 is unused, and
        // 5 to 7 are O1+O2, O1+O3 and O2+O3. ymfm holds it as int16_t, so the
        // sums truncate.
        let algorithm_ops = u32::from(ALGORITHM_OPS[self.regs.ch_algorithm(choffs) as usize]);
        let mut opout = [0i16; 8];
        opout[1] = op1value as i16;

        let opmod = i32::from(opout[bitfield(algorithm_ops, 0, 1) as usize]) >> 1;
        opout[2] = self.volume(
            ops[1],
            self.op_phase(ops[1]).wrapping_add(opmod as u32),
            am_offset,
        ) as i16;
        opout[5] = (i32::from(opout[1]) + i32::from(opout[2])) as i16;

        let opmod = i32::from(opout[bitfield(algorithm_ops, 1, 3) as usize]) >> 1;
        opout[3] = self.volume(
            ops[2],
            self.op_phase(ops[2]).wrapping_add(opmod as u32),
            am_offset,
        ) as i16;
        opout[6] = (i32::from(opout[1]) + i32::from(opout[3])) as i16;
        opout[7] = (i32::from(opout[2]) + i32::from(opout[3])) as i16;

        let opmod = i32::from(opout[bitfield(algorithm_ops, 4, 3) as usize]) >> 1;
        let mut result = self.volume(
            ops[3],
            self.op_phase(ops[3]).wrapping_add(opmod as u32),
            am_offset,
        );
        result >>= rshift;

        let clipmin = -clipmax - 1;
        if bitfield(algorithm_ops, 7, 1) != 0 {
            result = clamp(result + (i32::from(opout[1]) >> rshift), clipmin, clipmax);
        }
        if bitfield(algorithm_ops, 8, 1) != 0 {
            result = clamp(result + (i32::from(opout[2]) >> rshift), clipmin, clipmax);
        }
        if bitfield(algorithm_ops, 9, 1) != 0 {
            result = clamp(result + (i32::from(opout[3]) >> rshift), clipmin, clipmax);
        }
        self.add_to_output(choffs, output, result);
    }

    /// Channel 6 in rhythm mode: the bass drum, a nearly normal 2-operator
    /// voice whose first operator is ignored rather than added when the
    /// connection is 1.
    fn output_rhythm_ch6(&mut self, chnum: usize, output: &mut Output, rshift: u32) {
        let choffs = self.channels[chnum].choffs;
        let ops = self.channels[chnum].ops;
        let am_offset = self.regs.lfo_am_offset();
        let opout1 = self.operator1(chnum, am_offset);
        let opmod = if bitfield(self.regs.ch_algorithm(choffs), 0, 1) != 0 {
            0
        } else {
            opout1 >> 1
        };
        let result = self.volume(
            ops[1],
            self.op_phase(ops[1]).wrapping_add(opmod as u32),
            am_offset,
        ) >> rshift;
        self.add_to_output(choffs, output, result * 2);
    }

    /// Channel 7 in rhythm mode: the high hat on operator 13's envelope and
    /// the snare drum on operator 16's, both phased from noise and operator 13.
    fn output_rhythm_ch7(
        &mut self,
        chnum: usize,
        phase_select: u32,
        output: &mut Output,
        rshift: u32,
        clipmax: i32,
    ) {
        let choffs = self.channels[chnum].choffs;
        let ops = self.channels[chnum].ops;
        let am_offset = self.regs.lfo_am_offset();
        let noise_state = bitfield(self.regs.noise_state(), 0, 1);

        let phase = (phase_select << 9) | (0xd0 >> (2 * (noise_state ^ phase_select)));
        let mut result = self.volume(ops[0], phase, am_offset) >> rshift;

        let op13phase = self.op_phase(ops[0]);
        let phase = (0x100 << bitfield(op13phase, 8, 1)) ^ (noise_state << 8);
        result += self.volume(ops[1], phase, am_offset) >> rshift;
        let result = clamp(result, -clipmax - 1, clipmax);
        self.add_to_output(choffs, output, result * 2);
    }

    /// Channel 8 in rhythm mode: the tom tom, one plain operator, and the top
    /// cymbal on operator 17's envelope, phased from the phase select.
    fn output_rhythm_ch8(
        &mut self,
        chnum: usize,
        phase_select: u32,
        output: &mut Output,
        rshift: u32,
        clipmax: i32,
    ) {
        let choffs = self.channels[chnum].choffs;
        let ops = self.channels[chnum].ops;
        let am_offset = self.regs.lfo_am_offset();

        let mut result = self.volume(ops[0], self.op_phase(ops[0]), am_offset) >> rshift;
        let phase = 0x100 | (phase_select << 9);
        result += self.volume(ops[1], phase, am_offset) >> rshift;
        let result = clamp(result, -clipmax - 1, clipmax);
        self.add_to_output(choffs, output, result * 2);
    }
}
