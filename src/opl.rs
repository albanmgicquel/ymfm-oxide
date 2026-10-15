// This file is a Rust translation of ymfm's `ymfm_opl.h` and `ymfm_opl.cpp`
// at commit 81aec25 (https://github.com/aaronsgiles/ymfm), with changes: the
// register class template over the revision is a const generic; its waveform
// tables are computed once at compile time and shared rather than held per
// chip; a register write returns its key-on target rather than writing
// through output parameters; key-on logging and save states are gone; a new
// chip is reset, where ymfm leaves its register file undefined until
// `reset()`; and a chip owns its interface. So far it covers the YM3526
// (OPL), the YM3812 (OPL2) and the YMF262 (OPL3); the Y8950, the YMF289B,
// the YMF278B and the OPLL family are still to come. Every computation that
// reaches a sample, a status bit or a timer is kept as ymfm writes it.
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

//! The OPL family: the YM3526 (OPL), YM3812 (OPL2) and YMF262 (OPL3).
//!
//! The register map, as ymfm documents it:
//!
//! ```text
//! System-wide registers:
//!      01 xxxxxxxx Test register
//!         --x----- Enable OPL compatibility mode [OPL2 only] (0 = enable)
//!      02 xxxxxxxx Timer A value (4 * OPN)
//!      03 xxxxxxxx Timer B value
//!      04 x------- RST
//!         -x------ Mask timer A
//!         --x----- Mask timer B
//!         ------x- Load timer B
//!         -------x Load timer A
//!      08 x------- CSM mode [OPL/OPL2 only]
//!         -x------ Note select
//!      BD x------- AM depth
//!         -x------ PM depth
//!         --x----- Rhythm enable
//!         ---x---- Bass drum key on
//!         ----x--- Snare drum key on
//!         -----x-- Tom key on
//!         ------x- Top cymbal key on
//!         -------x High hat key on
//!     101 --xxxxxx Test register 2 [OPL3 only]
//!     104 --x----- Channel 6 4-operator mode [OPL3 only]
//!         ---x---- Channel 5 4-operator mode [OPL3 only]
//!         ----x--- Channel 4 4-operator mode [OPL3 only]
//!         -----x-- Channel 3 4-operator mode [OPL3 only]
//!         ------x- Channel 2 4-operator mode [OPL3 only]
//!         -------x Channel 1 4-operator mode [OPL3 only]
//!     105 -------x New [OPL3 only]
//!         ------x- New2 [OPL4 only]
//!
//! Per-channel registers (channel in address bits 0-3)
//! Note that all these apply to address+100 as well on OPL3+
//!   A0-A8 xxxxxxxx F-number (low 8 bits)
//!   B0-B8 --x----- Key on
//!         ---xxx-- Block (octave, 0-7)
//!         ------xx F-number (high two bits)
//!   C0-C8 x------- CHD output (to DO0 pin) [OPL3+ only]
//!         -x------ CHC output (to DO0 pin) [OPL3+ only]
//!         --x----- CHB output (mixed right, to DO2 pin) [OPL3+ only]
//!         ---x---- CHA output (mixed left, to DO2 pin) [OPL3+ only]
//!         ----xxx- Feedback level for operator 1 (0-7)
//!         -------x Operator connection algorithm
//!
//! Per-operator registers (operator in bits 0-5)
//! Note that all these apply to address+100 as well on OPL3+
//!   20-35 x------- AM enable
//!         -x------ PM enable (VIB)
//!         --x----- EG type
//!         ---x---- Key scale rate
//!         ----xxxx Multiple value (0-15)
//!   40-55 xx------ Key scale level (0-3)
//!         --xxxxxx Total level (0-63)
//!   60-75 xxxx---- Attack rate (0-15)
//!         ----xxxx Decay rate (0-15)
//!   80-95 xxxx---- Sustain level (0-15)
//!         ----xxxx Release rate (0-15)
//!   E0-F5 ------xx Wave select (0-3) [OPL2 only]
//!         -----xxx Wave select (0-7) [OPL3+ only]
//! ```

use crate::fm::{
    abs_sin_attenuation, effective_rate, operator_list, FmEngine, FmRegisters, OpdataCache,
    MAX_CHANNELS, RHYTHM_CHANNEL, WAVEFORM_LENGTH,
};
use crate::ymfm::{
    bitfield, Interface, NullInterface, Output, EG_ATTACK, EG_DECAY, EG_DEPRESS, EG_RELEASE,
    EG_SUSTAIN,
};

/// Converts an OPL concatenated block (3 bits) and the top four bits of the
/// fnum into an attenuation offset, for 6dB/octave in 0.75dB units (matching
/// the total level LSB).
fn opl_key_scale_atten(block: u32, fnum_4msb: u32) -> u32 {
    // this table uses the top 4 bits of FNUM and are the maximal values (for
    // when block == 7). Values for other blocks can be computed by
    // subtracting 8 for each block below 7.
    const FNUM_TO_ATTEN: [u8; 16] = [
        0, 24, 32, 37, 40, 43, 45, 47, 48, 50, 51, 52, 53, 54, 55, 56,
    ];
    let result = i32::from(FNUM_TO_ATTEN[fnum_4msb as usize]) - 8 * (block ^ 7) as i32;
    result.max(0) as u32
}

//*********************************************************
//  OPL REGISTERS
//*********************************************************

/// The eight OPL waveforms, of which the OPL2 offers the first four and the
/// OPL the first. ymfm builds them in each register set's constructor.
static OPL_WAVEFORMS: [[u16; WAVEFORM_LENGTH]; 8] = {
    let mut table = [[0u16; WAVEFORM_LENGTH]; 8];
    let mut index = 0;
    while index < WAVEFORM_LENGTH {
        let i = index as u32;
        table[0][index] = (abs_sin_attenuation(i) | (bitfield(i, 9, 1) << 15)) as u16;
        index += 1;
    }
    let zeroval = table[0][0];
    let mut index = 0;
    while index < WAVEFORM_LENGTH {
        let i = index as u32;
        let negative = bitfield(i, 9, 1) != 0;
        table[1][index] = if negative { zeroval } else { table[0][index] };
        table[2][index] = table[0][index] & 0x7fff;
        table[3][index] = if bitfield(i, 8, 1) != 0 {
            zeroval
        } else {
            table[0][index] & 0x7fff
        };
        table[4][index] = if negative {
            zeroval
        } else {
            table[0][(index * 2) % WAVEFORM_LENGTH]
        };
        table[5][index] = if negative {
            zeroval
        } else {
            table[0][(index * 2) & 0x1ff]
        };
        table[6][index] = (bitfield(i, 9, 1) << 15) as u16;
        // ymfm stores `(index ^ 0x13ff) << 3` in a uint16_t, which truncates.
        table[7][index] = ((if negative { i ^ 0x13ff } else { i }) << 3) as u16;
        index += 1;
    }
    table
};

/// The register set of the OPL family, for revision 1 (OPL), 2 (OPL2), 3
/// (OPL3) or 4 (OPL4), with its LFOs and noise generator.
#[derive(Clone)]
pub(crate) struct OplRegisters<const REVISION: u32> {
    /// The LFO AM counter.
    lfo_am_counter: u16,
    /// The LFO PM counter.
    lfo_pm_counter: u16,
    /// The noise LFSR state.
    noise_lfsr: u32,
    /// The current LFO AM value.
    lfo_am: u8,
    /// The register data, of which the OPL and OPL2 use the first 0x100.
    regdata: [u8; 0x200],
}

impl<const REVISION: u32> OplRegisters<REVISION> {
    const IS_OPL2: bool = REVISION == 2;
    const IS_OPL2_PLUS: bool = REVISION >= 2;
    const IS_OPL3_PLUS: bool = REVISION >= 3;
    const IS_OPL4_PLUS: bool = REVISION >= 4;

    /// The number of waveforms offered.
    const WAVEFORMS: u32 = if Self::IS_OPL3_PLUS {
        8
    } else if Self::IS_OPL2_PLUS {
        4
    } else {
        1
    };
    /// The number of 8-bit registers.
    pub(crate) const REGISTERS: usize = if Self::IS_OPL3_PLUS { 0x200 } else { 0x100 };

    /// Returns a bitfield extracted from a byte.
    fn byte(&self, offset: u32, start: u32, count: u32, extra_offset: u32) -> u32 {
        bitfield(
            u32::from(self.regdata[(offset + extra_offset) as usize]),
            start,
            count,
        )
    }

    /// Returns a bitfield extracted from a pair of bytes, MSBs listed first.
    #[allow(clippy::too_many_arguments)]
    fn word(
        &self,
        offset1: u32,
        start1: u32,
        count1: u32,
        offset2: u32,
        start2: u32,
        count2: u32,
        extra_offset: u32,
    ) -> u32 {
        (self.byte(offset1, start1, count1, extra_offset) << count2)
            | self.byte(offset2, start2, count2, extra_offset)
    }

    /// Returns the register data, for a diagnostic.
    pub(crate) fn data(&self) -> &[u8] {
        &self.regdata[..Self::REGISTERS]
    }

    // system-wide registers
    #[allow(dead_code)]
    fn test(&self) -> u32 {
        self.byte(0x01, 0, 8, 0)
    }
    fn waveform_enable(&self) -> u32 {
        if Self::IS_OPL2 {
            self.byte(0x01, 5, 1, 0)
        } else if Self::IS_OPL3_PLUS {
            1
        } else {
            0
        }
    }
    fn note_select(&self) -> u32 {
        self.byte(0x08, 6, 1, 0)
    }
    fn lfo_am_depth(&self) -> u32 {
        self.byte(0xbd, 7, 1, 0)
    }
    fn lfo_pm_depth(&self) -> u32 {
        self.byte(0xbd, 6, 1, 0)
    }
    /// Returns the NEW bit of the OPL3, which gates its second bank.
    pub(crate) fn newflag(&self) -> u32 {
        if Self::IS_OPL3_PLUS {
            self.byte(0x105, 0, 1, 0)
        } else {
            0
        }
    }
    #[allow(dead_code)] // used by the OPL4, which is not ported yet
    pub(crate) fn new2flag(&self) -> u32 {
        if Self::IS_OPL4_PLUS {
            self.byte(0x105, 1, 1, 0)
        } else {
            0
        }
    }
    fn fourop_enable(&self) -> u32 {
        if Self::IS_OPL3_PLUS {
            self.byte(0x104, 0, 6, 0)
        } else {
            0
        }
    }

    // per-channel registers
    fn ch_block_freq(&self, choffs: u32) -> u32 {
        self.word(0xb0, 0, 5, 0xa0, 0, 8, choffs)
    }

    // per-operator registers
    fn op_lfo_pm_enable(&self, opoffs: u32) -> u32 {
        self.byte(0x20, 6, 1, opoffs)
    }
    fn op_eg_sustain(&self, opoffs: u32) -> u32 {
        self.byte(0x20, 5, 1, opoffs)
    }
    fn op_ksr(&self, opoffs: u32) -> u32 {
        self.byte(0x20, 4, 1, opoffs)
    }
    fn op_multiple(&self, opoffs: u32) -> u32 {
        self.byte(0x20, 0, 4, opoffs)
    }
    fn op_ksl(&self, opoffs: u32) -> u32 {
        let temp = self.byte(0x40, 6, 2, opoffs);
        bitfield(temp, 1, 1) | (bitfield(temp, 0, 1) << 1)
    }
    fn op_total_level(&self, opoffs: u32) -> u32 {
        self.byte(0x40, 0, 6, opoffs)
    }
    fn op_attack_rate(&self, opoffs: u32) -> u32 {
        self.byte(0x60, 4, 4, opoffs)
    }
    fn op_decay_rate(&self, opoffs: u32) -> u32 {
        self.byte(0x60, 0, 4, opoffs)
    }
    fn op_sustain_level(&self, opoffs: u32) -> u32 {
        self.byte(0x80, 4, 4, opoffs)
    }
    fn op_release_rate(&self, opoffs: u32) -> u32 {
        self.byte(0x80, 0, 4, opoffs)
    }
    fn op_waveform(&self, opoffs: u32) -> u32 {
        if self.waveform_enable() != 0 {
            let bits = if self.newflag() != 0 { 3 } else { 2 };
            self.byte(0xe0, 0, bits, opoffs)
        } else {
            0
        }
    }
}

/// Clocks the noise generator and both LFOs, and returns the PM value; ymfm
/// shares this between the OPL and OPLL register sets.
fn opl_clock_noise_and_lfo(
    noise_lfsr: &mut u32,
    lfo_am_counter: &mut u16,
    lfo_pm_counter: &mut u16,
    lfo_am: &mut u8,
    am_depth: u32,
    pm_depth: u32,
) -> i32 {
    // OPL has a 23-bit noise generator for the rhythm section, running at a
    // constant rate, used only for percussion input
    *noise_lfsr <<= 1;
    *noise_lfsr |= bitfield(*noise_lfsr, 23, 1)
        ^ bitfield(*noise_lfsr, 9, 1)
        ^ bitfield(*noise_lfsr, 8, 1)
        ^ bitfield(*noise_lfsr, 1, 1);

    // OPL has two fixed-frequency LFOs, one for AM, one for PM

    // the AM LFO has 210*64 steps; at a nominal 50kHz output, this equates
    // to a period of 50000/(210*64) = 3.72Hz
    let am_counter = u32::from(*lfo_am_counter);
    *lfo_am_counter = lfo_am_counter.wrapping_add(1);
    if am_counter >= 210 * 64 - 1 {
        *lfo_am_counter = 0;
    }

    // low 8 bits are fractional; depth 0 is divided by 2, while depth 1 is
    // times 2
    let shift = 9 - 2 * am_depth;

    // AM value is the upper bits of the value, inverted across the midpoint
    // to produce a triangle
    let triangle = if am_counter < 105 * 64 {
        am_counter
    } else {
        210 * 64 + 63 - am_counter
    };
    *lfo_am = (triangle >> shift) as u8;

    // the PM LFO has 8192 steps, or a nominal period of 6.1Hz
    let pm_counter = u32::from(*lfo_pm_counter);
    *lfo_pm_counter = lfo_pm_counter.wrapping_add(1);

    // PM LFO is broken into 8 chunks, each lasting 1024 steps; the PM value
    // depends on the upper bits of FNUM, so this value is a fraction and
    // sign to apply to that value, as a 1.3 value
    const PM_SCALE: [i8; 8] = [8, 4, 0, -4, -8, -4, 0, 4];
    i32::from(PM_SCALE[bitfield(pm_counter, 10, 3) as usize]) >> (pm_depth ^ 1)
}

/// Computes the phase step from the block and frequency number, the x.1
/// multiple and the PM LFO; ymfm shares this between the OPL and OPLL.
fn opl_compute_phase_step(block_freq: u32, multiple: u32, lfo_raw_pm: i32) -> u32 {
    // OPL phase calculation has no detuning, but uses FNUMs like the OPN
    // version, and computes PM a bit differently

    // extract frequency number as a 12-bit fraction
    let mut fnum = bitfield(block_freq, 0, 10) << 2;

    // apply the phase adjustment based on the upper 3 bits of FNUM and the PM
    // depth parameters; ymfm multiplies an int32_t by a uint32_t, which C
    // does unsigned, and keeps twelve bits, so wrapping unsigned arithmetic
    // gives the same bits
    fnum = fnum.wrapping_add((lfo_raw_pm as u32).wrapping_mul(bitfield(block_freq, 7, 3)) >> 1);

    // keep fnum to 12 bits
    fnum &= 0xfff;

    // apply block shift to compute phase step
    let block = bitfield(block_freq, 10, 3);
    let phase_step = (fnum << block) >> 2;

    // apply frequency multiplier (which is cached as an x.1 value)
    (phase_step * multiple) >> 1
}

impl<const REVISION: u32> FmRegisters for OplRegisters<REVISION> {
    const OUTPUTS: usize = if Self::IS_OPL3_PLUS { 4 } else { 1 };
    const CHANNELS: usize = if Self::IS_OPL3_PLUS { 18 } else { 9 };
    const ALL_CHANNELS: u32 = (1 << Self::CHANNELS) - 1;
    const OPERATORS: usize = Self::CHANNELS * 2;
    const DEFAULT_PRESCALE: u32 = if Self::IS_OPL4_PLUS {
        19
    } else if Self::IS_OPL3_PLUS {
        8
    } else {
        4
    };
    const EG_CLOCK_DIVIDER: u32 = 1;
    const CSM_TRIGGER_MASK: u32 = Self::ALL_CHANNELS;
    const REG_MODE: u32 = 0x04;
    const STATUS_TIMERA: u8 = 0x40;
    const STATUS_TIMERB: u8 = 0x20;
    const STATUS_BUSY: u8 = 0;
    const STATUS_IRQ: u8 = 0x80;
    const DYNAMIC_OPS: bool = Self::IS_OPL3_PLUS;
    const MODULATOR_DELAY: bool = !Self::IS_OPL3_PLUS;

    fn new() -> Self {
        // ymfm leaves the register file undefined until reset, except for
        // 0x104 on the OPL3, which the constructor's operator map reads
        OplRegisters {
            lfo_am_counter: 0,
            lfo_pm_counter: 0,
            noise_lfsr: 1,
            lfo_am: 0,
            regdata: [0; 0x200],
        }
    }

    fn reset(&mut self) {
        self.regdata = [0; 0x200];
    }

    fn channel_offset(chnum: u32) -> u32 {
        if !Self::IS_OPL3_PLUS {
            chnum
        } else {
            (chnum % 9) + 0x100 * (chnum / 9)
        }
    }

    fn operator_offset(opnum: u32) -> u32 {
        if !Self::IS_OPL3_PLUS {
            opnum + 2 * (opnum / 6)
        } else {
            (opnum % 18) + 2 * ((opnum % 18) / 6) + 0x100 * (opnum / 18)
        }
    }

    fn operator_map(&self, dest: &mut [u32; MAX_CHANNELS]) {
        const fn pair(o1: u8, o2: u8) -> u32 {
            operator_list(o1, o2, 0xff, 0xff)
        }
        const NONE: u32 = operator_list(0xff, 0xff, 0xff, 0xff);
        if REVISION <= 2 {
            // OPL/OPL2 has a fixed map, all 2 operators
            dest[..9].copy_from_slice(&[
                pair(0, 3),   // Channel 0 operators
                pair(1, 4),   // Channel 1 operators
                pair(2, 5),   // Channel 2 operators
                pair(6, 9),   // Channel 3 operators
                pair(7, 10),  // Channel 4 operators
                pair(8, 11),  // Channel 5 operators
                pair(12, 15), // Channel 6 operators
                pair(13, 16), // Channel 7 operators
                pair(14, 17), // Channel 8 operators
            ]);
        } else {
            // OPL3/OPL4 can be configured for 2 or 4 operators
            let fourop = self.fourop_enable();
            let four = |bit: u32| bitfield(fourop, bit, 1) != 0;
            let pick = |bit: u32, yes: u32, no: u32| if four(bit) { yes } else { no };

            dest[0] = pick(0, operator_list(0, 3, 6, 9), pair(0, 3));
            dest[1] = pick(1, operator_list(1, 4, 7, 10), pair(1, 4));
            dest[2] = pick(2, operator_list(2, 5, 8, 11), pair(2, 5));
            dest[3] = pick(0, NONE, pair(6, 9));
            dest[4] = pick(1, NONE, pair(7, 10));
            dest[5] = pick(2, NONE, pair(8, 11));
            dest[6] = pair(12, 15);
            dest[7] = pair(13, 16);
            dest[8] = pair(14, 17);

            dest[9] = pick(3, operator_list(18, 21, 24, 27), pair(18, 21));
            dest[10] = pick(4, operator_list(19, 22, 25, 28), pair(19, 22));
            dest[11] = pick(5, operator_list(20, 23, 26, 29), pair(20, 23));
            dest[12] = pick(3, NONE, pair(24, 27));
            dest[13] = pick(4, NONE, pair(25, 28));
            dest[14] = pick(5, NONE, pair(26, 29));
            dest[15] = pair(30, 33);
            dest[16] = pair(31, 34);
            dest[17] = pair(32, 35);
        }
    }

    fn write(&mut self, index: u16, data: u8) -> Option<(u32, u32)> {
        let index = u32::from(index);

        // writes to the mode register with high bit set ignore the low bits
        if index == Self::REG_MODE && bitfield(u32::from(data), 7, 1) != 0 {
            self.regdata[index as usize] |= 0x80;
        } else {
            self.regdata[index as usize] = data;
        }
        let data = u32::from(data);

        // handle writes to the rhythm keyons
        if index == 0xbd {
            let opmask = if bitfield(data, 5, 1) != 0 {
                bitfield(data, 0, 5)
            } else {
                0
            };
            return Some((RHYTHM_CHANNEL, opmask));
        }

        // handle writes to the channel keyons
        if (index & 0xf0) == 0xb0 {
            let mut channel = index & 0x0f;
            if channel < 9 {
                if Self::IS_OPL3_PLUS {
                    channel += 9 * bitfield(index, 8, 1);
                }
                let opmask = if bitfield(data, 5, 1) != 0 { 15 } else { 0 };
                return Some((channel, opmask));
            }
        }
        None
    }

    fn clock_noise_and_lfo(&mut self) -> i32 {
        let (am_depth, pm_depth) = (self.lfo_am_depth(), self.lfo_pm_depth());
        opl_clock_noise_and_lfo(
            &mut self.noise_lfsr,
            &mut self.lfo_am_counter,
            &mut self.lfo_pm_counter,
            &mut self.lfo_am,
            am_depth,
            pm_depth,
        )
    }

    fn lfo_am_offset(&self, _choffs: u32) -> u32 {
        // on OPL this is just a fixed value
        u32::from(self.lfo_am)
    }

    fn noise_state(&self) -> u32 {
        self.noise_lfsr >> 23
    }

    fn cache_operator_data(&mut self, choffs: u32, opoffs: u32, cache: &mut OpdataCache) {
        // set up the easy stuff
        cache.waveform = (self.op_waveform(opoffs) % Self::WAVEFORMS) as u8;

        // get frequency from the channel
        let block_freq = self.ch_block_freq(choffs);
        cache.block_freq = block_freq;

        // compute the keycode: block_freq is:
        //
        //     111  |
        //     21098|76543210
        //     BBBFF|FFFFFFFF
        //     ^^^??
        //
        // the 4-bit keycode uses the top 3 bits plus one of the next two bits
        let mut keycode = bitfield(block_freq, 10, 3) << 1;

        // lowest bit is determined by note_select(); note that it is actually
        // reversed from what the manual says, however
        keycode |= bitfield(block_freq, 9 - self.note_select(), 1);

        // no detune adjustment on OPL
        cache.detune = 0;

        // multiple value, as an x.1 value (0 means 0.5); replace the low bit
        // with a table lookup to give 0,1,2,3,4,5,6,7,8,9,10,10,12,12,15,15
        let multiple = self.op_multiple(opoffs);
        cache.multiple = ((multiple & 0xe) | bitfield(0xc2aa, multiple, 1)) * 2;
        if cache.multiple == 0 {
            cache.multiple = 1;
        }

        // phase step, or PHASE_STEP_DYNAMIC if PM is active; this depends on
        // block_freq, detune, and multiple, so compute it after we've done
        // those
        cache.phase_step = if self.op_lfo_pm_enable(opoffs) == 0 {
            self.compute_phase_step(choffs, opoffs, cache, 0)
        } else {
            OpdataCache::PHASE_STEP_DYNAMIC
        };

        // total level, scaled by 8
        cache.total_level = self.op_total_level(opoffs) << 3;

        // pre-add key scale level
        let ksl = self.op_ksl(opoffs);
        if ksl != 0 {
            cache.total_level +=
                opl_key_scale_atten(bitfield(block_freq, 10, 3), bitfield(block_freq, 6, 4)) << ksl;
        }

        // 4-bit sustain level, but 15 means 31 so effectively 5 bits
        cache.eg_sustain = self.op_sustain_level(opoffs);
        cache.eg_sustain |= (cache.eg_sustain + 1) & 0x10;
        cache.eg_sustain <<= 5;

        // determine KSR adjustment for enevlope rates
        let ksrval = keycode >> (2 * (self.op_ksr(opoffs) ^ 1));
        cache.eg_rate[EG_ATTACK] = effective_rate(self.op_attack_rate(opoffs) * 4, ksrval);
        cache.eg_rate[EG_DECAY] = effective_rate(self.op_decay_rate(opoffs) * 4, ksrval);
        cache.eg_rate[EG_SUSTAIN] = if self.op_eg_sustain(opoffs) != 0 {
            0
        } else {
            effective_rate(self.op_release_rate(opoffs) * 4, ksrval)
        };
        cache.eg_rate[EG_RELEASE] = effective_rate(self.op_release_rate(opoffs) * 4, ksrval);
        cache.eg_rate[EG_DEPRESS] = 0x3f;
    }

    fn compute_phase_step(
        &mut self,
        _choffs: u32,
        opoffs: u32,
        cache: &OpdataCache,
        lfo_raw_pm: i32,
    ) -> u32 {
        let pm = if self.op_lfo_pm_enable(opoffs) != 0 {
            lfo_raw_pm
        } else {
            0
        };
        opl_compute_phase_step(cache.block_freq, cache.multiple, pm)
    }

    fn waveform(&self, index: u8) -> &[u16; WAVEFORM_LENGTH] {
        &OPL_WAVEFORMS[index as usize]
    }

    fn timer_a_value(&self) -> u32 {
        self.byte(0x02, 0, 8, 0) * 4 // 8->10 bits
    }
    fn timer_b_value(&self) -> u32 {
        self.byte(0x03, 0, 8, 0)
    }
    fn status_mask(&self) -> u32 {
        self.byte(0x04, 0, 8, 0) & 0x78
    }
    fn irq_reset(&self) -> u32 {
        self.byte(0x04, 7, 1, 0)
    }
    fn reset_timer_b(&self) -> u32 {
        self.byte(0x04, 7, 1, 0) | self.byte(0x04, 5, 1, 0)
    }
    fn reset_timer_a(&self) -> u32 {
        self.byte(0x04, 7, 1, 0) | self.byte(0x04, 6, 1, 0)
    }
    fn enable_timer_b(&self) -> u32 {
        1
    }
    fn enable_timer_a(&self) -> u32 {
        1
    }
    fn load_timer_b(&self) -> u32 {
        self.byte(0x04, 1, 1, 0)
    }
    fn load_timer_a(&self) -> u32 {
        self.byte(0x04, 0, 1, 0)
    }
    fn csm(&self) -> u32 {
        if Self::IS_OPL3_PLUS {
            0
        } else {
            self.byte(0x08, 7, 1, 0)
        }
    }
    fn rhythm_enable(&self) -> u32 {
        self.byte(0xbd, 5, 1, 0)
    }

    fn ch_feedback(&self, choffs: u32) -> u32 {
        self.byte(0xc0, 1, 3, choffs)
    }
    fn ch_algorithm(&self, choffs: u32) -> u32 {
        self.byte(0xc0, 0, 1, choffs)
            | if Self::IS_OPL3_PLUS {
                8 | (self.byte(0xc3, 0, 1, choffs) << 1)
            } else {
                0
            }
    }
    fn ch_output_any(&self, choffs: u32) -> u32 {
        if self.newflag() != 0 {
            self.byte(0xc0 + choffs, 4, 4, 0)
        } else {
            1
        }
    }
    fn ch_output_0(&self, choffs: u32) -> u32 {
        if self.newflag() != 0 {
            self.byte(0xc0 + choffs, 4, 1, 0)
        } else {
            1
        }
    }
    fn ch_output_1(&self, choffs: u32) -> u32 {
        if self.newflag() != 0 {
            self.byte(0xc0 + choffs, 5, 1, 0)
        } else if Self::IS_OPL3_PLUS {
            1
        } else {
            0
        }
    }
    fn ch_output_2(&self, choffs: u32) -> u32 {
        if self.newflag() != 0 {
            self.byte(0xc0 + choffs, 6, 1, 0)
        } else {
            0
        }
    }
    fn ch_output_3(&self, choffs: u32) -> u32 {
        if self.newflag() != 0 {
            self.byte(0xc0 + choffs, 7, 1, 0)
        } else {
            0
        }
    }

    fn op_lfo_am_enable(&self, opoffs: u32) -> u32 {
        self.byte(0x20, 7, 1, opoffs)
    }
}

/// The OPL register set.
pub(crate) type Opl1Registers = OplRegisters<1>;
/// The OPL2 register set.
pub(crate) type Opl2Registers = OplRegisters<2>;
/// The OPL3 register set.
pub(crate) type Opl3Registers = OplRegisters<3>;

// ---- OPLL register set (opll_registers): not yet ported ----

//*********************************************************
//  YM3526
//*********************************************************

/// The Yamaha YM3526 (OPL): nine 2-operator channels with one waveform, and
/// one output through a floating-point DAC.
#[derive(Clone)]
pub struct Ym3526<I: Interface = NullInterface> {
    /// The address register.
    address: u8,
    /// The core FM engine.
    fm: FmEngine<Opl1Registers, I>,
}

impl<I: Interface> Ym3526<I> {
    /// The number of outputs.
    pub const OUTPUTS: usize = 1;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        let mut chip = Ym3526 {
            address: 0,
            fm: FmEngine::new(intf),
        };
        chip.reset();
        chip
    }

    /// Resets the chip.
    pub fn reset(&mut self) {
        // reset the engines
        self.fm.reset();
    }

    /// Returns the output sample rate for an input clock.
    pub fn sample_rate(&self, input_clock: u32) -> u32 {
        self.fm.sample_rate(input_clock)
    }

    /// Invalidates the cached operator data.
    pub fn invalidate_caches(&mut self) {
        self.fm.invalidate_caches();
    }

    /// Reads the status register.
    pub fn read_status(&mut self) -> u8 {
        self.fm.status() | 0x06
    }

    /// Reads from the chip: the status at offset 0.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 1 {
            0 => self.read_status(), // status port
            // when A0=1 datasheet says "the data on the bus are not
            // guaranteed"
            _ => 0xff,
        }
    }

    /// Writes the address register.
    pub fn write_address(&mut self, data: u8) {
        // YM3526 doesn't expose a busy signal, and the datasheets don't
        // indicate delays, but all other OPL chips need 12 cycles for address
        // writes
        let busy = 12 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);

        // just set the address
        self.address = data;
    }

    /// Writes the data register.
    pub fn write_data(&mut self, data: u8) {
        // YM3526 doesn't expose a busy signal, and the datasheets don't
        // indicate delays, but all other OPL chips need 84 cycles for data
        // writes
        let busy = 84 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);

        // write to FM
        self.fm.write(u16::from(self.address), data);
    }

    /// Writes to the chip: the address port at offset 0, the data port at 1.
    pub fn write(&mut self, offset: u32, data: u8) {
        match offset & 1 {
            0 => self.write_address(data), // address port
            _ => self.write_data(data),    // data port
        }
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<1>]) {
        for out in output.iter_mut() {
            // clock the system
            self.fm.clock(FmEngine::<Opl1Registers, I>::ALL_CHANNELS);

            // update the FM content; mixing details for YM3526 need
            // verification
            self.fm.output(
                &mut out.clear().data,
                1,
                32767,
                FmEngine::<Opl1Registers, I>::ALL_CHANNELS,
            );

            // YM3526 uses an external DAC (YM3014) with mantissa/exponent
            // format; convert to 10.3 floating point value and back to
            // simulate truncation
            out.roundtrip_fp();
        }
    }

    /// Reports the expiry of timer `tnum` (0 or 1), which the interface's
    /// `set_timer` asked for.
    pub fn timer_expired(&mut self, tnum: u32) {
        self.fm.engine_timer_expired(tnum);
    }

    /// Returns the interface.
    pub fn intf(&self) -> &I {
        self.fm.intf()
    }

    /// Returns the interface, mutably.
    pub fn intf_mut(&mut self) -> &mut I {
        self.fm.intf_mut()
    }

    /// Returns the register file as last written, for a diagnostic.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}

// ---- Y8950: not yet ported ----

//*********************************************************
//  YM3812
//*********************************************************

/// The Yamaha YM3812 (OPL2): nine 2-operator channels with four waveforms,
/// and one output through a floating-point DAC.
#[derive(Clone)]
pub struct Ym3812<I: Interface = NullInterface> {
    /// The address register.
    address: u8,
    /// The core FM engine.
    fm: FmEngine<Opl2Registers, I>,
}

impl<I: Interface> Ym3812<I> {
    /// The number of outputs.
    pub const OUTPUTS: usize = 1;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        let mut chip = Ym3812 {
            address: 0,
            fm: FmEngine::new(intf),
        };
        chip.reset();
        chip
    }

    /// Resets the chip.
    pub fn reset(&mut self) {
        // reset the engines
        self.fm.reset();
    }

    /// Returns the output sample rate for an input clock.
    pub fn sample_rate(&self, input_clock: u32) -> u32 {
        self.fm.sample_rate(input_clock)
    }

    /// Invalidates the cached operator data.
    pub fn invalidate_caches(&mut self) {
        self.fm.invalidate_caches();
    }

    /// Reads the status register.
    pub fn read_status(&mut self) -> u8 {
        self.fm.status() | 0x06
    }

    /// Reads from the chip: the status at offset 0.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 1 {
            0 => self.read_status(), // status port
            _ => 0xff,               // "inhibit" according to datasheet
        }
    }

    /// Writes the address register.
    pub fn write_address(&mut self, data: u8) {
        // YM3812 doesn't expose a busy signal, but it does indicate that
        // address writes should be no faster than every 12 clocks
        let busy = 12 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);

        // just set the address
        self.address = data;
    }

    /// Writes the data register.
    pub fn write_data(&mut self, data: u8) {
        // YM3812 doesn't expose a busy signal, but it does indicate that data
        // writes should be no faster than every 84 clocks
        let busy = 84 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);

        // write to FM
        self.fm.write(u16::from(self.address), data);
    }

    /// Writes to the chip: the address port at offset 0, the data port at 1.
    pub fn write(&mut self, offset: u32, data: u8) {
        match offset & 1 {
            0 => self.write_address(data), // address port
            _ => self.write_data(data),    // data port
        }
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<1>]) {
        for out in output.iter_mut() {
            // clock the system
            self.fm.clock(FmEngine::<Opl2Registers, I>::ALL_CHANNELS);

            // update the FM content; mixing details for YM3812 need
            // verification
            self.fm.output(
                &mut out.clear().data,
                1,
                32767,
                FmEngine::<Opl2Registers, I>::ALL_CHANNELS,
            );

            // YM3812 uses an external DAC (YM3014) with mantissa/exponent
            // format; convert to 10.3 floating point value and back to
            // simulate truncation
            out.roundtrip_fp();
        }
    }

    /// Reports the expiry of timer `tnum` (0 or 1), which the interface's
    /// `set_timer` asked for.
    pub fn timer_expired(&mut self, tnum: u32) {
        self.fm.engine_timer_expired(tnum);
    }

    /// Returns the interface.
    pub fn intf(&self) -> &I {
        self.fm.intf()
    }

    /// Returns the interface, mutably.
    pub fn intf_mut(&mut self) -> &mut I {
        self.fm.intf_mut()
    }

    /// Returns the register file as last written, for a diagnostic.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}

//*********************************************************
//  YMF262
//*********************************************************

/// The Yamaha YMF262 (OPL3): eighteen channels that pair into 4-operator
/// voices, eight waveforms, and four outputs, A to D.
#[derive(Clone)]
pub struct Ymf262<I: Interface = NullInterface> {
    /// The address register.
    address: u16,
    /// The core FM engine.
    fm: FmEngine<Opl3Registers, I>,
}

impl<I: Interface> Ymf262<I> {
    /// The number of outputs.
    pub const OUTPUTS: usize = 4;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        let mut chip = Ymf262 {
            address: 0,
            fm: FmEngine::new(intf),
        };
        chip.reset();
        chip
    }

    /// Resets the chip.
    pub fn reset(&mut self) {
        // reset the engines
        self.fm.reset();
    }

    /// Returns the output sample rate for an input clock.
    pub fn sample_rate(&self, input_clock: u32) -> u32 {
        self.fm.sample_rate(input_clock)
    }

    /// Invalidates the cached operator data.
    pub fn invalidate_caches(&mut self) {
        self.fm.invalidate_caches();
    }

    /// Reads the status register.
    pub fn read_status(&mut self) -> u8 {
        self.fm.status()
    }

    /// Reads from the chip: the status at offset 0.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 3 {
            0 => self.read_status(), // status port
            _ => 0xff,
        }
    }

    /// Writes the low address register.
    pub fn write_address(&mut self, data: u8) {
        // YMF262 doesn't expose a busy signal, but it does indicate that
        // address writes should be no faster than every 32 clocks
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);

        // just set the address
        self.address = u16::from(data);
    }

    /// Writes the data register.
    pub fn write_data(&mut self, data: u8) {
        // YMF262 doesn't expose a busy signal, but it does indicate that data
        // writes should be no faster than every 32 clocks
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);

        // write to FM
        self.fm.write(self.address, data);
    }

    /// Writes the high address register, which selects the second bank.
    pub fn write_address_hi(&mut self, data: u8) {
        // YMF262 doesn't expose a busy signal, but it does indicate that
        // address writes should be no faster than every 32 clocks
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);

        // just set the address
        self.address = u16::from(data) | 0x100;

        // tests reveal that in compatibility mode, upper bit is masked except
        // for register 0x105
        if self.fm.regs().newflag() == 0 && self.address != 0x105 {
            self.address &= 0xff;
        }
    }

    /// Writes to the chip: the address ports at offsets 0 and 2 (the second
    /// bank), the data ports at 1 and 3.
    pub fn write(&mut self, offset: u32, data: u8) {
        match offset & 3 {
            0 => self.write_address(data),    // address port
            2 => self.write_address_hi(data), // address port
            _ => self.write_data(data),       // data port
        }
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<4>]) {
        for out in output.iter_mut() {
            // clock the system
            self.fm.clock(FmEngine::<Opl3Registers, I>::ALL_CHANNELS);

            // update the FM content; mixing details for YMF262 need
            // verification
            self.fm.output(
                &mut out.clear().data,
                0,
                32767,
                FmEngine::<Opl3Registers, I>::ALL_CHANNELS,
            );

            // YMF262 output is 16-bit offset serial via YAC512 DAC
            out.clamp16();
        }
    }

    /// Reports the expiry of timer `tnum` (0 or 1), which the interface's
    /// `set_timer` asked for.
    pub fn timer_expired(&mut self, tnum: u32) {
        self.fm.engine_timer_expired(tnum);
    }

    /// Returns the interface.
    pub fn intf(&self) -> &I {
        self.fm.intf()
    }

    /// Returns the interface, mutably.
    pub fn intf_mut(&mut self) -> &mut I {
        self.fm.intf_mut()
    }

    /// Returns the register file as last written, for a diagnostic.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}

// ---- YMF289B: not yet ported ----

// ---- YMF278B: not yet ported ----

// ---- OPLL chips (YM2413, YM2423, YMF281, DS1001): not yet ported ----
