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

//*********************************************************
//  OPLL REGISTERS
//*********************************************************

// The OPLL register map, as ymfm documents it:
//
//      System-wide registers:
//           0E --x----- Rhythm enable
//              ---x---- Bass drum key on
//              ----x--- Snare drum key on
//              -----x-- Tom key on
//              ------x- Top cymbal key on
//              -------x High hat key on
//           0F xxxxxxxx Test register
//
//     Per-channel registers (channel in address bits 0-3)
//        10-18 xxxxxxxx F-number (low 8 bits)
//        20-28 --x----- Sustain on
//              ---x---- Key on
//              --- xxx- Block (octvate, 0-7)
//              -------x F-number (high bit)
//        30-38 xxxx---- Instrument selection
//              ----xxxx Volume
//
//     User instrument registers (for carrier, modulator operators)
//        00-01 x------- AM enable
//              -x------ PM enable (VIB)
//              --x----- EG type
//              ---x---- Key scale rate
//              ----xxxx Multiple value (0-15)
//           02 xx------ Key scale level (carrier, 0-3)
//              --xxxxxx Total level (modulator, 0-63)
//           03 xx------ Key scale level (modulator, 0-3)
//              ---x---- Rectified wave (carrier)
//              ----x--- Rectified wave (modulator)
//              -----xxx Feedback level for operator 1 (0-7)
//        04-05 xxxx---- Attack rate (0-15)
//              ----xxxx Decay rate (0-15)
//        06-07 xxxx---- Sustain level (0-15)
//              ----xxxx Release rate (0-15)
//
//     Internal (fake) registers:
//        40-48 xxxxxxxx Current instrument base address
//        4E-5F xxxxxxxx Current instrument base address + operator slot (0/1)
//        70-FF xxxxxxxx Data for instruments (1-16 plus 3 drums)

/// The size of an OPLL instrument set: fifteen melodic instruments and the
/// three rhythm ones, eight bytes each.
pub const OPLL_INSTRUMENT_DATA_SIZE: usize = 0x90;

/// The register set of the OPLL family, with its LFOs and noise generator.
#[derive(Clone)]
pub(crate) struct OpllRegisters {
    /// The LFO AM counter.
    lfo_am_counter: u16,
    /// The LFO PM counter.
    lfo_pm_counter: u16,
    /// The noise LFSR state.
    noise_lfsr: u32,
    /// The current LFO AM value.
    lfo_am: u8,
    /// Where each channel's instrument data starts in `data`; it stands in
    /// for ymfm's pointer.
    chinst: [u8; 9],
    /// Where each operator's instrument data starts in `data`, the channel's
    /// plus the operator slot; it stands in for ymfm's pointer.
    opinst: [u8; 18],
    /// The register data, then the instrument data, so that ymfm's pointers
    /// into either become one index.
    data: [u8; OpllRegisters::REGISTERS + OPLL_INSTRUMENT_DATA_SIZE],
}

impl OpllRegisters {
    /// The number of waveforms offered.
    const WAVEFORMS: u32 = 2;
    /// The number of 8-bit registers.
    pub(crate) const REGISTERS: usize = 0x40;
    /// Where the instrument data starts in `data`.
    const INSTDATA: usize = Self::REGISTERS;

    /// Returns a bitfield extracted from a byte.
    fn byte(&self, offset: u32, start: u32, count: u32, extra_offset: u32) -> u32 {
        bitfield(
            u32::from(self.data[(offset + extra_offset) as usize]),
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

    /// Returns a bitfield of a channel's instrument data.
    fn instchbyte(&self, offset: u32, start: u32, count: u32, choffs: u32) -> u32 {
        let base = u32::from(self.chinst[choffs as usize]);
        bitfield(u32::from(self.data[(base + offset) as usize]), start, count)
    }

    /// Returns a bitfield of an operator's instrument data.
    fn instopbyte(&self, offset: u32, start: u32, count: u32, opoffs: u32) -> u32 {
        let base = u32::from(self.opinst[opoffs as usize]);
        bitfield(u32::from(self.data[(base + offset) as usize]), start, count)
    }

    /// Reports whether a channel is an active rhythm channel.
    fn is_rhythm(&self, choffs: u32) -> bool {
        self.rhythm_enable() != 0 && choffs >= 6
    }

    /// Sets the instrument data: fifteen melodic instruments, then the bass
    /// drum, the high hat and snare drum, and the tom and top cymbal.
    pub(crate) fn set_instrument_data(&mut self, data: &[u8; OPLL_INSTRUMENT_DATA_SIZE]) {
        self.data[Self::INSTDATA..].copy_from_slice(data);
    }

    /// Returns the register data, for a diagnostic.
    pub(crate) fn data(&self) -> &[u8] {
        &self.data[..Self::REGISTERS]
    }

    // per-channel registers
    fn ch_block_freq(&self, choffs: u32) -> u32 {
        self.word(0x20, 0, 4, 0x10, 0, 8, choffs)
    }
    fn ch_sustain(&self, choffs: u32) -> u32 {
        self.byte(0x20, 5, 1, choffs)
    }
    fn ch_total_level(&self, choffs: u32) -> u32 {
        self.instchbyte(0x02, 0, 6, choffs)
    }
    fn ch_instrument(&self, choffs: u32) -> u32 {
        self.byte(0x30, 4, 4, choffs)
    }

    // per-operator registers
    fn op_lfo_pm_enable(&self, opoffs: u32) -> u32 {
        self.instopbyte(0x00, 6, 1, opoffs)
    }
    fn op_eg_sustain(&self, opoffs: u32) -> u32 {
        self.instopbyte(0x00, 5, 1, opoffs)
    }
    fn op_ksr(&self, opoffs: u32) -> u32 {
        self.instopbyte(0x00, 4, 1, opoffs)
    }
    fn op_multiple(&self, opoffs: u32) -> u32 {
        self.instopbyte(0x00, 0, 4, opoffs)
    }
    fn op_ksl(&self, opoffs: u32) -> u32 {
        self.instopbyte(0x02, 6, 2, opoffs)
    }
    fn op_waveform(&self, opoffs: u32) -> u32 {
        self.instchbyte(0x03, 3 + bitfield(opoffs, 0, 1), 1, opoffs >> 1)
    }
    fn op_attack_rate(&self, opoffs: u32) -> u32 {
        self.instopbyte(0x04, 4, 4, opoffs)
    }
    fn op_decay_rate(&self, opoffs: u32) -> u32 {
        self.instopbyte(0x04, 0, 4, opoffs)
    }
    fn op_sustain_level(&self, opoffs: u32) -> u32 {
        self.instopbyte(0x06, 4, 4, opoffs)
    }
    fn op_release_rate(&self, opoffs: u32) -> u32 {
        self.instopbyte(0x06, 0, 4, opoffs)
    }
    fn op_volume(&self, opoffs: u32) -> u32 {
        self.byte(0x30, 4 * bitfield(!opoffs, 0, 1), 4, opoffs >> 1)
    }
}

impl FmRegisters for OpllRegisters {
    const OUTPUTS: usize = 2;
    const CHANNELS: usize = 9;
    const ALL_CHANNELS: u32 = (1 << Self::CHANNELS) - 1;
    const OPERATORS: usize = Self::CHANNELS * 2;
    const DEFAULT_PRESCALE: u32 = 4;
    const EG_CLOCK_DIVIDER: u32 = 1;
    const CSM_TRIGGER_MASK: u32 = 0;
    const REG_MODE: u32 = 0x3f;
    const STATUS_TIMERA: u8 = 0;
    const STATUS_TIMERB: u8 = 0;
    const STATUS_BUSY: u8 = 0;
    const STATUS_IRQ: u8 = 0;
    const EG_HAS_DEPRESS: bool = true;
    const MODULATOR_DELAY: bool = true;

    fn new() -> Self {
        // ymfm leaves the register and instrument data undefined until reset
        // and the chip's constructor, and points every channel at the user
        // instrument in the meantime
        let mut opinst = [0u8; 18];
        for (opoffs, base) in opinst.iter_mut().enumerate() {
            *base = bitfield(opoffs as u32, 0, 1) as u8;
        }
        OpllRegisters {
            lfo_am_counter: 0,
            lfo_pm_counter: 0,
            noise_lfsr: 1,
            lfo_am: 0,
            chinst: [0; 9],
            opinst,
            data: [0; Self::REGISTERS + OPLL_INSTRUMENT_DATA_SIZE],
        }
    }

    fn reset(&mut self) {
        self.data[..Self::REGISTERS].fill(0);
    }

    fn channel_offset(chnum: u32) -> u32 {
        chnum
    }

    fn operator_offset(opnum: u32) -> u32 {
        opnum
    }

    fn operator_map(&self, dest: &mut [u32; MAX_CHANNELS]) {
        // for OPLL the map is fixed
        const fn pair(o1: u8, o2: u8) -> u32 {
            operator_list(o1, o2, 0xff, 0xff)
        }
        dest[..9].copy_from_slice(&[
            pair(0, 1),   // Channel 0 operators
            pair(2, 3),   // Channel 1 operators
            pair(4, 5),   // Channel 2 operators
            pair(6, 7),   // Channel 3 operators
            pair(8, 9),   // Channel 4 operators
            pair(10, 11), // Channel 5 operators
            pair(12, 13), // Channel 6 operators
            pair(14, 15), // Channel 7 operators
            pair(16, 17), // Channel 8 operators
        ]);
    }

    fn write(&mut self, index: u16, data: u8) -> Option<(u32, u32)> {
        let index = u32::from(index);

        // unclear the address is masked down to 6 bits or if writes above
        // the register top are ignored; ymfm assumes the latter for now
        if index >= Self::REGISTERS as u32 {
            return None;
        }

        // write the new data
        self.data[index as usize] = data;
        let data = u32::from(data);

        // handle writes to the rhythm keyons
        if index == 0x0e {
            let opmask = if bitfield(data, 5, 1) != 0 {
                bitfield(data, 0, 5)
            } else {
                0
            };
            return Some((RHYTHM_CHANNEL, opmask));
        }

        // handle writes to the channel keyons
        if (index & 0xf0) == 0x20 {
            let channel = index & 0x0f;
            if (channel as usize) < Self::CHANNELS {
                let opmask = if bitfield(data, 4, 1) != 0 { 3 } else { 0 };
                return Some((channel, opmask));
            }
        }
        None
    }

    fn clock_noise_and_lfo(&mut self) -> i32 {
        // implementation is the same as OPL with fixed depths
        opl_clock_noise_and_lfo(
            &mut self.noise_lfsr,
            &mut self.lfo_am_counter,
            &mut self.lfo_pm_counter,
            &mut self.lfo_am,
            1,
            1,
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
        // first set up the instrument data
        let instrument = self.ch_instrument(choffs);
        self.chinst[choffs as usize] = if self.rhythm_enable() != 0 && choffs >= 6 {
            (Self::INSTDATA as u32 + 8 * (15 + (choffs - 6))) as u8
        } else if instrument == 0 {
            0
        } else {
            (Self::INSTDATA as u32 + 8 * (instrument - 1)) as u8
        };
        self.opinst[opoffs as usize] = self.chinst[choffs as usize] + bitfield(opoffs, 0, 1) as u8;

        // set up the easy stuff
        cache.waveform = (self.op_waveform(opoffs) % Self::WAVEFORMS) as u8;

        // get frequency from the channel
        let block_freq = self.ch_block_freq(choffs);
        cache.block_freq = block_freq;

        // compute the keycode: block_freq is:
        //
        //     11  |
        //     1098|76543210
        //     BBBF|FFFFFFFF
        //     ^^^^
        //
        // the 4-bit keycode uses the top 4 bits
        let keycode = bitfield(block_freq, 8, 4);

        // no detune adjustment on OPLL
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

        // total level, scaled by 8; for non-rhythm operator 0, this is the
        // total level from the instrument data; for other operators it is
        // 4*volume
        cache.total_level =
            if bitfield(opoffs, 0, 1) == 1 || (self.rhythm_enable() != 0 && choffs >= 7) {
                self.op_volume(opoffs) * 4
            } else {
                self.ch_total_level(choffs)
            };
        cache.total_level <<= 3;

        // pre-add key scale level
        let ksl = self.op_ksl(opoffs);
        if ksl != 0 {
            cache.total_level +=
                opl_key_scale_atten(bitfield(block_freq, 9, 3), bitfield(block_freq, 5, 4)) << ksl;
        }

        // 4-bit sustain level, but 15 means 31 so effectively 5 bits
        cache.eg_sustain = self.op_sustain_level(opoffs);
        cache.eg_sustain |= (cache.eg_sustain + 1) & 0x10;
        cache.eg_sustain <<= 5;

        // The envelope diagram in the YM2413 datasheet gives values for these
        // in ms from 0->48dB. The attack/decay tables give values in ms from
        // 0->96dB, so to pick an equivalent decay rate, ymfm finds the
        // closest match that is 2x the 0->48dB value:
        //
        //     DP =   10ms (0->48db) ->   20ms (0->96db); decay of 12 gives   19.20ms
        //     RR =  310ms (0->48db) ->  620ms (0->96db); decay of  7 gives  613.76ms
        //     RS = 1200ms (0->48db) -> 2400ms (0->96db); decay of  5 gives 2455.04ms
        //
        // The envelope diagram for percussive sounds (eg_sustain() == 0) also
        // uses "RR" to mean both the constant RR above and the Release Rate
        // specified in the instrument data. In this case, Relief Pitcher's
        // credit sound bears out that the Release Rate is used during
        // sustain, and that the constant RR (or RS) is used during the
        // release phase.
        const DP: u8 = 12 * 4;
        const RR: u8 = 7 * 4;
        const RS: u8 = 5 * 4;

        // determine KSR adjustment for envelope rates
        let ksrval = keycode >> (2 * (self.op_ksr(opoffs) ^ 1));
        cache.eg_rate[EG_DEPRESS] = DP;
        cache.eg_rate[EG_ATTACK] = effective_rate(self.op_attack_rate(opoffs) * 4, ksrval);
        cache.eg_rate[EG_DECAY] = effective_rate(self.op_decay_rate(opoffs) * 4, ksrval);
        if self.op_eg_sustain(opoffs) != 0 {
            cache.eg_rate[EG_SUSTAIN] = 0;
            cache.eg_rate[EG_RELEASE] = if self.ch_sustain(choffs) != 0 {
                RS
            } else {
                effective_rate(self.op_release_rate(opoffs) * 4, ksrval)
            };
        } else {
            cache.eg_rate[EG_SUSTAIN] = effective_rate(self.op_release_rate(opoffs) * 4, ksrval);
            cache.eg_rate[EG_RELEASE] = if self.ch_sustain(choffs) != 0 { RS } else { RR };
        }
    }

    fn compute_phase_step(
        &mut self,
        _choffs: u32,
        opoffs: u32,
        cache: &OpdataCache,
        lfo_raw_pm: i32,
    ) -> u32 {
        // phase step computation is the same as OPL but the block_freq has
        // one more bit, which we shift in
        let pm = if self.op_lfo_pm_enable(opoffs) != 0 {
            lfo_raw_pm
        } else {
            0
        };
        opl_compute_phase_step(cache.block_freq << 1, cache.multiple, pm)
    }

    fn waveform(&self, index: u8) -> &[u16; WAVEFORM_LENGTH] {
        // the OPLL's two waveforms, the sine and the half sine, are the
        // first two of the OPL's, built the same way
        &OPL_WAVEFORMS[index as usize]
    }

    // the OPLL has no timers, status or CSM
    fn timer_a_value(&self) -> u32 {
        0
    }
    fn timer_b_value(&self) -> u32 {
        0
    }
    fn reset_timer_b(&self) -> u32 {
        0
    }
    fn reset_timer_a(&self) -> u32 {
        0
    }
    fn enable_timer_b(&self) -> u32 {
        0
    }
    fn enable_timer_a(&self) -> u32 {
        0
    }
    fn load_timer_b(&self) -> u32 {
        0
    }
    fn load_timer_a(&self) -> u32 {
        0
    }
    fn csm(&self) -> u32 {
        0
    }
    fn rhythm_enable(&self) -> u32 {
        self.byte(0x0e, 5, 1, 0)
    }

    fn ch_feedback(&self, choffs: u32) -> u32 {
        self.instchbyte(0x03, 0, 3, choffs)
    }
    fn ch_algorithm(&self, _choffs: u32) -> u32 {
        0
    }
    fn ch_output_any(&self, _choffs: u32) -> u32 {
        1
    }
    fn ch_output_0(&self, choffs: u32) -> u32 {
        u32::from(!self.is_rhythm(choffs))
    }
    fn ch_output_1(&self, choffs: u32) -> u32 {
        u32::from(self.is_rhythm(choffs))
    }
    fn ch_output_2(&self, _choffs: u32) -> u32 {
        0
    }
    fn ch_output_3(&self, _choffs: u32) -> u32 {
        0
    }

    fn op_lfo_am_enable(&self, opoffs: u32) -> u32 {
        self.instopbyte(0x00, 7, 1, opoffs)
    }
}

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

//*********************************************************
//  Y8950
//*********************************************************

use crate::adpcm::AdpcmBEngine;
use crate::ymfm::AccessClass;

/// The Yamaha Y8950 (MSX-AUDIO): the OPL's nine 2-operator channels with an
/// ADPCM-B channel beside them, a keyboard port and four I/O lines, and one
/// output through a floating-point DAC.
///
/// The ADPCM-B sample memory is the interface's `AdpcmB` class, and the
/// keyboard and I/O ports are its `Io` class: keyboard in and out at address
/// 1, the I/O lines at address 0.
#[derive(Clone)]
pub struct Y8950<I: Interface = NullInterface> {
    /// The address register.
    address: u8,
    /// The data direction register for I/O.
    io_ddr: u8,
    /// The core FM engine.
    fm: FmEngine<Opl1Registers, I>,
    /// The ADPCM-B engine.
    adpcm_b: AdpcmBEngine,
}

impl<I: Interface> Y8950<I> {
    /// The number of outputs.
    pub const OUTPUTS: usize = 1;

    /// The status bit for a playing ADPCM-B sample.
    pub const STATUS_ADPCM_B_PLAYING: u8 = 0x01;
    /// The status bit for the ADPCM-B buffer being ready.
    pub const STATUS_ADPCM_B_BRDY: u8 = 0x08;
    /// The status bit for the ADPCM-B end of sample.
    pub const STATUS_ADPCM_B_EOS: u8 = 0x10;
    /// Every status bit that could signal an IRQ. ymfm declares it but never
    /// installs it as the IRQ mask, so only the timers raise the IRQ line.
    pub const ALL_IRQS: u8 = Self::STATUS_ADPCM_B_BRDY
        | Self::STATUS_ADPCM_B_EOS
        | FmEngine::<Opl1Registers, I>::STATUS_TIMERA
        | FmEngine::<Opl1Registers, I>::STATUS_TIMERB;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        let mut chip = Y8950 {
            address: 0,
            io_ddr: 0,
            fm: FmEngine::new(intf),
            adpcm_b: AdpcmBEngine::new(0),
        };
        chip.reset();
        chip
    }

    /// Resets the chip.
    pub fn reset(&mut self) {
        // reset the engines
        self.fm.reset();
        self.adpcm_b.reset();
    }

    /// Returns the output sample rate for an input clock.
    pub fn sample_rate(&self, input_clock: u32) -> u32 {
        self.fm.sample_rate(input_clock)
    }

    /// Invalidates the cached operator data.
    pub fn invalidate_caches(&mut self) {
        self.fm.invalidate_caches();
    }

    /// Reads the status register, with the live ADPCM-B bits.
    pub fn read_status(&mut self) -> u8 {
        // start with current FM status, masking out bits we might set
        let mut status = self.fm.status()
            & !(Self::STATUS_ADPCM_B_EOS
                | Self::STATUS_ADPCM_B_BRDY
                | Self::STATUS_ADPCM_B_PLAYING);

        // insert the live ADPCM status bits
        let adpcm_status = self.adpcm_b.status();
        if (adpcm_status & AdpcmBEngine::STATUS_EOS) != 0 {
            status |= Self::STATUS_ADPCM_B_EOS;
        }
        if (adpcm_status & AdpcmBEngine::STATUS_BRDY) != 0 {
            status |= Self::STATUS_ADPCM_B_BRDY;
        }
        if (adpcm_status & AdpcmBEngine::STATUS_PLAYING) != 0 {
            status |= Self::STATUS_ADPCM_B_PLAYING;
        }

        // run it through the FM engine to handle interrupts for us
        self.fm.set_reset_status(status, !status)
    }

    /// Reads the data port: the keyboard, the I/O lines, or ADPCM data.
    pub fn read_data(&mut self) -> u8 {
        match self.address {
            // keyboard in
            0x05 => self.fm.intf_mut().external_read(AccessClass::Io, 1),

            // ADPCM data; ymfm passes these on as ADPCM-B registers 2 and
            // 0x13, neither of which is the data register 8, so they read 0
            0x09 | 0x1a => {
                self.adpcm_b
                    .read(u32::from(self.address) - 0x07, self.fm.intf_mut()) as u8
            }

            // I/O data
            0x19 => self.fm.intf_mut().external_read(AccessClass::Io, 0),

            _ => 0xff,
        }
    }

    /// Reads from the chip: the status at offset 0, the data port at 1.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 1 {
            0 => self.read_status(), // status port
            // when A0=1 datasheet says "the data on the bus are not
            // guaranteed"
            _ => self.read_data(),
        }
    }

    /// Writes the address register.
    pub fn write_address(&mut self, data: u8) {
        // Y8950 doesn't expose a busy signal, but it does indicate that
        // address writes should be no faster than every 12 clocks
        let busy = 12 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);

        // just set the address
        self.address = data;
    }

    /// Writes the data register.
    pub fn write_data(&mut self, data: u8) {
        // Y8950 doesn't expose a busy signal, but it does indicate that data
        // writes should be no faster than every 12 clocks for registers
        // 00-1A, or every 84 clocks for other registers
        let busy = if self.address <= 0x1a { 12 } else { 84 } * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);

        // handle special addresses
        match self.address {
            // IRQ control
            0x04 => {
                self.fm.write(u16::from(self.address), data);
                self.read_status();
            }

            // keyboard out
            0x06 => self.fm.intf_mut().external_write(AccessClass::Io, 1, data),

            // split FM/ADPCM-B
            0x08 => {
                self.adpcm_b.write(
                    u32::from(self.address) - 0x07,
                    (data & 0x0f) | 0x80,
                    self.fm.intf_mut(),
                );
                self.fm.write(u16::from(self.address), data & 0xc0);
            }

            // ADPCM-B registers
            0x07 | 0x09..=0x12 | 0x15..=0x17 => {
                self.adpcm_b
                    .write(u32::from(self.address) - 0x07, data, self.fm.intf_mut());
            }

            // I/O direction
            0x18 => self.io_ddr = data & 0x0f,

            // I/O data
            0x19 => {
                let value = data & self.io_ddr;
                self.fm.intf_mut().external_write(AccessClass::Io, 0, value);
            }

            // everything else to FM
            _ => self.fm.write(u16::from(self.address), data),
        }
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
            self.adpcm_b.clock(self.fm.intf_mut());

            // update the FM content; clipping need verification
            self.fm.output(
                &mut out.clear().data,
                1,
                32767,
                FmEngine::<Opl1Registers, I>::ALL_CHANNELS,
            );

            // mix in the ADPCM; ADPCM-B is stereo, but only one channel not
            // sure how it's wired up internally
            self.adpcm_b.output(out, 3);

            // Y8950 uses an external DAC (YM3014) with mantissa/exponent
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

    /// Returns the FM register file as last written, for a diagnostic.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }

    /// Returns the ADPCM-B register file as last written, for a diagnostic.
    pub fn adpcm_b_registers(&self) -> &[u8] {
        self.adpcm_b.registers()
    }
}

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

//*********************************************************
//  YMF289B
//*********************************************************

// YMF289B is a YMF262 with the following changes:
//   * "Power down" mode added
//   * Bulk register clear added
//   * Busy flag added to the status register
//   * Shorter busy times
//   * All registers can be read
//   * Only 2 outputs exposed

/// The Yamaha YMF289B (OPL3L): a YMF262 with a power-down mode, a bulk
/// register clear, a busy flag, shorter busy times, readable registers, and
/// two of its four outputs.
#[derive(Clone)]
pub struct Ymf289b<I: Interface = NullInterface> {
    /// The address register.
    address: u16,
    /// The core FM engine.
    fm: FmEngine<Opl3Registers, I>,
}

impl<I: Interface> Ymf289b<I> {
    /// The number of outputs.
    pub const OUTPUTS: usize = 2;

    /// The status bits the busy flag sets.
    const STATUS_BUSY_FLAGS: u8 = 0x05;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        let mut chip = Ymf289b {
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

    /// Reports whether the chip is in YMF289B mode, bit 2 of register 0x105.
    fn ymf289b_mode(&self) -> bool {
        (self.fm.regs().data()[0x105] & 0x04) != 0
    }

    /// Reads the status register.
    pub fn read_status(&mut self) -> u8 {
        let mut result = self.fm.status();

        // YMF289B adds a busy flag
        if self.ymf289b_mode() && self.fm.intf_mut().is_busy() {
            result |= Self::STATUS_BUSY_FLAGS;
        }
        result
    }

    /// Reads the data register: in YMF289B mode, the register the address
    /// selects.
    pub fn read_data(&mut self) -> u8 {
        // YMF289B can read register data back
        if self.ymf289b_mode() {
            self.fm.regs().data()[self.address as usize]
        } else {
            0xff
        }
    }

    /// Reads from the chip: the status at offset 0, the data at 1.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 3 {
            0 => self.read_status(), // status port
            1 => self.read_data(),   // data port
            _ => 0xff,
        }
    }

    /// Writes the low address register.
    pub fn write_address(&mut self, data: u8) {
        self.address = u16::from(data);

        // count busy time
        self.fm.intf_mut().set_busy_end(56);
    }

    /// Writes the data register.
    pub fn write_data(&mut self, data: u8) {
        // write to FM
        self.fm.write(self.address, data);

        // writes to 0x108 with the CLR flag set clear the registers
        if self.address == 0x108 && bitfield(u32::from(data), 2, 1) != 0 {
            self.fm.regs_mut().reset();
        }

        // count busy time
        self.fm.intf_mut().set_busy_end(56);
    }

    /// Writes the high address register, which selects the second bank.
    pub fn write_address_hi(&mut self, data: u8) {
        // just set the address
        self.address = u16::from(data) | 0x100;

        // tests reveal that in compatibility mode, upper bit is masked except
        // for register 0x105
        if self.fm.regs().newflag() == 0 && self.address != 0x105 {
            self.address &= 0xff;
        }

        // count busy time
        self.fm.intf_mut().set_busy_end(56);
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
    pub fn generate(&mut self, output: &mut [Output<2>]) {
        for out in output.iter_mut() {
            // clock the system
            self.fm.clock(FmEngine::<Opl3Registers, I>::ALL_CHANNELS);

            // update the FM content; mixing details for YMF262 need
            // verification
            let mut full = Output::<4>::default();
            self.fm.output(
                &mut full.clear().data,
                0,
                32767,
                FmEngine::<Opl3Registers, I>::ALL_CHANNELS,
            );

            // YMF278B output is 16-bit offset serial via YAC512 DAC, but only
            // 2 of the 4 outputs are exposed
            out.data[0] = full.data[0];
            out.data[1] = full.data[1];
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

// ---- YMF278B: not yet ported ----

//*********************************************************
//  OPLL BASE
//*********************************************************

/// The OPLL core every OPLL variant shares: nine 2-operator channels, or six
/// and five rhythm sounds, playing instruments from a built-in set of
/// fifteen, three rhythm ones and one the registers define; two outputs, the
/// melody and the rhythm, through a built-in 9-bit DAC. The chips differ
/// only in their instrument sets; this takes any set.
#[derive(Clone)]
pub struct OpllBase<I: Interface = NullInterface> {
    /// The address register.
    address: u8,
    /// The core FM engine.
    fm: FmEngine<OpllRegisters, I>,
}

impl<I: Interface> OpllBase<I> {
    /// The number of outputs: the melody, then the rhythm.
    pub const OUTPUTS: usize = 2;

    /// Creates a chip that talks to the system through `intf` and plays the
    /// instrument set `instruments`, reset. The set is the fifteen melodic
    /// instruments, then the bass drum, the high hat and snare drum, and the
    /// tom and top cymbal, eight bytes each in the layout of registers 0 to
    /// 7.
    pub fn new(intf: I, instruments: &[u8; OPLL_INSTRUMENT_DATA_SIZE]) -> Self {
        let mut chip = OpllBase {
            address: 0,
            fm: FmEngine::new(intf),
        };
        chip.set_instrument_data(instruments);
        chip.reset();
        chip
    }

    /// Replaces the instrument set. As in ymfm this takes effect as the
    /// cached operator data is next refreshed, on the next register write
    /// or `invalidate_caches`.
    pub fn set_instrument_data(&mut self, instruments: &[u8; OPLL_INSTRUMENT_DATA_SIZE]) {
        self.fm.regs_mut().set_instrument_data(instruments);
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

    /// Reads the status register; the OPLL has none, and this gives 0.
    pub fn read_status(&mut self) -> u8 {
        0x00
    }

    /// Reads from the chip; the OPLL has nothing to read, and this gives 0.
    pub fn read(&mut self, _offset: u32) -> u8 {
        0x00
    }

    /// Writes the address register.
    pub fn write_address(&mut self, data: u8) {
        // OPLL doesn't expose a busy signal, but datasheets are pretty
        // consistent in indicating that address writes should be no faster
        // than every 12 clocks
        self.fm.intf_mut().set_busy_end(12);

        // just set the address
        self.address = data;
    }

    /// Writes the data register.
    pub fn write_data(&mut self, data: u8) {
        // OPLL doesn't expose a busy signal, but datasheets are pretty
        // consistent in indicating that data writes should be no faster than
        // every 84 clocks
        self.fm.intf_mut().set_busy_end(84);

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
    pub fn generate(&mut self, output: &mut [Output<2>]) {
        for out in output.iter_mut() {
            // clock the system
            self.fm.clock(FmEngine::<OpllRegisters, I>::ALL_CHANNELS);

            // update the FM content; OPLL has a built-in 9-bit DAC
            self.fm.output(
                &mut out.clear().data,
                5,
                256,
                FmEngine::<OpllRegisters, I>::ALL_CHANNELS,
            );

            // final output is multiplexed; ymfm doesn't simulate that except
            // to average over everything
            out.data[0] = (out.data[0] * 128) / 9;
            out.data[1] = (out.data[1] * 128) / 9;
        }
    }

    /// Reports the expiry of timer `tnum`; the OPLL has no timers, and
    /// never asks for one.
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

/// Defines an OPLL variant: `OpllBase` with its own default instrument set.
macro_rules! opll_variant {
    ($(#[$doc:meta])* $chip:ident, $rom:ident) => {
        $(#[$doc])*
        #[derive(Clone)]
        pub struct $chip<I: Interface = NullInterface>(OpllBase<I>);

        impl<I: Interface> $chip<I> {
            /// The number of outputs: the melody, then the rhythm.
            pub const OUTPUTS: usize = 2;

            /// The built-in instrument set, in the layout `with_instruments`
            /// takes.
            pub const DEFAULT_INSTRUMENTS: &'static [u8; OPLL_INSTRUMENT_DATA_SIZE] = &$rom;

            /// Creates a chip that talks to the system through `intf`, with
            /// its built-in instrument set, reset.
            pub fn new(intf: I) -> Self {
                $chip(OpllBase::new(intf, &$rom))
            }

            /// Creates a chip that plays `instruments` in place of its
            /// built-in set, reset.
            pub fn with_instruments(
                intf: I,
                instruments: &[u8; OPLL_INSTRUMENT_DATA_SIZE],
            ) -> Self {
                $chip(OpllBase::new(intf, instruments))
            }

            /// Replaces the instrument set; see `OpllBase::set_instrument_data`.
            pub fn set_instrument_data(&mut self, instruments: &[u8; OPLL_INSTRUMENT_DATA_SIZE]) {
                self.0.set_instrument_data(instruments);
            }

            /// Resets the chip.
            pub fn reset(&mut self) {
                self.0.reset();
            }

            /// Returns the output sample rate for an input clock.
            pub fn sample_rate(&self, input_clock: u32) -> u32 {
                self.0.sample_rate(input_clock)
            }

            /// Invalidates the cached operator data.
            pub fn invalidate_caches(&mut self) {
                self.0.invalidate_caches();
            }

            /// Reads the status register; the OPLL has none, and this gives
            /// 0.
            pub fn read_status(&mut self) -> u8 {
                self.0.read_status()
            }

            /// Reads from the chip; the OPLL has nothing to read, and this
            /// gives 0.
            pub fn read(&mut self, offset: u32) -> u8 {
                self.0.read(offset)
            }

            /// Writes the address register.
            pub fn write_address(&mut self, data: u8) {
                self.0.write_address(data);
            }

            /// Writes the data register.
            pub fn write_data(&mut self, data: u8) {
                self.0.write_data(data);
            }

            /// Writes to the chip: the address port at offset 0, the data
            /// port at 1.
            pub fn write(&mut self, offset: u32, data: u8) {
                self.0.write(offset, data);
            }

            /// Generates one sample into each element of `output`.
            pub fn generate(&mut self, output: &mut [Output<2>]) {
                self.0.generate(output);
            }

            /// Reports the expiry of timer `tnum`; the OPLL has no timers,
            /// and never asks for one.
            pub fn timer_expired(&mut self, tnum: u32) {
                self.0.timer_expired(tnum);
            }

            /// Returns the interface.
            pub fn intf(&self) -> &I {
                self.0.intf()
            }

            /// Returns the interface, mutably.
            pub fn intf_mut(&mut self) -> &mut I {
                self.0.intf_mut()
            }

            /// Returns the register file as last written, for a diagnostic.
            pub fn registers(&self) -> &[u8] {
                self.0.registers()
            }
        }
    };
}

//*********************************************************
//  YM2413
//*********************************************************

opll_variant!(
    /// The Yamaha YM2413 (OPLL), with ymfm's copyright-free stand-in for its
    /// instrument ROM.
    Ym2413,
    YM2413_INSTRUMENTS
);

// table below taken from
// https://github.com/plgDavid/misc/wiki/Copyright-free-OPLL(x)-ROM-patches
#[rustfmt::skip]
const YM2413_INSTRUMENTS: [u8; OPLL_INSTRUMENT_DATA_SIZE] = [
    //April 2015 David Viens, tweaked May 19-21th 2015 Hubert Lamontagne
    0x71, 0x61, 0x1E, 0x17, 0xEF, 0x7F, 0x00, 0x17, //Violin
    0x13, 0x41, 0x1A, 0x0D, 0xF8, 0xF7, 0x23, 0x13, //Guitar
    0x13, 0x01, 0x99, 0x00, 0xF2, 0xC4, 0x11, 0x23, //Piano
    0x31, 0x61, 0x0E, 0x07, 0x98, 0x64, 0x70, 0x27, //Flute
    0x22, 0x21, 0x1E, 0x06, 0xBF, 0x76, 0x00, 0x28, //Clarinet
    0x31, 0x22, 0x16, 0x05, 0xE0, 0x71, 0x0F, 0x18, //Oboe
    0x21, 0x61, 0x1D, 0x07, 0x82, 0x8F, 0x10, 0x07, //Trumpet
    0x23, 0x21, 0x2D, 0x14, 0xFF, 0x7F, 0x00, 0x07, //Organ
    0x41, 0x61, 0x1B, 0x06, 0x64, 0x65, 0x10, 0x17, //Horn
    0x61, 0x61, 0x0B, 0x18, 0x85, 0xFF, 0x81, 0x07, //Synthesizer
    0x13, 0x01, 0x83, 0x11, 0xFA, 0xE4, 0x10, 0x04, //Harpsichord
    0x17, 0x81, 0x23, 0x07, 0xF8, 0xF8, 0x22, 0x12, //Vibraphone
    0x61, 0x50, 0x0C, 0x05, 0xF2, 0xF5, 0x29, 0x42, //Synthesizer Bass
    0x01, 0x01, 0x54, 0x03, 0xC3, 0x92, 0x03, 0x02, //Acoustic Bass
    0x41, 0x41, 0x89, 0x03, 0xF1, 0xE5, 0x11, 0x13, //Electric Guitar
    0x01, 0x01, 0x18, 0x0F, 0xDF, 0xF8, 0x6A, 0x6D, //rhythm 1
    0x01, 0x01, 0x00, 0x00, 0xC8, 0xD8, 0xA7, 0x48, //rhythm 2
    0x05, 0x01, 0x00, 0x00, 0xF8, 0xAA, 0x59, 0x55, //rhythm 3
];

//*********************************************************
//  YM2423
//*********************************************************

opll_variant!(
    /// The Yamaha YM2423 (OPLL-X), with ymfm's copyright-free stand-in for
    /// its instrument ROM.
    Ym2423,
    YM2423_INSTRUMENTS
);

// table below taken from
// https://github.com/plgDavid/misc/wiki/Copyright-free-OPLL(x)-ROM-patches
#[rustfmt::skip]
const YM2423_INSTRUMENTS: [u8; OPLL_INSTRUMENT_DATA_SIZE] = [
    // May 4-6 2016 Hubert Lamontagne
    // Doesn't seem to have any diff between opllx-x and opllx-y
    // Drums seem identical to regular opll
    0x61, 0x61, 0x1B, 0x07, 0x94, 0x5F, 0x10, 0x06, //1 Strings Saw wave with vibrato Violin
    0x93, 0xB1, 0x51, 0x04, 0xF3, 0xF2, 0x70, 0xFB, //2 Guitar Jazz GuitarPiano
    0x41, 0x21, 0x11, 0x85, 0xF2, 0xF2, 0x70, 0x75, //3 Electric Guitar Same as OPLL No.15 Synth
    0x93, 0xB2, 0x28, 0x07, 0xF3, 0xF2, 0x70, 0xB4, //4 Electric Piano 2 Slow attack, tremoloDing-a-ling
    0x72, 0x31, 0x97, 0x05, 0x51, 0x6F, 0x60, 0x09, //5  Flute Same as OPLL No.4Clarinet
    0x13, 0x30, 0x18, 0x06, 0xF7, 0xF4, 0x50, 0x85, //6 Marimba  Also be used as steel drumXyophone
    0x51, 0x31, 0x1C, 0x07, 0x51, 0x71, 0x20, 0x26, //7 Trumpet  Same as OPLL No.7Trumpet
    0x41, 0xF4, 0x1B, 0x07, 0x74, 0x34, 0x00, 0x06, //8 Harmonica Harmonica synth
    0x50, 0x30, 0x4D, 0x03, 0x42, 0x65, 0x20, 0x06, //9 Tuba Tuba
    0x40, 0x20, 0x10, 0x85, 0xF3, 0xF5, 0x20, 0x04, //10  Synth Brass 2 Synth sweep
    0x61, 0x61, 0x1B, 0x07, 0xC5, 0x96, 0xF3, 0xF6, //11  Short Saw Saw wave with short envelopeSynth hit
    0xF9, 0xF1, 0xDC, 0x00, 0xF5, 0xF3, 0x77, 0xF2, //12  Vibraphone Bright vibraphoneVibes
    0x60, 0xA2, 0x91, 0x03, 0x94, 0xC1, 0xF7, 0xF7, //13  Electric Guitar 2 Clean guitar with feedbackHarmonic bass
    0x30, 0x30, 0x17, 0x06, 0xF3, 0xF1, 0xB7, 0xFC, //14  Synth Bass 2Snappy bass
    0x31, 0x36, 0x0D, 0x05, 0xF2, 0xF4, 0x27, 0x9C, //15  Sitar Also be used as ShamisenBanjo
    0x01, 0x01, 0x18, 0x0F, 0xDF, 0xF8, 0x6A, 0x6D, //rhythm 1
    0x01, 0x01, 0x00, 0x00, 0xC8, 0xD8, 0xA7, 0x48, //rhythm 2
    0x05, 0x01, 0x00, 0x00, 0xF8, 0xAA, 0x59, 0x55, //rhythm 3
];

//*********************************************************
//  YMF281
//*********************************************************

opll_variant!(
    /// The Yamaha YMF281 (OPLLP), with ymfm's copyright-free stand-in for
    /// its instrument ROM.
    Ymf281,
    YMF281_INSTRUMENTS
);

// table below taken from
// https://github.com/plgDavid/misc/wiki/Copyright-free-OPLL(x)-ROM-patches
#[rustfmt::skip]
const YMF281_INSTRUMENTS: [u8; OPLL_INSTRUMENT_DATA_SIZE] = [
    // May 14th 2015 Hubert Lamontagne
    0x72, 0x21, 0x1A, 0x07, 0xF6, 0x64, 0x01, 0x16, // Clarinet ~~ Electric String  Square wave with vibrato
    0x00, 0x10, 0x45, 0x00, 0xF6, 0x83, 0x73, 0x63, // Synth Bass ~~ Bow wow  Triangular wave
    0x13, 0x01, 0x96, 0x00, 0xF1, 0xF4, 0x31, 0x23, // Piano ~~ Electric Guitar  Despite of its name, same as Piano of YM2413.
    0x71, 0x21, 0x0B, 0x0F, 0xF9, 0x64, 0x70, 0x17, // Flute ~~ Organ  Sine wave
    0x02, 0x21, 0x1E, 0x06, 0xF9, 0x76, 0x00, 0x28, // Square Wave ~~ Clarinet  Same as ones of YM2413.
    0x00, 0x61, 0x82, 0x0E, 0xF9, 0x61, 0x20, 0x27, // Space Oboe ~~ Saxophone  Saw wave with vibrato
    0x21, 0x61, 0x1B, 0x07, 0x84, 0x8F, 0x10, 0x07, // Trumpet ~~ Trumpet  Same as ones of YM2413.
    0x37, 0x32, 0xCA, 0x02, 0x66, 0x64, 0x47, 0x29, // Wow Bell ~~ Street Organ  Calliope
    0x41, 0x41, 0x07, 0x03, 0xF5, 0x70, 0x51, 0xF5, // Electric Guitar ~~ Synth Brass  Same as Synthesizer of YM2413.
    0x36, 0x01, 0x5E, 0x07, 0xF2, 0xF3, 0xF7, 0xF7, // Vibes ~~ Electric Piano  Simulate of Rhodes Piano
    0x00, 0x00, 0x18, 0x06, 0xC5, 0xF3, 0x20, 0xF2, // Bass ~~ Bass  Electric bass
    0x17, 0x81, 0x25, 0x07, 0xF7, 0xF3, 0x21, 0xF7, // Vibraphone ~~ Vibraphone Same as ones of YM2413.
    0x35, 0x64, 0x00, 0x00, 0xFF, 0xF3, 0x77, 0xF5, // Vibrato Bell ~~ Chime  Bell
    0x11, 0x31, 0x00, 0x07, 0xDD, 0xF3, 0xFF, 0xFB, // Click Sine ~~ Tom Tom II  Tom
    0x3A, 0x21, 0x00, 0x07, 0x95, 0x84, 0x0F, 0xF5, // Noise and Tone ~~ Noise  for S.E.
    0x01, 0x01, 0x18, 0x0F, 0xDF, 0xF8, 0x6A, 0x6D, //rhythm 1
    0x01, 0x01, 0x00, 0x00, 0xC8, 0xD8, 0xA7, 0x48, //rhythm 2
    0x05, 0x01, 0x00, 0x00, 0xF8, 0xAA, 0x59, 0x55, //rhythm 3
];

//*********************************************************
//  DS1001
//*********************************************************

opll_variant!(
    /// The Yamaha DS1001 (Konami VRC7), with ymfm's copyright-free stand-in
    /// for its instrument ROM.
    Ds1001,
    DS1001_INSTRUMENTS
);

// table below taken from
// https://github.com/plgDavid/misc/wiki/Copyright-free-OPLL(x)-ROM-patches
#[rustfmt::skip]
const DS1001_INSTRUMENTS: [u8; OPLL_INSTRUMENT_DATA_SIZE] = [
    // May 15th 2015 Hubert Lamontagne & David Viens
    0x03, 0x21, 0x05, 0x06, 0xC8, 0x81, 0x42, 0x27, // Buzzy Bell
    0x13, 0x41, 0x14, 0x0D, 0xF8, 0xF7, 0x23, 0x12, // Guitar
    0x31, 0x11, 0x08, 0x08, 0xFA, 0xC2, 0x28, 0x22, // Wurly
    0x31, 0x61, 0x0C, 0x07, 0xF8, 0x64, 0x60, 0x27, // Flute
    0x22, 0x21, 0x1E, 0x06, 0xFF, 0x76, 0x00, 0x28, // Clarinet
    0x02, 0x01, 0x05, 0x00, 0xAC, 0xF2, 0x03, 0x02, // Synth
    0x21, 0x61, 0x1D, 0x07, 0x82, 0x8F, 0x10, 0x07, // Trumpet
    0x23, 0x21, 0x22, 0x17, 0xFF, 0x73, 0x00, 0x17, // Organ
    0x15, 0x11, 0x25, 0x00, 0x41, 0x71, 0x00, 0xF1, // Bells
    0x95, 0x01, 0x10, 0x0F, 0xB8, 0xAA, 0x50, 0x02, // Vibes
    0x17, 0xC1, 0x5E, 0x07, 0xFA, 0xF8, 0x22, 0x12, // Vibraphone
    0x71, 0x23, 0x11, 0x06, 0x65, 0x74, 0x10, 0x16, // Tutti
    0x01, 0x02, 0xD3, 0x05, 0xF3, 0x92, 0x83, 0xF2, // Fretless
    0x61, 0x63, 0x0C, 0x00, 0xA4, 0xFF, 0x30, 0x06, // Synth Bass
    0x21, 0x62, 0x0D, 0x00, 0xA1, 0xFF, 0x50, 0x08, // Sweep
    0x01, 0x01, 0x18, 0x0F, 0xDF, 0xF8, 0x6A, 0x6D, //rhythm 1
    0x01, 0x01, 0x00, 0x00, 0xC8, 0xD8, 0xA7, 0x48, //rhythm 2
    0x05, 0x01, 0x00, 0x00, 0xF8, 0xAA, 0x59, 0x55, //rhythm 3
];
