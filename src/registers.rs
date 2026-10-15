// This file is a Rust translation of ymfm's `ymfm_opl.h` and `ymfm_opl.cpp`
// at commit 81aec25 (https://github.com/aaronsgiles/ymfm), with changes: it
// keeps the OPL2 (YM3812) and OPL3 (YMF262) revisions only, as a const
// generic over the revision rather than a template parameter; the waveform
// tables are computed once at compile time and shared rather than held per
// chip; reads, status, timers, busy time and save states are gone; and a
// register write returns its key-on target rather than writing through output
// parameters. Every computation that reaches a sample is kept as ymfm writes
// it, so the output matches ymfm sample for sample.
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

//! The OPL register file, and the YM3812 and YMF262 around the FM core.
//!
//! The register map, as ymfm documents it:
//!
//! ```text
//! System-wide registers:
//!      01 --x----- Enable waveform select [OPL2 only]
//!      04 x------- IRQ reset, and timer bits this translation ignores
//!      08 -x------ Note select
//!      BD x------- AM depth
//!         -x------ PM depth
//!         --x----- Rhythm enable
//!         ---x---- Bass drum key on
//!         ----x--- Snare drum key on
//!         -----x-- Tom key on
//!         ------x- Top cymbal key on
//!         -------x High hat key on
//!     104 --xxxxxx 4-operator mode for channels 6 to 1 [OPL3 only]
//!     105 -------x New [OPL3 only]
//!
//! Per-channel registers, channel in address bits 0-3, again at +100 on OPL3:
//!   A0-A8 xxxxxxxx F-number (low 8 bits)
//!   B0-B8 --x----- Key on
//!         ---xxx-- Block (octave, 0-7)
//!         ------xx F-number (high two bits)
//!   C0-C8 x------- CHD output [OPL3 only]
//!         -x------ CHC output [OPL3 only]
//!         --x----- CHB output, mixed right [OPL3 only]
//!         ---x---- CHA output, mixed left [OPL3 only]
//!         ----xxx- Feedback level for operator 1 (0-7)
//!         -------x Operator connection algorithm
//!
//! Per-operator registers, operator in bits 0-5, again at +100 on OPL3:
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
//!   E0-F5 ------xx Wave select (0-3) [OPL2]
//!         -----xxx Wave select (0-7) [OPL3]
//! ```

use super::fm::{
    abs_sin_attenuation, bitfield, roundtrip_fp, Cache, Engine, Output, EG_ATTACK, EG_DECAY,
    EG_DEPRESS, EG_RELEASE, EG_SUSTAIN, PHASE_STEP_DYNAMIC, WAVEFORM_LENGTH,
};

/// The key-on target a write to 0xBD reports, which keys the rhythm section.
pub(crate) const RHYTHM_CHANNEL: u32 = 0xff;

/// The eight OPL waveforms, of which the OPL2 offers the first four.
const WAVEFORMS: [[u16; WAVEFORM_LENGTH as usize]; 8] = {
    let length = WAVEFORM_LENGTH as usize;
    let mut table = [[0u16; WAVEFORM_LENGTH as usize]; 8];
    let mut index = 0;
    while index < length {
        let i = index as u32;
        table[0][index] = (abs_sin_attenuation(i) | (bitfield(i, 9, 1) << 15)) as u16;
        index += 1;
    }
    let zeroval = table[0][0];
    let mut index = 0;
    while index < length {
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
            table[0][(index * 2) % length]
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

/// Converts the top four bits of the frequency number and the block to a key
/// scale attenuation, 6 dB an octave in 0.75 dB steps.
fn opl_key_scale_atten(block: u32, fnum_4msb: u32) -> u32 {
    // The maximal values, for block 7; each block below subtracts 8.
    const FNUM_TO_ATTEN: [u8; 16] = [
        0, 24, 32, 37, 40, 43, 45, 47, 48, 50, 51, 52, 53, 54, 55, 56,
    ];
    let result = i32::from(FNUM_TO_ATTEN[fnum_4msb as usize]) - 8 * (block ^ 7) as i32;
    result.max(0) as u32
}

/// Computes the phase step from the block and frequency number, the x.1
/// multiple and the PM LFO.
fn opl_compute_phase_step(block_freq: u32, multiple: u32, lfo_raw_pm: i32) -> u32 {
    // The frequency number as a 12-bit fraction.
    let mut fnum = bitfield(block_freq, 0, 10) << 2;
    // ymfm multiplies an int32_t by a uint32_t, which C does unsigned, and
    // keeps twelve bits, so wrapping unsigned arithmetic gives the same bits.
    fnum = fnum.wrapping_add((lfo_raw_pm as u32).wrapping_mul(bitfield(block_freq, 7, 3)) >> 1);
    fnum &= 0xfff;
    let block = bitfield(block_freq, 10, 3);
    let phase_step = (fnum << block) >> 2;
    (phase_step * multiple) >> 1
}

/// The register file of one OPL2 or OPL3, with its LFOs and noise generator.
#[derive(Clone)]
pub(crate) struct Registers<const OPL3: bool> {
    lfo_am_counter: u16,
    lfo_pm_counter: u16,
    noise_lfsr: u32,
    lfo_am: u8,
    regdata: [u8; 0x200],
}

impl<const OPL3: bool> Registers<OPL3> {
    pub(crate) const CHANNELS: usize = if OPL3 { 18 } else { 9 };
    pub(crate) const REG_MODE: u32 = 0x04;
    /// The waveforms a register can select.
    const WAVEFORMS: u32 = if OPL3 { 8 } else { 4 };

    pub(crate) fn new() -> Registers<OPL3> {
        // ymfm leaves the register file uninitialized until `reset`, except
        // for 0x104, which the constructor's operator map reads.
        Registers {
            lfo_am_counter: 0,
            lfo_pm_counter: 0,
            noise_lfsr: 1,
            lfo_am: 0,
            regdata: [0; 0x200],
        }
    }

    pub(crate) fn reset(&mut self) {
        self.regdata = [0; 0x200];
    }

    /// Maps a channel number to its register offset.
    pub(crate) const fn channel_offset(chnum: u32) -> u32 {
        if OPL3 {
            (chnum % 9) + 0x100 * (chnum / 9)
        } else {
            chnum
        }
    }

    /// Maps an operator number to its register offset.
    pub(crate) const fn operator_offset(opnum: u32) -> u32 {
        if OPL3 {
            (opnum % 18) + 2 * ((opnum % 18) / 6) + 0x100 * (opnum / 18)
        } else {
            opnum + 2 * (opnum / 6)
        }
    }

    /// Lists each channel's operators, four 8-bit indices packed per channel,
    /// with 0xff for an empty slot.
    pub(crate) fn operator_map(&self) -> [u32; 18] {
        const fn list(o1: u32, o2: u32, o3: u32, o4: u32) -> u32 {
            o1 | (o2 << 8) | (o3 << 16) | (o4 << 24)
        }
        const NONE: u32 = 0xff;
        const fn pair(o1: u32, o2: u32) -> u32 {
            list(o1, o2, NONE, NONE)
        }
        const EMPTY: u32 = list(NONE, NONE, NONE, NONE);
        let mut map = [EMPTY; 18];
        if !OPL3 {
            // The OPL2 map is fixed, every channel 2-operator.
            map[..9].copy_from_slice(&[
                pair(0, 3),
                pair(1, 4),
                pair(2, 5),
                pair(6, 9),
                pair(7, 10),
                pair(8, 11),
                pair(12, 15),
                pair(13, 16),
                pair(14, 17),
            ]);
            return map;
        }
        let fourop = self.fourop_enable();
        let four = |bit: u32| bitfield(fourop, bit, 1) != 0;
        map[0] = if four(0) {
            list(0, 3, 6, 9)
        } else {
            pair(0, 3)
        };
        map[1] = if four(1) {
            list(1, 4, 7, 10)
        } else {
            pair(1, 4)
        };
        map[2] = if four(2) {
            list(2, 5, 8, 11)
        } else {
            pair(2, 5)
        };
        map[3] = if four(0) { EMPTY } else { pair(6, 9) };
        map[4] = if four(1) { EMPTY } else { pair(7, 10) };
        map[5] = if four(2) { EMPTY } else { pair(8, 11) };
        map[6] = pair(12, 15);
        map[7] = pair(13, 16);
        map[8] = pair(14, 17);
        map[9] = if four(3) {
            list(18, 21, 24, 27)
        } else {
            pair(18, 21)
        };
        map[10] = if four(4) {
            list(19, 22, 25, 28)
        } else {
            pair(19, 22)
        };
        map[11] = if four(5) {
            list(20, 23, 26, 29)
        } else {
            pair(20, 23)
        };
        map[12] = if four(3) { EMPTY } else { pair(24, 27) };
        map[13] = if four(4) { EMPTY } else { pair(25, 28) };
        map[14] = if four(5) { EMPTY } else { pair(26, 29) };
        map[15] = pair(30, 33);
        map[16] = pair(31, 34);
        map[17] = pair(32, 35);
        map
    }

    /// Writes a register and returns the key-on target it carries: a channel
    /// and an operator mask, or `RHYTHM_CHANNEL` and the rhythm bits.
    pub(crate) fn write(&mut self, index: u16, data: u8) -> Option<(u32, u32)> {
        let index = u32::from(index);
        // A write to the mode register with the high bit set ignores the rest.
        if index == Self::REG_MODE && bitfield(u32::from(data), 7, 1) != 0 {
            self.regdata[index as usize] |= 0x80;
        } else {
            self.regdata[index as usize] = data;
        }
        let data = u32::from(data);

        if index == 0xbd {
            let opmask = if bitfield(data, 5, 1) != 0 {
                bitfield(data, 0, 5)
            } else {
                0
            };
            return Some((RHYTHM_CHANNEL, opmask));
        }
        if (index & 0xf0) == 0xb0 {
            let mut channel = index & 0x0f;
            if channel < 9 {
                if OPL3 {
                    channel += 9 * bitfield(index, 8, 1);
                }
                let opmask = if bitfield(data, 5, 1) != 0 { 15 } else { 0 };
                return Some((channel, opmask));
            }
        }
        None
    }

    /// Clocks the noise generator and both LFOs, and returns the PM value.
    pub(crate) fn clock_noise_and_lfo(&mut self) -> i32 {
        // A 23-bit noise generator at a constant rate, for the rhythm section.
        self.noise_lfsr <<= 1;
        self.noise_lfsr |= bitfield(self.noise_lfsr, 23, 1)
            ^ bitfield(self.noise_lfsr, 9, 1)
            ^ bitfield(self.noise_lfsr, 8, 1)
            ^ bitfield(self.noise_lfsr, 1, 1);

        // The AM LFO has 210 * 64 steps, 3.7 Hz at a nominal 50 kHz.
        let am_counter = u32::from(self.lfo_am_counter);
        self.lfo_am_counter = self.lfo_am_counter.wrapping_add(1);
        if am_counter >= 210 * 64 - 1 {
            self.lfo_am_counter = 0;
        }
        // The low eight bits are a fraction; depth 0 divides by two and
        // depth 1 multiplies by two.
        let shift = 9 - 2 * self.lfo_am_depth();
        // A triangle: the counter inverted across its midpoint.
        let triangle = if am_counter < 105 * 64 {
            am_counter
        } else {
            210 * 64 + 63 - am_counter
        };
        self.lfo_am = (triangle >> shift) as u8;

        // The PM LFO has 8192 steps in eight chunks, 6.1 Hz nominal; its value
        // is a 1.3 fraction and sign applied to the top bits of the FNUM.
        let pm_counter = u32::from(self.lfo_pm_counter);
        self.lfo_pm_counter = self.lfo_pm_counter.wrapping_add(1);
        const PM_SCALE: [i8; 8] = [8, 4, 0, -4, -8, -4, 0, 4];
        i32::from(PM_SCALE[bitfield(pm_counter, 10, 3) as usize]) >> (self.lfo_pm_depth() ^ 1)
    }

    /// Returns the AM offset, which on the OPL is the same for every channel.
    pub(crate) fn lfo_am_offset(&self) -> u32 {
        u32::from(self.lfo_am)
    }

    /// Returns the noise generator's output, in bit 0.
    pub(crate) fn noise_state(&self) -> u32 {
        self.noise_lfsr >> 23
    }

    /// Returns a waveform table by the index an operator cached.
    pub(crate) fn waveform(&self, index: u8) -> &'static [u16; WAVEFORM_LENGTH as usize] {
        &WAVEFORMS[index as usize]
    }

    /// Fills an operator's cache from the registers.
    pub(crate) fn cache_operator_data(&self, choffs: u32, opoffs: u32, cache: &mut Cache) {
        cache.waveform = (self.op_waveform(opoffs) % Self::WAVEFORMS) as u8;

        let block_freq = self.ch_block_freq(choffs);
        cache.block_freq = block_freq;

        // The 4-bit keycode is the block plus one of the next two bits, which
        // note select picks; ymfm notes the bit is reversed from the manual.
        let mut keycode = bitfield(block_freq, 10, 3) << 1;
        keycode |= bitfield(block_freq, 9 - self.note_select(), 1);

        // The multiple as an x.1 value, where 0 means 0.5; the low bit comes
        // from a table giving 0,1,2,3,4,5,6,7,8,9,10,10,12,12,15,15.
        let multiple = self.op_multiple(opoffs);
        cache.multiple = ((multiple & 0xe) | bitfield(0xc2aa, multiple, 1)) * 2;
        if cache.multiple == 0 {
            cache.multiple = 1;
        }

        cache.phase_step = if self.op_lfo_pm_enable(opoffs) == 0 {
            opl_compute_phase_step(cache.block_freq, cache.multiple, 0)
        } else {
            PHASE_STEP_DYNAMIC
        };

        cache.total_level = self.op_total_level(opoffs) << 3;
        let ksl = self.op_ksl(opoffs);
        if ksl != 0 {
            cache.total_level +=
                opl_key_scale_atten(bitfield(block_freq, 10, 3), bitfield(block_freq, 6, 4)) << ksl;
        }

        // A 4-bit sustain level where 15 means 31, so effectively five bits.
        let mut sustain = self.op_sustain_level(opoffs);
        sustain |= (sustain + 1) & 0x10;
        cache.eg_sustain = sustain << 5;

        let ksrval = keycode >> (2 * (self.op_ksr(opoffs) ^ 1));
        let effective_rate = |rawrate: u32| -> u8 {
            if rawrate == 0 {
                0
            } else {
                (rawrate + ksrval).min(63) as u8
            }
        };
        cache.eg_rate[EG_ATTACK] = effective_rate(self.op_attack_rate(opoffs) * 4);
        cache.eg_rate[EG_DECAY] = effective_rate(self.op_decay_rate(opoffs) * 4);
        cache.eg_rate[EG_SUSTAIN] = if self.op_eg_sustain(opoffs) != 0 {
            0
        } else {
            effective_rate(self.op_release_rate(opoffs) * 4)
        };
        cache.eg_rate[EG_RELEASE] = effective_rate(self.op_release_rate(opoffs) * 4);
        cache.eg_rate[EG_DEPRESS] = 0x3f;
    }

    /// Computes the phase step for a PM value.
    pub(crate) fn compute_phase_step(&self, opoffs: u32, cache: &Cache, lfo_raw_pm: i32) -> u32 {
        let pm = if self.op_lfo_pm_enable(opoffs) != 0 {
            lfo_raw_pm
        } else {
            0
        };
        opl_compute_phase_step(cache.block_freq, cache.multiple, pm)
    }

    fn byte(&self, offset: u32, start: u32, count: u32, extra_offset: u32) -> u32 {
        bitfield(
            u32::from(self.regdata[(offset + extra_offset) as usize]),
            start,
            count,
        )
    }

    fn waveform_enable(&self) -> u32 {
        if OPL3 {
            1
        } else {
            self.byte(0x01, 5, 1, 0)
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

    pub(crate) fn rhythm_enable(&self) -> u32 {
        self.byte(0xbd, 5, 1, 0)
    }

    pub(crate) fn newflag(&self) -> u32 {
        if OPL3 {
            self.byte(0x105, 0, 1, 0)
        } else {
            0
        }
    }

    fn fourop_enable(&self) -> u32 {
        if OPL3 {
            self.byte(0x104, 0, 6, 0)
        } else {
            0
        }
    }

    fn ch_block_freq(&self, choffs: u32) -> u32 {
        (self.byte(0xb0, 0, 5, choffs) << 8) | self.byte(0xa0, 0, 8, choffs)
    }

    pub(crate) fn ch_feedback(&self, choffs: u32) -> u32 {
        self.byte(0xc0, 1, 3, choffs)
    }

    pub(crate) fn ch_algorithm(&self, choffs: u32) -> u32 {
        let base = self.byte(0xc0, 0, 1, choffs);
        if OPL3 {
            base | 8 | (self.byte(0xc3, 0, 1, choffs) << 1)
        } else {
            base
        }
    }

    pub(crate) fn ch_output_any(&self, choffs: u32) -> u32 {
        if self.newflag() != 0 {
            self.byte(0xc0 + choffs, 4, 4, 0)
        } else {
            1
        }
    }

    pub(crate) fn ch_output_0(&self, choffs: u32) -> u32 {
        if self.newflag() != 0 {
            self.byte(0xc0 + choffs, 4, 1, 0)
        } else {
            1
        }
    }

    pub(crate) fn ch_output_1(&self, choffs: u32) -> u32 {
        if self.newflag() != 0 {
            self.byte(0xc0 + choffs, 5, 1, 0)
        } else if OPL3 {
            1
        } else {
            0
        }
    }

    pub(crate) fn ch_output_2(&self, choffs: u32) -> u32 {
        if self.newflag() != 0 {
            self.byte(0xc0 + choffs, 6, 1, 0)
        } else {
            0
        }
    }

    pub(crate) fn ch_output_3(&self, choffs: u32) -> u32 {
        if self.newflag() != 0 {
            self.byte(0xc0 + choffs, 7, 1, 0)
        } else {
            0
        }
    }

    pub(crate) fn op_lfo_am_enable(&self, opoffs: u32) -> u32 {
        self.byte(0x20, 7, 1, opoffs)
    }

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
        if self.waveform_enable() == 0 {
            return 0;
        }
        let bits = if self.newflag() != 0 { 3 } else { 2 };
        self.byte(0xe0, 0, bits, opoffs)
    }

    /// Returns the raw register file, for a diagnostic.
    pub(crate) fn data(&self) -> &[u8; 0x200] {
        &self.regdata
    }
}

/// The Yamaha YM3812 (OPL2): nine channels, one output through a
/// floating-point DAC.
#[derive(Clone)]
pub(crate) struct Ym3812 {
    address: u8,
    fm: Engine<false>,
}

impl Ym3812 {
    /// Creates a chip in its power-on state.
    pub(crate) fn new() -> Ym3812 {
        let mut chip = Ym3812 {
            address: 0,
            fm: Engine::new(),
        };
        chip.fm.reset();
        chip
    }

    /// Writes to the address port at offset 0 or the data port at offset 1.
    pub(crate) fn write(&mut self, offset: u32, data: u8) {
        if offset & 1 == 0 {
            self.address = data;
        } else {
            self.fm.write(u16::from(self.address), data);
        }
    }

    /// Generates one sample.
    pub(crate) fn generate(&mut self) -> i32 {
        let all = (1 << Engine::<false>::CHANNELS) - 1;
        self.fm.clock(all);
        let mut output: Output = [0; 4];
        self.fm.output(&mut output, 1, 32767, all);
        // The YM3014 DAC takes a mantissa and exponent, so the round trip
        // through that format is what reaches the output.
        roundtrip_fp(output[0])
    }

    pub(crate) fn registers(&self) -> &[u8; 0x200] {
        self.fm.regs.data()
    }
}

/// The Yamaha YMF262 (OPL3): eighteen channels, 4-operator pairs and four
/// outputs, of which A is the left and B the right of a stereo pair.
#[derive(Clone)]
pub(crate) struct Ymf262 {
    address: u16,
    fm: Engine<true>,
}

impl Ymf262 {
    /// Creates a chip in its power-on state.
    pub(crate) fn new() -> Ymf262 {
        let mut chip = Ymf262 {
            address: 0,
            fm: Engine::new(),
        };
        chip.fm.reset();
        chip
    }

    /// Writes to one of the four ports: 0 and 2 are the low and high address
    /// ports, and 1 and 3 the data ports.
    pub(crate) fn write(&mut self, offset: u32, data: u8) {
        match offset & 3 {
            0 => self.address = u16::from(data),
            2 => {
                self.address = u16::from(data) | 0x100;
                // In compatibility mode the bank bit is masked, except for
                // 0x105, which is how the mode is left.
                if self.fm.regs.newflag() == 0 && self.address != 0x105 {
                    self.address &= 0xff;
                }
            }
            _ => self.fm.write(self.address, data),
        }
    }

    /// Generates one sample on all four outputs.
    pub(crate) fn generate(&mut self) -> Output {
        let all = (1 << Engine::<true>::CHANNELS) - 1;
        self.fm.clock(all);
        let mut output: Output = [0; 4];
        self.fm.output(&mut output, 0, 32767, all);
        // The YAC512 DAC takes 16-bit samples.
        for value in output.iter_mut() {
            *value = (*value).clamp(-32768, 32767);
        }
        output
    }

    pub(crate) fn registers(&self) -> &[u8; 0x200] {
        self.fm.regs.data()
    }
}
