// This file is a Rust translation of ymfm's `ymfm_opn.h` and `ymfm_opn.cpp`
// at commit 81aec25 (https://github.com/aaronsgiles/ymfm), with changes: the
// register class template over OPN/OPNA is a const generic; the sine table is
// computed once at compile time and shared rather than held per chip; a
// register write returns its key-on target rather than writing through output
// parameters; key-on logging, the unexpected-access debug log and save states
// are gone; a new chip is reset, where ymfm leaves its register file undefined
// until `reset()`; a chip owns its interface; and the YM3438 and YMF276, which
// ymfm derives from the YM2612 to override `generate()`, wrap it, as the
// YM2610B, derived from the YM2610 with another channel mask, wraps that; the
// SSG resampler takes the SSG engine as an argument rather than holding a
// reference to it, and its resampling function is an enum rather than a
// member function pointer; the ADPCM and SSG engines take the interface as an
// argument, the FM engine owning it; the SSG override is a type parameter of
// the chip, and `with_ssg_override` makes a chip with one in place (as
// `ssg_override` then `set_fidelity` would, then a reset). It covers every
// chip of the file: the YM2203, YM2608, YMF288, YM2610, YM2610B, YM2612,
// YM3438 and YMF276. Every computation that reaches a sample, a status bit
// or a timer is kept as ymfm writes it.
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

//! The OPN family: the YM2203 (OPN, FM and an SSG), the YM2608 (OPNA: FM,
//! an SSG, the ADPCM-A rhythm sounds and ADPCM-B), the YMF288 (OPN3L, the
//! YM2608 without ADPCM-B), the YM2610 (OPNB) and YM2610B (OPNB2), with
//! FM, an SSG, ADPCM-A and ADPCM-B from external memory, and the FM-only
//! YM2612 (OPN2), YM3438 (OPN2C) and YMF276 (OPN2L).
//!
//! The chips with an SSG render at a rate set by their [`OpnFidelity`],
//! repeating the FM samples and resampling the SSG to it; the YM2203 and
//! YM2608 also take a prescale (6, 3 or 2) from writes of the addresses
//! 0x2D to 0x2F. Their SSG can be replaced by an
//! [`SsgOverride`], which then takes its resets,
//! reads and writes and is told of prescale changes, and whose output the
//! chip leaves alone.
//!
//! The register map, as ymfm documents it:
//!
//! ```text
//! System-wide registers:
//!      21 xxxxxxxx Test register
//!      22 ----x--- LFO enable [OPNA+ only]
//!         -----xxx LFO rate [OPNA+ only]
//!      24 xxxxxxxx Timer A value (upper 8 bits)
//!      25 ------xx Timer A value (lower 2 bits)
//!      26 xxxxxxxx Timer B value
//!      27 xx------ CSM/Multi-frequency mode for channel #2
//!         --x----- Reset timer B
//!         ---x---- Reset timer A
//!         ----x--- Enable timer B
//!         -----x-- Enable timer A
//!         ------x- Load timer B
//!         -------x Load timer A
//!      28 x------- Key on/off operator 4
//!         -x------ Key on/off operator 3
//!         --x----- Key on/off operator 2
//!         ---x---- Key on/off operator 1
//!         ------xx Channel select
//!
//! Per-channel registers (channel in address bits 0-1)
//! Note that all these apply to address+100 as well on OPNA+
//!   A0-A3 xxxxxxxx Frequency number lower 8 bits
//!   A4-A7 --xxx--- Block (0-7)
//!         -----xxx Frequency number upper 3 bits
//!   B0-B3 --xxx--- Feedback level for operator 1 (0-7)
//!         -----xxx Operator connection algorithm (0-7)
//!   B4-B7 x------- Pan left [OPNA]
//!         -x------ Pan right [OPNA]
//!         --xx---- LFO AM shift (0-3) [OPNA+ only]
//!         -----xxx LFO PM depth (0-7) [OPNA+ only]
//!
//! Per-operator registers (channel in address bits 0-1, operator in bits 2-3)
//! Note that all these apply to address+100 as well on OPNA+
//!   30-3F -xxx---- Detune value (0-7)
//!         ----xxxx Multiple value (0-15)
//!   40-4F -xxxxxxx Total level (0-127)
//!   50-5F xx------ Key scale rate (0-3)
//!         ---xxxxx Attack rate (0-31)
//!   60-6F x------- LFO AM enable [OPNA]
//!         ---xxxxx Decay rate (0-31)
//!   70-7F ---xxxxx Sustain rate (0-31)
//!   80-8F xxxx---- Sustain level (0-15)
//!         ----xxxx Release rate (0-15)
//!   90-9F ----x--- SSG-EG enable
//!         -----xxx SSG-EG envelope (0-7)
//!
//! Special multi-frequency registers (channel implicitly #2; operator in
//! address bits 0-1)
//!   A8-AB xxxxxxxx Frequency number lower 8 bits
//!   AC-AF --xxx--- Block (0-7)
//!         -----xxx Frequency number upper 3 bits
//!
//! Internal (fake) registers:
//!   B8-BB --xxxxxx Latched frequency number upper bits (from A4-A7)
//!   BC-BF --xxxxxx Latched frequency number upper bits (from AC-AF)
//! ```
//!
//! The YM2612 family adds, on its lower port only:
//!
//! ```text
//!      2A xxxxxxxx DAC data (most significant 8 bits)
//!      2B x------- DAC enable (replaces channel 6)
//!      2C ----x--- Test; the lowest DAC bit
//! ```

use crate::adpcm::{AdpcmAEngine, AdpcmBEngine};
use crate::fm::{
    abs_sin_attenuation, detune_adjustment, effective_rate, operator_list,
    opn_lfo_pm_phase_adjustment, FmEngine, FmRegisters, OpdataCache, MAX_CHANNELS, WAVEFORM_LENGTH,
};
use crate::ssg::{NoSsgOverride, SsgEngine, SsgOverride};
use crate::ymfm::{
    bitfield, clamp, Interface, NullInterface, Output, EG_ATTACK, EG_DECAY, EG_RELEASE, EG_SUSTAIN,
};

//*********************************************************
//  OPN/OPNA REGISTERS
//*********************************************************

/// The OPN's one waveform, the sine; ymfm builds it in the register set's
/// constructor.
static OPN_WAVEFORMS: [[u16; WAVEFORM_LENGTH]; 1] = {
    let mut table = [[0u16; WAVEFORM_LENGTH]; 1];
    let mut index = 0;
    while index < WAVEFORM_LENGTH {
        let i = index as u32;
        table[0][index] = (abs_sin_attenuation(i) | (bitfield(i, 9, 1) << 15)) as u16;
        index += 1;
    }
    table
};

/// The OPN register set (`IS_OPNA` false: three channels, one output, no
/// LFO) or the OPNA one (`IS_OPNA` true: six channels in two banks, stereo,
/// the LFO).
#[derive(Clone)]
pub(crate) struct OpnRegistersBase<const IS_OPNA: bool> {
    /// The LFO counter.
    lfo_counter: u32,
    /// The current LFO AM value.
    lfo_am: u8,
    /// The register data, of which the OPN uses the first 0x100.
    regdata: [u8; 0x200],
}

/// The OPN register set, for the YM2203.
pub(crate) type OpnRegisters = OpnRegistersBase<false>;
/// The OPNA register set, for the YM2608, YMF288, YM2610 and YM2612 family.
pub(crate) type OpnaRegisters = OpnRegistersBase<true>;

impl<const IS_OPNA: bool> OpnRegistersBase<IS_OPNA> {
    /// The number of 8-bit registers.
    const REGISTERS: usize = if IS_OPNA { 0x200 } else { 0x100 };

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

    /// Reads a register value.
    pub(crate) fn read(&self, index: u16) -> u8 {
        self.regdata[usize::from(index)]
    }

    /// Resets the LFO.
    #[allow(dead_code)] // ymfm declares it but never calls it
    pub(crate) fn reset_lfo(&mut self) {
        self.lfo_counter = 0;
    }

    // system-wide registers
    #[allow(dead_code)]
    fn test(&self) -> u32 {
        self.byte(0x21, 0, 8, 0)
    }
    fn lfo_enable(&self) -> u32 {
        if IS_OPNA {
            self.byte(0x22, 3, 1, 0)
        } else {
            0
        }
    }
    fn lfo_rate(&self) -> u32 {
        if IS_OPNA {
            self.byte(0x22, 0, 3, 0)
        } else {
            0
        }
    }
    fn multi_freq(&self) -> u32 {
        u32::from(self.byte(0x27, 6, 2, 0) != 0)
    }
    fn multi_block_freq(&self, num: u32) -> u32 {
        self.word(0xac, 0, 6, 0xa8, 0, 8, num)
    }

    // per-channel registers
    fn ch_block_freq(&self, choffs: u32) -> u32 {
        self.word(0xa4, 0, 6, 0xa0, 0, 8, choffs)
    }
    fn ch_lfo_am_sens(&self, choffs: u32) -> u32 {
        if IS_OPNA {
            self.byte(0xb4, 4, 2, choffs)
        } else {
            0
        }
    }
    fn ch_lfo_pm_sens(&self, choffs: u32) -> u32 {
        if IS_OPNA {
            self.byte(0xb4, 0, 3, choffs)
        } else {
            0
        }
    }

    // per-operator registers
    fn op_detune(&self, opoffs: u32) -> u32 {
        self.byte(0x30, 4, 3, opoffs)
    }
    fn op_multiple(&self, opoffs: u32) -> u32 {
        self.byte(0x30, 0, 4, opoffs)
    }
    fn op_total_level(&self, opoffs: u32) -> u32 {
        self.byte(0x40, 0, 7, opoffs)
    }
    fn op_ksr(&self, opoffs: u32) -> u32 {
        self.byte(0x50, 6, 2, opoffs)
    }
    fn op_attack_rate(&self, opoffs: u32) -> u32 {
        self.byte(0x50, 0, 5, opoffs)
    }
    fn op_decay_rate(&self, opoffs: u32) -> u32 {
        self.byte(0x60, 0, 5, opoffs)
    }
    fn op_sustain_rate(&self, opoffs: u32) -> u32 {
        self.byte(0x70, 0, 5, opoffs)
    }
    fn op_sustain_level(&self, opoffs: u32) -> u32 {
        self.byte(0x80, 4, 4, opoffs)
    }
    fn op_release_rate(&self, opoffs: u32) -> u32 {
        self.byte(0x80, 0, 4, opoffs)
    }
}

impl<const IS_OPNA: bool> FmRegisters for OpnRegistersBase<IS_OPNA> {
    const OUTPUTS: usize = if IS_OPNA { 2 } else { 1 };
    const CHANNELS: usize = if IS_OPNA { 6 } else { 3 };
    const ALL_CHANNELS: u32 = (1 << Self::CHANNELS) - 1;
    const OPERATORS: usize = Self::CHANNELS * 4;
    const DEFAULT_PRESCALE: u32 = 6;
    const EG_CLOCK_DIVIDER: u32 = 3;
    const CSM_TRIGGER_MASK: u32 = 1 << 2;
    const REG_MODE: u32 = 0x27;
    const STATUS_TIMERA: u8 = 0x01;
    const STATUS_TIMERB: u8 = 0x02;
    const STATUS_BUSY: u8 = 0x80;
    const STATUS_IRQ: u8 = 0;
    const EG_HAS_SSG: bool = true;
    const MODULATOR_DELAY: bool = false;

    fn new() -> Self {
        OpnRegistersBase {
            lfo_counter: 0,
            lfo_am: 0,
            regdata: [0; 0x200],
        }
    }

    fn reset(&mut self) {
        self.regdata[..Self::REGISTERS].fill(0);
        if IS_OPNA {
            // enable output on both channels by default
            self.regdata[0xb4..0xb7].fill(0xc0);
            self.regdata[0x1b4..0x1b7].fill(0xc0);
        }
    }

    fn channel_offset(chnum: u32) -> u32 {
        if !IS_OPNA {
            chnum
        } else {
            (chnum % 3) + 0x100 * (chnum / 3)
        }
    }

    fn operator_offset(opnum: u32) -> u32 {
        if !IS_OPNA {
            opnum + opnum / 3
        } else {
            (opnum % 12) + ((opnum % 12) / 3) + 0x100 * (opnum / 12)
        }
    }

    fn operator_map(&self, dest: &mut [u32; MAX_CHANNELS]) {
        // Note that the channel index order is 0,2,1,3, so we bitswap the
        // index.
        //
        // This is because the order in the map is:
        //    carrier 1, carrier 2, modulator 1, modulator 2
        //
        // But when wiring up the connections, the more natural order is:
        //    carrier 1, modulator 1, carrier 2, modulator 2
        const FIXED_MAP: [u32; 6] = [
            operator_list(0, 6, 3, 9),     // Channel 0 operators
            operator_list(1, 7, 4, 10),    // Channel 1 operators
            operator_list(2, 8, 5, 11),    // Channel 2 operators
            operator_list(12, 18, 15, 21), // Channel 3 operators
            operator_list(13, 19, 16, 22), // Channel 4 operators
            operator_list(14, 20, 17, 23), // Channel 5 operators
        ];
        let channels = Self::CHANNELS;
        dest[..channels].copy_from_slice(&FIXED_MAP[..channels]);
    }

    fn write(&mut self, index: u16, data: u8) -> Option<(u32, u32)> {
        debug_assert!(usize::from(index) < Self::REGISTERS);
        let index = u32::from(index);

        // writes in the 0xa0-af/0x1a0-af region are handled as latched pairs
        // borrow unused registers 0xb8-bf as temporary holding locations
        if (index & 0xf0) == 0xa0 {
            if bitfield(index, 0, 2) == 3 {
                return None;
            }

            // one latch per half (0xb8, 0xb9), shared by both banks
            let latchindex = (0xb8 | bitfield(index, 3, 1)) as usize;

            // writes to the upper half just latch (only low 6 bits matter)
            if bitfield(index, 2, 1) != 0 {
                self.regdata[latchindex] = data & 0x3f;
            }
            // writes to the lower half also apply said latch
            else {
                self.regdata[index as usize] = data;
                self.regdata[(index | 4) as usize] = self.regdata[latchindex];
            }
            return None;
        } else if (index & 0xf8) == 0xb8 {
            // registers 0xb8-0xbf are used internally
            return None;
        }

        // everything else is normal
        self.regdata[index as usize] = data;

        // handle writes to the key on index
        if index == 0x28 {
            let data = u32::from(data);
            let mut channel = bitfield(data, 0, 2);
            if channel == 3 {
                return None;
            }
            if IS_OPNA {
                channel += bitfield(data, 2, 1) * 3;
            }
            return Some((channel, bitfield(data, 4, 4)));
        }
        None
    }

    fn clock_noise_and_lfo(&mut self) -> i32 {
        // OPN has no noise generation

        // if LFO not enabled (not present on OPN), quick exit with 0s
        if !IS_OPNA || self.lfo_enable() == 0 {
            self.lfo_counter = 0;

            // special case: if LFO is disabled on OPNA, it basically just
            // keeps the counter at 0; since position 0 gives an AM value of
            // 0x3f, it is important to reflect that here; for example,
            // MegaDrive Venom plays some notes with LFO globally disabled but
            // enabling LFO on the operators, and it expects this added
            // attenutation
            self.lfo_am = if IS_OPNA { 0x3f } else { 0x00 };
            return 0;
        }

        // this table is based on converting the frequencies in the
        // applications manual to clock dividers, based on the assumption of a
        // 7-bit LFO value
        const LFO_MAX_COUNT: [u8; 8] = [109, 78, 72, 68, 63, 45, 9, 6];
        let subcount = u32::from(self.lfo_counter as u8);
        self.lfo_counter = self.lfo_counter.wrapping_add(1);

        // when we cross the divider count, add enough to zero it and cause an
        // increment at bit 8; the 7-bit value lives from bits 8-14
        if subcount >= u32::from(LFO_MAX_COUNT[self.lfo_rate() as usize]) {
            // note: to match the published values this should be 0x100 -
            // subcount; however, tests on the hardware and nuked bear out an
            // off-by-one error exists that causes the max LFO rate to be
            // faster than published
            self.lfo_counter = self.lfo_counter.wrapping_add(0x101 - subcount);
        }

        // AM value is 7 bits, staring at bit 8; grab the low 6 directly
        self.lfo_am = bitfield(self.lfo_counter, 8, 6) as u8;

        // first half of the AM period (bit 6 == 0) is inverted
        if bitfield(self.lfo_counter, 8 + 6, 1) == 0 {
            self.lfo_am ^= 0x3f;
        }

        // PM value is 5 bits, starting at bit 10; grab the low 3 directly
        let mut pm = bitfield(self.lfo_counter, 10, 3) as i32;

        // PM is reflected based on bit 3
        if bitfield(self.lfo_counter, 10 + 3, 1) != 0 {
            pm ^= 7;
        }

        // PM is negated based on bit 4
        if bitfield(self.lfo_counter, 10 + 4, 1) != 0 {
            -pm
        } else {
            pm
        }
    }

    fn lfo_am_offset(&self, choffs: u32) -> u32 {
        // shift value for AM sensitivity is [7, 3, 1, 0], mapping to values
        // of [0, 1.4, 5.9, and 11.8dB]
        let am_shift = (1 << (self.ch_lfo_am_sens(choffs) ^ 3)) - 1;

        // QUESTION (ymfm's): max sensitivity should give 11.8dB range, but
        // this value is directly added to an x.8 attenuation value, which
        // will only give 126/256 or ~4.9dB range -- what am I missing? The
        // calculation below matches several other emulators, including the
        // Nuked implemenation.

        // raw LFO AM value on OPN is 0-3F, scale that up by a factor of 2
        // (giving 7 bits) before applying the final shift
        (u32::from(self.lfo_am) << 1) >> am_shift
    }

    fn noise_state(&self) -> u32 {
        0
    }

    fn cache_operator_data(&mut self, choffs: u32, opoffs: u32, cache: &mut OpdataCache) {
        // set up the easy stuff
        cache.waveform = 0;

        // get frequency from the channel
        let mut block_freq = self.ch_block_freq(choffs);
        cache.block_freq = block_freq;

        // if multi-frequency mode is enabled and this is channel 2, fetch one
        // of the special frequencies
        if self.multi_freq() != 0 && choffs == 2 {
            if opoffs == 2 {
                block_freq = self.multi_block_freq(1);
                cache.block_freq = block_freq;
            } else if opoffs == 10 {
                block_freq = self.multi_block_freq(2);
                cache.block_freq = block_freq;
            } else if opoffs == 6 {
                block_freq = self.multi_block_freq(0);
                cache.block_freq = block_freq;
            }
        }

        // compute the keycode: block_freq is:
        //
        //     BBBFFFFFFFFFFF
        //     ^^^^???
        //
        // the 5-bit keycode uses the top 4 bits plus a magic formula for the
        // final bit
        let mut keycode = bitfield(block_freq, 10, 4) << 1;

        // lowest bit is determined by a mix of next lower FNUM bits according
        // to this equation from the YM2608 manual:
        //
        //   (F11 & (F10 | F9 | F8)) | (!F11 & F10 & F9 & F8)
        //
        // for speed, we just look it up in a 16-bit constant
        keycode |= bitfield(0xfe80, bitfield(block_freq, 7, 4), 1);

        // detune adjustment
        cache.detune = detune_adjustment(self.op_detune(opoffs), keycode);

        // multiple value, as an x.1 value (0 means 0.5)
        cache.multiple = self.op_multiple(opoffs) * 2;
        if cache.multiple == 0 {
            cache.multiple = 1;
        }

        // phase step, or PHASE_STEP_DYNAMIC if PM is active; this depends on
        // block_freq, detune, and multiple, so compute it after we've done
        // those
        cache.phase_step = if !IS_OPNA || self.lfo_enable() == 0 || self.ch_lfo_pm_sens(choffs) == 0
        {
            self.compute_phase_step(choffs, opoffs, cache, 0)
        } else {
            OpdataCache::PHASE_STEP_DYNAMIC
        };

        // total level, scaled by 8
        cache.total_level = self.op_total_level(opoffs) << 3;

        // 4-bit sustain level, but 15 means 31 so effectively 5 bits
        cache.eg_sustain = self.op_sustain_level(opoffs);
        cache.eg_sustain |= (cache.eg_sustain + 1) & 0x10;
        cache.eg_sustain <<= 5;

        // determine KSR adjustment for enevlope rates
        let ksrval = keycode >> (self.op_ksr(opoffs) ^ 3);
        cache.eg_rate[EG_ATTACK] = effective_rate(self.op_attack_rate(opoffs) * 2, ksrval);
        cache.eg_rate[EG_DECAY] = effective_rate(self.op_decay_rate(opoffs) * 2, ksrval);
        cache.eg_rate[EG_SUSTAIN] = effective_rate(self.op_sustain_rate(opoffs) * 2, ksrval);
        cache.eg_rate[EG_RELEASE] = effective_rate(self.op_release_rate(opoffs) * 4 + 2, ksrval);
    }

    fn compute_phase_step(
        &mut self,
        choffs: u32,
        _opoffs: u32,
        cache: &OpdataCache,
        lfo_raw_pm: i32,
    ) -> u32 {
        // OPN phase calculation has only a single detune parameter and uses
        // FNUMs instead of keycodes

        // extract frequency number (low 11 bits of block_freq)
        let mut fnum = bitfield(cache.block_freq, 0, 11) << 1;

        // if there's a non-zero PM sensitivity, compute the adjustment
        let pm_sensitivity = self.ch_lfo_pm_sens(choffs);
        if pm_sensitivity != 0 {
            // apply the phase adjustment based on the upper 7 bits of FNUM
            // and the PM depth parameters; `uint32_t += int32_t` wraps
            fnum = fnum.wrapping_add(opn_lfo_pm_phase_adjustment(
                bitfield(cache.block_freq, 4, 7),
                pm_sensitivity,
                lfo_raw_pm,
            ) as u32);

            // keep fnum to 12 bits
            fnum &= 0xfff;
        }

        // apply block shift to compute phase step
        let block = bitfield(cache.block_freq, 11, 3);
        let mut phase_step = (fnum << block) >> 2;

        // apply detune based on the keycode
        phase_step = phase_step.wrapping_add(cache.detune as u32);

        // clamp to 17 bits in case detune overflows
        // QUESTION (ymfm's): is this specific to the YM2612/3438?
        phase_step &= 0x1ffff;

        // apply frequency multiplier (which is cached as an x.1 value)
        (phase_step * cache.multiple) >> 1
    }

    fn waveform(&self, index: u8) -> &[u16; WAVEFORM_LENGTH] {
        &OPN_WAVEFORMS[index as usize]
    }

    fn timer_a_value(&self) -> u32 {
        self.word(0x24, 0, 8, 0x25, 0, 2, 0)
    }
    fn timer_b_value(&self) -> u32 {
        self.byte(0x26, 0, 8, 0)
    }
    fn csm(&self) -> u32 {
        u32::from(self.byte(0x27, 6, 2, 0) == 2)
    }
    fn reset_timer_b(&self) -> u32 {
        self.byte(0x27, 5, 1, 0)
    }
    fn reset_timer_a(&self) -> u32 {
        self.byte(0x27, 4, 1, 0)
    }
    fn enable_timer_b(&self) -> u32 {
        self.byte(0x27, 3, 1, 0)
    }
    fn enable_timer_a(&self) -> u32 {
        self.byte(0x27, 2, 1, 0)
    }
    fn load_timer_b(&self) -> u32 {
        self.byte(0x27, 1, 1, 0)
    }
    fn load_timer_a(&self) -> u32 {
        self.byte(0x27, 0, 1, 0)
    }

    fn ch_output_any(&self, choffs: u32) -> u32 {
        if IS_OPNA {
            self.byte(0xb4, 6, 2, choffs)
        } else {
            1
        }
    }
    fn ch_output_0(&self, choffs: u32) -> u32 {
        if IS_OPNA {
            self.byte(0xb4, 7, 1, choffs)
        } else {
            1
        }
    }
    fn ch_output_1(&self, choffs: u32) -> u32 {
        if IS_OPNA {
            self.byte(0xb4, 6, 1, choffs)
        } else {
            0
        }
    }
    fn ch_output_2(&self, _choffs: u32) -> u32 {
        0
    }
    fn ch_output_3(&self, _choffs: u32) -> u32 {
        0
    }
    fn ch_feedback(&self, choffs: u32) -> u32 {
        self.byte(0xb0, 3, 3, choffs)
    }
    fn ch_algorithm(&self, choffs: u32) -> u32 {
        self.byte(0xb0, 0, 3, choffs)
    }

    fn op_lfo_am_enable(&self, opoffs: u32) -> u32 {
        if IS_OPNA {
            self.byte(0x60, 7, 1, opoffs)
        } else {
            0
        }
    }
    fn op_ssg_eg_enable(&self, opoffs: u32) -> u32 {
        self.byte(0x90, 3, 1, opoffs)
    }
    fn op_ssg_eg_mode(&self, opoffs: u32) -> u32 {
        self.byte(0x90, 0, 3, opoffs)
    }
}

//*********************************************************
//  OPN IMPLEMENTATION CLASSES
//*********************************************************

/// How finely a chip with an SSG renders, which sets its output rate and so
/// how its FM and SSG samples are repeated or averaged to reach it.
///
/// ymfm's note on prescaling and sample rates: the chips with an SSG run
/// their FM engine at the input clock over the prescale (6, 3 or 2) times
/// 12 operators, and the SSG at a multiple of that; the output rate is the
/// smallest common one at the maximum fidelity, and the FM rate (or less) at
/// the minimum, with the medium in between. The default is the maximum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OpnFidelity {
    /// The highest output rate, where every FM and SSG sample is kept.
    #[default]
    Max,
    /// The lowest output rate, where SSG samples are averaged.
    Min,
    /// An output rate between the two.
    Med,
}

/// How the SSG resampler turns SSG samples into output samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Resample {
    /// One SSG sample to every n output samples.
    N1(u32),
    /// n SSG samples to every output sample.
    OneN(i32),
    /// Nine SSG samples to every two output samples.
    TwoNine,
    /// Three SSG samples to every two output samples.
    TwoThree,
    /// Three SSG samples to every four output samples.
    FourThree,
    /// No SSG samples: only the sample index moves.
    Nop,
}

/// The SSG resampler: clocks the SSG engine at its own rate and writes its
/// three outputs, averaged to the chip's output rate, from output
/// `FIRST_OUTPUT` on, or mixed into that one output when `MIX_TO_1`.
///
/// ymfm's resampler holds a reference to the SSG engine; this one takes the
/// engine as an argument, and its resampling function is an enum rather
/// than a member function pointer.
#[derive(Clone, Debug)]
struct SsgResampler<const FIRST_OUTPUT: usize, const MIX_TO_1: bool> {
    /// The current sample index.
    sampindex: u32,
    /// The configured resampling.
    resampler: Resample,
    /// The last SSG output.
    last: Output<3>,
}

impl<const FIRST_OUTPUT: usize, const MIX_TO_1: bool> SsgResampler<FIRST_OUTPUT, MIX_TO_1> {
    /// Creates a resampler in the state ymfm's constructor leaves it.
    fn new() -> Self {
        SsgResampler {
            sampindex: 0,
            resampler: Resample::Nop,
            last: Output::default(),
        }
    }

    /// Returns the current sample index.
    fn sampindex(&self) -> u32 {
        self.sampindex
    }

    /// Configures the ratio of output samples to SSG samples.
    fn configure(&mut self, outsamples: u8, srcsamples: u8) {
        self.resampler = match (outsamples, srcsamples) {
            (4, 1) => Resample::N1(4),
            (2, 1) => Resample::N1(2),
            (4, 3) => Resample::FourThree,
            (1, 1) => Resample::N1(1),
            (2, 3) => Resample::TwoThree,
            (1, 3) => Resample::OneN(3),
            (2, 9) => Resample::TwoNine,
            (1, 6) => Resample::OneN(6),
            (0, 0) => Resample::Nop,
            // ymfm asserts; every chip configures one of the above
            _ => unreachable!("an SSG resampling ratio ymfm does not have"),
        };
    }

    /// Adds the last computed value to the sums, applying the given scale.
    fn add_last(&self, sum: &mut [i32; 3], scale: i32) {
        sum[0] += self.last.data[0] * scale;
        sum[1] += self.last.data[1] * scale;
        sum[2] += self.last.data[2] * scale;
    }

    /// Clocks a new value and then adds it to the sums, applying the given
    /// scale.
    fn clock_and_add<O: SsgOverride>(
        &mut self,
        ssg: &mut SsgEngine<O>,
        sum: &mut [i32; 3],
        scale: i32,
    ) {
        ssg.clock();
        ssg.output(&mut self.last);
        self.add_last(sum, scale);
    }

    /// Writes the sums to the appropriate outputs, applying the given
    /// divisor to the final result.
    fn write_to_output<const N: usize>(
        &mut self,
        output: &mut Output<N>,
        sum: [i32; 3],
        divisor: i32,
    ) {
        if MIX_TO_1 {
            // mixing to one, apply a 2/3 factor to prevent overflow
            output.data[FIRST_OUTPUT] = (sum[0] + sum[1] + sum[2]) * 2 / (3 * divisor);
        } else {
            // write three outputs in a row
            output.data[FIRST_OUTPUT] = sum[0] / divisor;
            output.data[FIRST_OUTPUT + 1] = sum[1] / divisor;
            output.data[FIRST_OUTPUT + 2] = sum[2] / divisor;
        }

        // track the sample index here
        self.sampindex = self.sampindex.wrapping_add(1);
    }

    /// Resamples the SSG into `output` as configured.
    fn resample<const N: usize, O: SsgOverride>(
        &mut self,
        ssg: &mut SsgEngine<O>,
        output: &mut [Output<N>],
    ) {
        match self.resampler {
            Resample::N1(multiplier) => {
                for out in output.iter_mut() {
                    if self.sampindex.is_multiple_of(multiplier) {
                        ssg.clock();
                        ssg.output(&mut self.last);
                    }
                    let last = self.last.data;
                    self.write_to_output(out, last, 1);
                }
            }
            Resample::OneN(divisor) => {
                for out in output.iter_mut() {
                    let mut sum = [0; 3];
                    for _ in 0..divisor {
                        self.clock_and_add(ssg, &mut sum, 1);
                    }
                    self.write_to_output(out, sum, divisor);
                }
            }
            Resample::TwoNine => {
                for out in output.iter_mut() {
                    let mut sum = [0; 3];
                    if bitfield(self.sampindex, 0, 1) != 0 {
                        self.add_last(&mut sum, 1);
                    }
                    self.clock_and_add(ssg, &mut sum, 2);
                    self.clock_and_add(ssg, &mut sum, 2);
                    self.clock_and_add(ssg, &mut sum, 2);
                    self.clock_and_add(ssg, &mut sum, 2);
                    if bitfield(self.sampindex, 0, 1) == 0 {
                        self.clock_and_add(ssg, &mut sum, 1);
                    }
                    self.write_to_output(out, sum, 9);
                }
            }
            Resample::TwoThree => {
                for out in output.iter_mut() {
                    let mut sum = [0; 3];
                    if bitfield(self.sampindex, 0, 1) == 0 {
                        self.clock_and_add(ssg, &mut sum, 2);
                        self.clock_and_add(ssg, &mut sum, 1);
                    } else {
                        self.add_last(&mut sum, 1);
                        self.clock_and_add(ssg, &mut sum, 2);
                    }
                    self.write_to_output(out, sum, 3);
                }
            }
            Resample::FourThree => {
                for out in output.iter_mut() {
                    let mut sum = [0; 3];
                    let step = bitfield(self.sampindex, 0, 2) as i32;
                    self.add_last(&mut sum, step);
                    if step != 3 {
                        self.clock_and_add(ssg, &mut sum, 3 - step);
                    }
                    self.write_to_output(out, sum, 3);
                }
            }
            Resample::Nop => {
                // nothing to do except increment the sample index
                self.sampindex = self.sampindex.wrapping_add(output.len() as u32);
            }
        }
    }
}

/// Configures the FM repeat count and the SSG resampler of the YM2203 and
/// YM2608 for a prescale at a fidelity; ymfm repeats this table in both
/// chips' `update_prescale`.
///
/// ```text
/// Fidelity:   ---- minimum ----    ---- medium -----    ---- maximum-----
///              rate = clock/24      rate = clock/12      rate = clock/4    (YM2203)
///              rate = clock/48      rate = clock/24      rate = clock/8    (YM2608)
/// Prescale    FM rate  SSG rate    FM rate  SSG rate    FM rate  SSG rate
///     6          3:1     2:3          6:1     4:3         18:1     4:1
///     3        1.5:1     1:3          3:1     2:3          9:1     2:1
///     2          1:1     1:6          2:1     1:3          6:1     1:1
/// ```
///
/// Returns the number of FM samples per output sample, 0 meaning 1.5.
fn configure_prescaled<const F: usize, const M: bool>(
    fidelity: OpnFidelity,
    prescale: u32,
    resampler: &mut SsgResampler<F, M>,
) -> u8 {
    let (fm_samples_per_output, out, src) = match fidelity {
        OpnFidelity::Min => match prescale {
            3 => (0, 1, 3),
            2 => (1, 1, 6),
            _ => (3, 2, 3),
        },
        OpnFidelity::Med => match prescale {
            3 => (3, 2, 3),
            2 => (2, 1, 3),
            _ => (6, 4, 3),
        },
        OpnFidelity::Max => match prescale {
            3 => (9, 2, 1),
            2 => (6, 1, 1),
            _ => (18, 4, 1),
        },
    };
    resampler.configure(out, src);
    fm_samples_per_output
}

//*********************************************************
//  YM2203
//*********************************************************

/// The Yamaha YM2203 (OPN): three 4-operator FM channels with channel 3's
/// special and CSM modes, two timers, and an SSG with two I/O ports, on
/// four outputs: the FM, through a floating-point DAC, then the SSG's three
/// channels.
///
/// The SSG's I/O ports are the interface's `Io` class, port A at address 0
/// and port B at 1. Writing the address 0x2D, 0x2E or 0x2F selects the
/// prescale (6, 3 or 2, 0x2E only from 6), which changes the FM rate, the
/// SSG's rate and, through the [`OpnFidelity`], how they are repeated and
/// averaged to the output rate, `sample_rate(clock)`.
#[derive(Clone)]
pub struct Ym2203<I: Interface = NullInterface, O: SsgOverride = NoSsgOverride> {
    /// The configured fidelity.
    fidelity: OpnFidelity,
    /// The address register.
    address: u8,
    /// How many output samples each FM sample is repeated for; 0 is 1.5.
    fm_samples_per_output: u8,
    /// The last FM output.
    last_fm: Output<1>,
    /// The core FM engine.
    fm: FmEngine<OpnRegisters, I>,
    /// The SSG engine.
    ssg: SsgEngine<O>,
    /// The SSG resampler.
    ssg_resampler: SsgResampler<1, false>,
}

impl<I: Interface> Ym2203<I> {
    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        Self::build(intf, None)
    }
}

impl<I: Interface, O: SsgOverride> Ym2203<I, O> {
    /// The number of FM outputs.
    pub const FM_OUTPUTS: usize = 1;
    /// The number of SSG outputs.
    pub const SSG_OUTPUTS: usize = SsgEngine::<O>::OUTPUTS;
    /// The number of outputs: the FM, then SSG channels A, B and C.
    pub const OUTPUTS: usize = Self::FM_OUTPUTS + Self::SSG_OUTPUTS;

    const ALL_CHANNELS: u32 = FmEngine::<OpnRegisters, I>::ALL_CHANNELS;

    /// Creates a chip with `ssg_override` in place of its SSG: as ymfm's
    /// `ssg_override` followed by `set_fidelity` at the default, which
    /// configures the resampler to leave the SSG outputs alone (and tells
    /// the override of the prescale), then a reset (which resets the
    /// override).
    pub fn with_ssg_override(intf: I, ssg_override: O) -> Self {
        Self::build(intf, Some(ssg_override))
    }

    fn build(intf: I, ssg_override: Option<O>) -> Self {
        let mut chip = Ym2203 {
            fidelity: OpnFidelity::Max,
            address: 0,
            fm_samples_per_output: 0,
            last_fm: Output::default(),
            fm: FmEngine::new(intf),
            ssg: SsgEngine::new(),
            ssg_resampler: SsgResampler::new(),
        };
        chip.update_prescale(chip.fm.clock_prescale());
        if let Some(ssg_override) = ssg_override {
            chip.ssg_override(ssg_override);
            chip.set_fidelity(chip.fidelity);
        }
        chip.reset();
        chip
    }

    /// Installs an override, which from now on takes the SSG's resets,
    /// reads and writes. As in ymfm, the resampler keeps its configuration
    /// until the prescale or the fidelity next changes.
    pub fn ssg_override(&mut self, ssg_override: O) {
        self.ssg.set_override(ssg_override);
    }

    /// Sets the fidelity, which sets the output rate.
    pub fn set_fidelity(&mut self, fidelity: OpnFidelity) {
        self.fidelity = fidelity;
        self.update_prescale(self.fm.clock_prescale());
    }

    /// Returns the configured fidelity.
    pub fn fidelity(&self) -> OpnFidelity {
        self.fidelity
    }

    /// Resets the chip.
    pub fn reset(&mut self) {
        // reset the engines
        self.fm.reset();
        self.ssg.reset();
    }

    /// Returns the output sample rate for an input clock.
    pub fn sample_rate(&self, input_clock: u32) -> u32 {
        match self.fidelity {
            OpnFidelity::Min => input_clock / 24,
            OpnFidelity::Med => input_clock / 12,
            OpnFidelity::Max => input_clock / 4,
        }
    }

    /// Returns the clock the SSG runs at for an input clock, as a stand-alone
    /// SSG would be clocked.
    pub fn ssg_effective_clock(&self, input_clock: u32) -> u32 {
        let scale = self.fm.clock_prescale() * 2 / 3;
        input_clock.wrapping_mul(2) / scale
    }

    /// Invalidates the cached operator data.
    pub fn invalidate_caches(&mut self) {
        self.fm.invalidate_caches();
    }

    /// Reads the status register.
    pub fn read_status(&mut self) -> u8 {
        let mut result = self.fm.status();
        if self.fm.intf_mut().is_busy() {
            result |= FmEngine::<OpnRegisters, I>::STATUS_BUSY;
        }
        result
    }

    /// Reads the data register: an SSG register, or 0.
    pub fn read_data(&mut self) -> u8 {
        let mut result = 0;
        if self.address < 0x10 {
            // 00-0F: Read from SSG
            result = self
                .ssg
                .read(self.fm.intf_mut(), u32::from(self.address & 0x0f));
        }
        result
    }

    /// Reads from the chip: the status at even offsets, the data at odd
    /// ones.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 1 {
            0 => self.read_status(), // status port
            _ => self.read_data(),   // data port (only SSG)
        }
    }

    /// Writes the address register; 0x2D to 0x2F select the prescale.
    pub fn write_address(&mut self, data: u8) {
        // just set the address
        self.address = data;

        // special case: update the prescale
        if (0x2d..=0x2f).contains(&self.address) {
            // 2D-2F: prescaler select
            if self.address == 0x2d {
                self.update_prescale(6);
            } else if self.address == 0x2e && self.fm.clock_prescale() == 6 {
                self.update_prescale(3);
            } else if self.address == 0x2f {
                self.update_prescale(2);
            }
        }
    }

    /// Writes the data register.
    pub fn write_data(&mut self, data: u8) {
        if self.address < 0x10 {
            // 00-0F: write to SSG
            self.ssg
                .write(self.fm.intf_mut(), u32::from(self.address & 0x0f), data);
        } else {
            // 10-FF: write to FM
            self.fm.write(u16::from(self.address), data);
        }

        // mark busy for a bit
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);
    }

    /// Writes to the chip: the address port at even offsets, the data port
    /// at odd ones.
    pub fn write(&mut self, offset: u32, data: u8) {
        match offset & 1 {
            0 => self.write_address(data), // address port
            _ => self.write_data(data),    // data port
        }
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<4>]) {
        let base = self.ssg_resampler.sampindex();

        // FM output is just repeated the prescale number of times; note that
        // 0 is a special 1.5 case
        if self.fm_samples_per_output != 0 {
            let repeat = u32::from(self.fm_samples_per_output);
            for (samp, out) in output.iter_mut().enumerate() {
                if base.wrapping_add(samp as u32).is_multiple_of(repeat) {
                    self.clock_fm();
                }
                out.data[0] = self.last_fm.data[0];
            }
        } else {
            for (samp, out) in output.iter_mut().enumerate() {
                let step = base.wrapping_add(samp as u32) % 3;
                if step == 0 {
                    self.clock_fm();
                }
                out.data[0] = self.last_fm.data[0];
                if step == 1 {
                    self.clock_fm();
                    out.data[0] = (out.data[0] + self.last_fm.data[0]) / 2;
                }
            }
        }

        // resample the SSG as configured
        self.ssg_resampler.resample(&mut self.ssg, output);
    }

    /// Updates the prescale value, recomputing derived values.
    fn update_prescale(&mut self, prescale: u32) {
        // tell the FM engine
        self.fm.set_clock_prescale(prescale);
        self.ssg.prescale_changed();

        // compute the number of FM samples per output sample, and select the
        // resampler function
        self.fm_samples_per_output =
            configure_prescaled(self.fidelity, prescale, &mut self.ssg_resampler);

        // if overriding the SSG, override the configuration with the nop
        // resampler to at least keep the sample index moving forward
        if self.ssg.overridden() {
            self.ssg_resampler.configure(0, 0);
        }
    }

    /// Clocks the FM state.
    fn clock_fm(&mut self) {
        // clock the system
        self.fm.clock(Self::ALL_CHANNELS);

        // update the FM content; OPN is full 14-bit with no intermediate
        // clipping
        self.fm
            .output(&mut self.last_fm.clear().data, 0, 32767, Self::ALL_CHANNELS);

        // convert to 10.3 floating point value for the DAC and back
        self.last_fm.roundtrip_fp();
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

    /// Returns the SSG engine, and through it its override.
    pub fn ssg(&self) -> &SsgEngine<O> {
        &self.ssg
    }

    /// Returns the SSG engine mutably, and through it its override.
    pub fn ssg_mut(&mut self) -> &mut SsgEngine<O> {
        &mut self.ssg
    }

    /// Returns the FM register file as last written, for a diagnostic,
    /// 0x00 to 0xFF; the SSG's registers are [`ssg`](Self::ssg)'s.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}

//*********************************************************
//  OPNA IMPLEMENTATION CLASSES
//*********************************************************

/// The ADPCM-A start and end addresses of the YM2608's and YMF288's
/// embedded rhythm ROM, per channel: bass drum, snare drum, top cymbal,
/// high hat, tom tom, rim shot.
const OPNA_RHYTHM: [(u16, u16); 6] = [
    (0x0000, 0x01bf), // bass drum
    (0x01c0, 0x043f), // snare drum
    (0x0440, 0x1b7f), // top cymbal
    (0x1b80, 0x1cff), // high hat
    (0x1d00, 0x1f7f), // tom tom
    (0x1f80, 0x1fff), // rim shot
];

//*********************************************************
//  YM2608
//*********************************************************

/// The Yamaha YM2608 (OPNA): six 4-operator FM channels with the LFO, SSG
/// envelopes and channel 3's modes, two timers, an SSG with two I/O ports,
/// six ADPCM-A rhythm channels and an ADPCM-B channel, on three outputs:
/// FM and ADPCM left and right, then the SSG's three channels mixed.
///
/// The rhythm sounds are in a ROM inside the chip; ymfm reads them, as
/// ADPCM-A, through the interface's `AdpcmA` class, 8 KiB from address 0,
/// so the host supplies that ROM. ADPCM-B memory is the `AdpcmB` class, and
/// the SSG's I/O ports the `Io` class. Writing the address 0x2D, 0x2E or
/// 0x2F selects the prescale, as on the YM2203; register 0x29 is the IRQ
/// enable (bit 7 enables channels 4 to 6), 0x110 the flag control.
#[derive(Clone)]
pub struct Ym2608<I: Interface = NullInterface, O: SsgOverride = NoSsgOverride> {
    /// The configured fidelity.
    fidelity: OpnFidelity,
    /// The address register.
    address: u16,
    /// How many output samples each FM sample is repeated for; 0 is 1.5.
    fm_samples_per_output: u8,
    /// The IRQ enable register.
    irq_enable: u8,
    /// The flag control register.
    flag_control: u8,
    /// The last FM output.
    last_fm: Output<2>,
    /// The core FM engine.
    fm: FmEngine<OpnaRegisters, I>,
    /// The SSG engine.
    ssg: SsgEngine<O>,
    /// The SSG resampler.
    ssg_resampler: SsgResampler<2, true>,
    /// The ADPCM-A engine.
    adpcm_a: AdpcmAEngine,
    /// The ADPCM-B engine.
    adpcm_b: AdpcmBEngine,
}

impl<I: Interface> Ym2608<I> {
    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        Self::build(intf, None)
    }
}

impl<I: Interface, O: SsgOverride> Ym2608<I, O> {
    const STATUS_ADPCM_B_EOS: u8 = 0x04;
    const STATUS_ADPCM_B_BRDY: u8 = 0x08;
    #[allow(dead_code)] // ymfm declares it; nothing sets it
    const STATUS_ADPCM_B_ZERO: u8 = 0x10;
    const STATUS_ADPCM_B_PLAYING: u8 = 0x20;

    /// The number of FM outputs.
    pub const FM_OUTPUTS: usize = 2;
    /// The number of SSG outputs: the three channels mixed into one.
    pub const SSG_OUTPUTS: usize = 1;
    /// The number of outputs: left, right, then the SSG.
    pub const OUTPUTS: usize = Self::FM_OUTPUTS + Self::SSG_OUTPUTS;

    const ALL_CHANNELS: u32 = FmEngine::<OpnaRegisters, I>::ALL_CHANNELS;

    /// Creates a chip with `ssg_override` in place of its SSG, as
    /// [`Ym2203::with_ssg_override`] does.
    pub fn with_ssg_override(intf: I, ssg_override: O) -> Self {
        Self::build(intf, Some(ssg_override))
    }

    fn build(intf: I, ssg_override: Option<O>) -> Self {
        let mut chip = Ym2608 {
            fidelity: OpnFidelity::Max,
            address: 0,
            fm_samples_per_output: 0,
            irq_enable: 0x1f,
            flag_control: 0x1c,
            last_fm: Output::default(),
            fm: FmEngine::new(intf),
            ssg: SsgEngine::new(),
            ssg_resampler: SsgResampler::new(),
            adpcm_a: AdpcmAEngine::new(0),
            adpcm_b: AdpcmBEngine::new(0),
        };
        chip.update_prescale(chip.fm.clock_prescale());
        if let Some(ssg_override) = ssg_override {
            chip.ssg_override(ssg_override);
            chip.set_fidelity(chip.fidelity);
        }
        chip.reset();
        chip
    }

    /// Installs an override, which from now on takes the SSG's resets,
    /// reads and writes. As in ymfm, the resampler keeps its configuration
    /// until the prescale or the fidelity next changes.
    pub fn ssg_override(&mut self, ssg_override: O) {
        self.ssg.set_override(ssg_override);
    }

    /// Sets the fidelity, which sets the output rate.
    pub fn set_fidelity(&mut self, fidelity: OpnFidelity) {
        self.fidelity = fidelity;
        self.update_prescale(self.fm.clock_prescale());
    }

    /// Returns the configured fidelity.
    pub fn fidelity(&self) -> OpnFidelity {
        self.fidelity
    }

    /// Resets the chip.
    pub fn reset(&mut self) {
        // reset the engines
        self.fm.reset();
        self.ssg.reset();
        self.adpcm_a.reset();
        self.adpcm_b.reset();

        // configure ADPCM percussion sounds; these are present in an embedded
        // ROM
        for (chnum, (start, end)) in OPNA_RHYTHM.iter().enumerate() {
            self.adpcm_a.set_start_end(chnum as u8, *start, *end);
        }

        // initialize our special interrupt states, then read the upper
        // status register, which updates the IRQs
        self.irq_enable = 0x1f;
        self.flag_control = 0x1c;
        self.read_status_hi();
    }

    /// Returns the output sample rate for an input clock.
    pub fn sample_rate(&self, input_clock: u32) -> u32 {
        match self.fidelity {
            OpnFidelity::Min => input_clock / 48,
            OpnFidelity::Med => input_clock / 24,
            OpnFidelity::Max => input_clock / 8,
        }
    }

    /// Returns the clock the SSG runs at for an input clock, as a stand-alone
    /// SSG would be clocked.
    pub fn ssg_effective_clock(&self, input_clock: u32) -> u32 {
        let scale = self.fm.clock_prescale() * 2 / 3;
        input_clock / scale
    }

    /// Invalidates the cached operator data.
    pub fn invalidate_caches(&mut self) {
        self.fm.invalidate_caches();
    }

    /// Reads the status register: the timers and busy.
    pub fn read_status(&mut self) -> u8 {
        let mut result = self.fm.status()
            & (FmEngine::<OpnaRegisters, I>::STATUS_TIMERA
                | FmEngine::<OpnaRegisters, I>::STATUS_TIMERB);
        if self.fm.intf_mut().is_busy() {
            result |= FmEngine::<OpnaRegisters, I>::STATUS_BUSY;
        }
        result
    }

    /// Reads the data register: an SSG register, the ID code (1) at 0xFF,
    /// or 0.
    pub fn read_data(&mut self) -> u8 {
        let mut result = 0;
        if self.address < 0x10 {
            // 00-0F: Read from SSG
            result = self
                .ssg
                .read(self.fm.intf_mut(), u32::from(self.address & 0x0f));
        } else if self.address == 0xff {
            // FF: ID code
            result = 1;
        }
        result
    }

    /// Reads the extended status register: the timers, ADPCM-B's end of
    /// sample, buffer ready and playing, less the masked flags, and busy;
    /// the read updates the IRQ.
    pub fn read_status_hi(&mut self) -> u8 {
        // fetch regular status
        let mut status = self.fm.status()
            & !(Self::STATUS_ADPCM_B_EOS
                | Self::STATUS_ADPCM_B_BRDY
                | Self::STATUS_ADPCM_B_PLAYING);

        // fetch ADPCM-B status, and merge in the bits
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

        // turn off any bits that have been requested to be masked
        status &= !(self.flag_control & 0x1f);

        // update the status so that IRQs are propagated
        self.fm.set_reset_status(status, !status);

        // merge in the busy flag
        if self.fm.intf_mut().is_busy() {
            status |= FmEngine::<OpnaRegisters, I>::STATUS_BUSY;
        }
        status
    }

    /// Reads the upper data register: an ADPCM-B register, or 0.
    pub fn read_data_hi(&mut self) -> u8 {
        let mut result = 0;
        if (self.address & 0xff) < 0x10 {
            // 00-0F: Read from ADPCM-B
            result =
                self.adpcm_b
                    .read(u32::from(self.address & 0x0f), self.fm.intf_mut()) as u8;
        }
        result
    }

    /// Reads from the chip: the status at offset 0, the data at 1, the
    /// extended status at 2 and the ADPCM-B data at 3.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 3 {
            0 => self.read_status(),    // status port, YM2203 compatible
            1 => self.read_data(),      // data port (only SSG)
            2 => self.read_status_hi(), // status port, extended
            _ => self.read_data_hi(),   // ADPCM-B data
        }
    }

    /// Writes the address register; 0x2D to 0x2F select the prescale.
    pub fn write_address(&mut self, data: u8) {
        // just set the address
        self.address = u16::from(data);

        // special case: update the prescale
        if (0x2d..=0x2f).contains(&self.address) {
            // 2D-2F: prescaler select
            if self.address == 0x2d {
                self.update_prescale(6);
            } else if self.address == 0x2e && self.fm.clock_prescale() == 6 {
                self.update_prescale(3);
            } else if self.address == 0x2f {
                self.update_prescale(2);
            }
        }
    }

    /// Writes the data register.
    pub fn write_data(&mut self, data: u8) {
        // ignore if paired with upper address
        if bitfield(u32::from(self.address), 8, 1) != 0 {
            return;
        }

        if self.address < 0x10 {
            // 00-0F: write to SSG
            self.ssg
                .write(self.fm.intf_mut(), u32::from(self.address & 0x0f), data);
        } else if self.address < 0x20 {
            // 10-1F: write to ADPCM-A
            self.adpcm_a.write(u32::from(self.address & 0x0f), data);
        } else if self.address == 0x29 {
            // 29: special IRQ mask register
            self.irq_enable = data;
            self.fm
                .set_irq_mask(self.irq_enable & !self.flag_control & 0x1f);
        } else {
            // 20-28, 2A-FF: write to FM
            self.fm.write(self.address, data);
        }

        // mark busy for a bit
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);
    }

    /// Writes the upper address register.
    pub fn write_address_hi(&mut self, data: u8) {
        // just set the address
        self.address = 0x100 | u16::from(data);
    }

    /// Writes the upper data register.
    pub fn write_data_hi(&mut self, data: u8) {
        // ignore if paired with upper address
        if bitfield(u32::from(self.address), 8, 1) == 0 {
            return;
        }

        if self.address < 0x110 {
            // 100-10F: write to ADPCM-B
            self.adpcm_b
                .write(u32::from(self.address & 0x0f), data, self.fm.intf_mut());
        } else if self.address == 0x110 {
            // 110: IRQ flag control
            if bitfield(u32::from(data), 7, 1) != 0 {
                self.fm.set_reset_status(0, 0xff);
            } else {
                self.flag_control = data;
                self.fm
                    .set_irq_mask(self.irq_enable & !self.flag_control & 0x1f);
            }
        } else {
            // 111-1FF: write to FM
            self.fm.write(self.address, data);
        }

        // mark busy for a bit
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);
    }

    /// Writes to the chip: the address port at offset 0, the data port at 1,
    /// the upper address port at 2 and the upper data port at 3.
    pub fn write(&mut self, offset: u32, data: u8) {
        match offset & 3 {
            0 => self.write_address(data),    // address port
            1 => self.write_data(data),       // data port
            2 => self.write_address_hi(data), // upper address port
            _ => self.write_data_hi(data),    // upper data port
        }
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<3>]) {
        let base = self.ssg_resampler.sampindex();

        // FM output is just repeated the prescale number of times; note that
        // 0 is a special 1.5 case
        if self.fm_samples_per_output != 0 {
            let repeat = u32::from(self.fm_samples_per_output);
            for (samp, out) in output.iter_mut().enumerate() {
                if base.wrapping_add(samp as u32).is_multiple_of(repeat) {
                    self.clock_fm_and_adpcm();
                }
                out.data[0] = self.last_fm.data[0];
                out.data[1] = self.last_fm.data[1];
            }
        } else {
            for (samp, out) in output.iter_mut().enumerate() {
                let step = base.wrapping_add(samp as u32) % 3;
                if step == 0 {
                    self.clock_fm_and_adpcm();
                }
                out.data[0] = self.last_fm.data[0];
                out.data[1] = self.last_fm.data[1];
                if step == 1 {
                    self.clock_fm_and_adpcm();
                    out.data[0] = (out.data[0] + self.last_fm.data[0]) / 2;
                    out.data[1] = (out.data[1] + self.last_fm.data[1]) / 2;
                }
            }
        }

        // resample the SSG as configured
        self.ssg_resampler.resample(&mut self.ssg, output);
    }

    /// Updates the prescale value, recomputing derived values.
    fn update_prescale(&mut self, prescale: u32) {
        // tell the FM engine
        self.fm.set_clock_prescale(prescale);
        self.ssg.prescale_changed();

        // compute the number of FM samples per output sample, and select the
        // resampler function
        self.fm_samples_per_output =
            configure_prescaled(self.fidelity, prescale, &mut self.ssg_resampler);

        // if overriding the SSG, override the configuration with the nop
        // resampler to at least keep the sample index moving forward
        if self.ssg.overridden() {
            self.ssg_resampler.configure(0, 0);
        }
    }

    /// Clocks the FM and ADPCM state.
    fn clock_fm_and_adpcm(&mut self) {
        // top bit of the IRQ enable flags controls 3-channel vs 6-channel
        // mode
        let fmmask = if bitfield(u32::from(self.irq_enable), 7, 1) != 0 {
            0x3f
        } else {
            0x07
        };

        // clock the system
        let env_counter = self.fm.clock(Self::ALL_CHANNELS);

        // clock the ADPCM-A engine on every envelope cycle (channels 4 and 5
        // clock every 2 envelope clocks)
        if bitfield(env_counter, 0, 2) == 0 {
            let mask = if bitfield(env_counter, 2, 1) != 0 {
                0x0f
            } else {
                0x3f
            };
            self.adpcm_a.clock(mask, self.fm.intf_mut());
        }

        // clock the ADPCM-B engine every cycle
        self.adpcm_b.clock(self.fm.intf_mut());

        // update the FM content; OPNA is 13-bit with no intermediate
        // clipping
        self.fm
            .output(&mut self.last_fm.clear().data, 1, 32767, fmmask);

        // mix in the ADPCM and clamp
        self.adpcm_a.output(&mut self.last_fm, 0x3f);
        self.adpcm_b.output(&mut self.last_fm, 1);
        self.last_fm.clamp16();
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

    /// Returns the SSG engine, and through it its override.
    pub fn ssg(&self) -> &SsgEngine<O> {
        &self.ssg
    }

    /// Returns the SSG engine mutably, and through it its override.
    pub fn ssg_mut(&mut self) -> &mut SsgEngine<O> {
        &mut self.ssg
    }

    /// Returns the FM register file as last written, for a diagnostic: both
    /// banks, 0x000 to 0x1FF, with the frequency latches at 0xB8 and 0xB9.
    /// The SSG, ADPCM and flag registers are not kept there.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}

//*********************************************************
//  YMF288
//*********************************************************

// YMF288 is a YM2608 with the following changes:
//   * ADPCM-B part removed
//   * prescaler removed (fixed at 6)
//   * CSM removed
//   * Low power mode added
//   * SSG tone frequency is altered in some way? (explicitly DC for Tp 0-7,
//     also double volume in some cases)
//   * I/O ports removed
//   * Shorter busy times
//   * All registers can be read

/// The Yamaha YMF288 (OPN3L): a YM2608 without ADPCM-B, the prescaler (it
/// is fixed at 6), CSM or the SSG's I/O ports, with shorter busy times and
/// readable registers in YMF288 mode (bit 1 of register 0x20), on three
/// outputs: FM and ADPCM-A left and right, then the SSG mixed.
///
/// As on the YM2608, the rhythm ROM is read through the interface's
/// `AdpcmA` class.
#[derive(Clone)]
pub struct Ymf288<I: Interface = NullInterface, O: SsgOverride = NoSsgOverride> {
    /// The configured fidelity.
    fidelity: OpnFidelity,
    /// The address register.
    address: u16,
    /// How many output samples each FM sample is repeated for.
    fm_samples_per_output: u8,
    /// The IRQ enable register.
    irq_enable: u8,
    /// The flag control register.
    flag_control: u8,
    /// The last FM output.
    last_fm: Output<2>,
    /// The core FM engine.
    fm: FmEngine<OpnaRegisters, I>,
    /// The SSG engine.
    ssg: SsgEngine<O>,
    /// The SSG resampler.
    ssg_resampler: SsgResampler<2, true>,
    /// The ADPCM-A engine.
    adpcm_a: AdpcmAEngine,
}

impl<I: Interface> Ymf288<I> {
    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        Self::build(intf, None)
    }
}

impl<I: Interface, O: SsgOverride> Ymf288<I, O> {
    /// The number of FM outputs.
    pub const FM_OUTPUTS: usize = 2;
    /// The number of SSG outputs: the three channels mixed into one.
    pub const SSG_OUTPUTS: usize = 1;
    /// The number of outputs: left, right, then the SSG.
    pub const OUTPUTS: usize = Self::FM_OUTPUTS + Self::SSG_OUTPUTS;

    const ALL_CHANNELS: u32 = FmEngine::<OpnaRegisters, I>::ALL_CHANNELS;

    /// Creates a chip with `ssg_override` in place of its SSG: as ymfm's
    /// `ssg_override` followed by `set_fidelity` at the default, which
    /// configures the resampler to leave the SSG output alone, then a reset
    /// (which resets the override).
    pub fn with_ssg_override(intf: I, ssg_override: O) -> Self {
        Self::build(intf, Some(ssg_override))
    }

    fn build(intf: I, ssg_override: Option<O>) -> Self {
        let mut chip = Ymf288 {
            fidelity: OpnFidelity::Max,
            address: 0,
            fm_samples_per_output: 0,
            irq_enable: 0x03,
            flag_control: 0x03,
            last_fm: Output::default(),
            fm: FmEngine::new(intf),
            ssg: SsgEngine::new(),
            ssg_resampler: SsgResampler::new(),
            adpcm_a: AdpcmAEngine::new(0),
        };
        chip.update_prescale();
        if let Some(ssg_override) = ssg_override {
            chip.ssg_override(ssg_override);
            chip.set_fidelity(chip.fidelity);
        }
        chip.reset();
        chip
    }

    /// Installs an override, which from now on takes the SSG's resets,
    /// reads and writes. As in ymfm, the resampler keeps its configuration
    /// until the fidelity next changes.
    pub fn ssg_override(&mut self, ssg_override: O) {
        self.ssg.set_override(ssg_override);
    }

    /// Sets the fidelity, which sets the output rate.
    pub fn set_fidelity(&mut self, fidelity: OpnFidelity) {
        self.fidelity = fidelity;
        self.update_prescale();
    }

    /// Returns the configured fidelity.
    pub fn fidelity(&self) -> OpnFidelity {
        self.fidelity
    }

    /// Resets the chip.
    pub fn reset(&mut self) {
        // reset the engines
        self.fm.reset();
        self.ssg.reset();
        self.adpcm_a.reset();

        // configure ADPCM percussion sounds; these are present in an embedded
        // ROM
        for (chnum, (start, end)) in OPNA_RHYTHM.iter().enumerate() {
            self.adpcm_a.set_start_end(chnum as u8, *start, *end);
        }

        // initialize our special interrupt states, then read the upper
        // status register, which updates the IRQs
        self.irq_enable = 0x03;
        self.flag_control = 0x00;
        self.read_status_hi();
    }

    /// Returns the output sample rate for an input clock.
    pub fn sample_rate(&self, input_clock: u32) -> u32 {
        match self.fidelity {
            OpnFidelity::Min => input_clock / 144,
            OpnFidelity::Med => input_clock / 144,
            OpnFidelity::Max => input_clock / 16,
        }
    }

    /// Returns the clock the SSG runs at for an input clock, as a stand-alone
    /// SSG would be clocked.
    pub fn ssg_effective_clock(&self, input_clock: u32) -> u32 {
        input_clock / 4
    }

    /// Invalidates the cached operator data.
    pub fn invalidate_caches(&mut self) {
        self.fm.invalidate_caches();
    }

    /// Whether the chip is in YMF288 mode, bit 1 of register 0x20.
    fn ymf288_mode(&self) -> bool {
        (self.fm.regs().read(0x20) & 0x02) != 0
    }

    /// Reads the status register: the timers and busy.
    pub fn read_status(&mut self) -> u8 {
        let mut result = self.fm.status()
            & (FmEngine::<OpnaRegisters, I>::STATUS_TIMERA
                | FmEngine::<OpnaRegisters, I>::STATUS_TIMERB);
        if self.fm.intf_mut().is_busy() {
            result |= FmEngine::<OpnaRegisters, I>::STATUS_BUSY;
        }
        result
    }

    /// Reads the data register: an SSG register, 0xFF for the missing I/O
    /// ports, the ID code (2) at 0xFF, or in YMF288 mode any register.
    pub fn read_data(&mut self) -> u8 {
        let mut result = 0;
        if self.address < 0x0e {
            // 00-0D: Read from SSG
            result = self
                .ssg
                .read(self.fm.intf_mut(), u32::from(self.address & 0x0f));
        } else if self.address < 0x10 {
            // 0E-0F: I/O ports not supported
            result = 0xff;
        } else if self.address == 0xff {
            // FF: ID code
            result = 2;
        } else if self.ymf288_mode() {
            // registers are readable in YMF288 mode
            result = self.fm.regs().read(self.address);
        }
        result
    }

    /// Reads the extended status register: the timers, less the masked
    /// flags, and busy; the read updates the IRQ.
    pub fn read_status_hi(&mut self) -> u8 {
        // fetch regular status
        let mut status = self.fm.status()
            & (FmEngine::<OpnaRegisters, I>::STATUS_TIMERA
                | FmEngine::<OpnaRegisters, I>::STATUS_TIMERB);

        // turn off any bits that have been requested to be masked
        status &= !(self.flag_control & 0x03);

        // update the status so that IRQs are propagated
        self.fm.set_reset_status(status, !status);

        // merge in the busy flag
        if self.fm.intf_mut().is_busy() {
            status |= FmEngine::<OpnaRegisters, I>::STATUS_BUSY;
        }
        status
    }

    /// Reads from the chip: the status at offset 0, the data at 1 and the
    /// extended status at 2; offset 3 is unmapped and reads 0.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 3 {
            0 => self.read_status(),    // status port, YM2203 compatible
            1 => self.read_data(),      // data port
            2 => self.read_status_hi(), // status port, extended
            _ => 0,                     // unmapped
        }
    }

    /// Writes the address register; in YMF288 mode that makes the chip busy
    /// too.
    pub fn write_address(&mut self, data: u8) {
        // just set the address
        self.address = u16::from(data);

        // in YMF288 mode, busy is signaled after address writes too
        if self.ymf288_mode() {
            self.fm.intf_mut().set_busy_end(16);
        }
    }

    /// Writes the data register.
    pub fn write_data(&mut self, mut data: u8) {
        // ignore if paired with upper address
        if bitfield(u32::from(self.address), 8, 1) != 0 {
            return;
        }

        // wait times are shorter in YMF288 mode
        let mut busy_cycles = if self.ymf288_mode() {
            16
        } else {
            32 * self.fm.clock_prescale()
        };
        if self.address < 0x0e {
            // 00-0D: write to SSG
            self.ssg
                .write(self.fm.intf_mut(), u32::from(self.address & 0x0f), data);
        } else if self.address < 0x10 {
            // 0E-0F: I/O ports not supported
        } else if self.address < 0x20 {
            // 10-1F: write to ADPCM-A
            self.adpcm_a.write(u32::from(self.address & 0x0f), data);
            busy_cycles = 32 * self.fm.clock_prescale();
        } else if self.address == 0x27 {
            // 27: mode register; CSM isn't supported so disable it
            data &= 0x7f;
            self.fm.write(self.address, data);
        } else if self.address == 0x29 {
            // 29: special IRQ mask register
            self.irq_enable = data;
            self.fm
                .set_irq_mask(self.irq_enable & !self.flag_control & 0x03);
        } else {
            // 20-27, 2A-FF: write to FM
            self.fm.write(self.address, data);
        }

        // mark busy for a bit
        self.fm.intf_mut().set_busy_end(busy_cycles);
    }

    /// Writes the upper address register; in YMF288 mode that makes the
    /// chip busy too.
    pub fn write_address_hi(&mut self, data: u8) {
        // just set the address
        self.address = 0x100 | u16::from(data);

        // in YMF288 mode, busy is signaled after address writes too
        if self.ymf288_mode() {
            self.fm.intf_mut().set_busy_end(16);
        }
    }

    /// Writes the upper data register.
    pub fn write_data_hi(&mut self, data: u8) {
        // ignore if paired with upper address
        if bitfield(u32::from(self.address), 8, 1) == 0 {
            return;
        }

        // wait times are shorter in YMF288 mode
        let busy_cycles = if self.ymf288_mode() {
            16
        } else {
            32 * self.fm.clock_prescale()
        };
        if self.address == 0x110 {
            // 110: IRQ flag control
            if bitfield(u32::from(data), 7, 1) != 0 {
                self.fm.set_reset_status(0, 0xff);
            } else {
                self.flag_control = data;
                self.fm
                    .set_irq_mask(self.irq_enable & !self.flag_control & 0x03);
            }
        } else {
            // 100-10F,111-1FF: write to FM
            self.fm.write(self.address, data);
        }

        // mark busy for a bit
        self.fm.intf_mut().set_busy_end(busy_cycles);
    }

    /// Writes to the chip: the address port at offset 0, the data port at 1,
    /// the upper address port at 2 and the upper data port at 3.
    pub fn write(&mut self, offset: u32, data: u8) {
        match offset & 3 {
            0 => self.write_address(data),    // address port
            1 => self.write_data(data),       // data port
            2 => self.write_address_hi(data), // upper address port
            _ => self.write_data_hi(data),    // upper data port
        }
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<3>]) {
        let base = self.ssg_resampler.sampindex();

        // FM output is just repeated the prescale number of times; note that
        // 0 is a special 1.5 case
        if self.fm_samples_per_output != 0 {
            let repeat = u32::from(self.fm_samples_per_output);
            for (samp, out) in output.iter_mut().enumerate() {
                if base.wrapping_add(samp as u32).is_multiple_of(repeat) {
                    self.clock_fm_and_adpcm();
                }
                out.data[0] = self.last_fm.data[0];
                out.data[1] = self.last_fm.data[1];
            }
        } else {
            for (samp, out) in output.iter_mut().enumerate() {
                let step = base.wrapping_add(samp as u32) % 3;
                if step == 0 {
                    self.clock_fm_and_adpcm();
                }
                out.data[0] = self.last_fm.data[0];
                out.data[1] = self.last_fm.data[1];
                if step == 1 {
                    self.clock_fm_and_adpcm();
                    out.data[0] = (out.data[0] + self.last_fm.data[0]) / 2;
                    out.data[1] = (out.data[1] + self.last_fm.data[1]) / 2;
                }
            }
        }

        // resample the SSG as configured
        self.ssg_resampler.resample(&mut self.ssg, output);
    }

    /// Updates the derived values for the fidelity; the prescale is fixed.
    ///
    /// ```text
    /// Fidelity:   ---- minimum ----    ---- medium -----    ---- maximum-----
    ///              rate = clock/144     rate = clock/144     rate = clock/16
    /// Prescale    FM rate  SSG rate    FM rate  SSG rate    FM rate  SSG rate
    ///     6          1:1     2:9          1:1     2:9         9:1     2:1
    /// ```
    fn update_prescale(&mut self) {
        // compute the number of FM samples per output sample, and select the
        // resampler function
        if self.fidelity == OpnFidelity::Min || self.fidelity == OpnFidelity::Med {
            self.fm_samples_per_output = 1;
            self.ssg_resampler.configure(2, 9);
        } else {
            self.fm_samples_per_output = 9;
            self.ssg_resampler.configure(2, 1);
        }

        // if overriding the SSG, override the configuration with the nop
        // resampler to at least keep the sample index moving forward
        if self.ssg.overridden() {
            self.ssg_resampler.configure(0, 0);
        }
    }

    /// Clocks the FM and ADPCM state.
    fn clock_fm_and_adpcm(&mut self) {
        // top bit of the IRQ enable flags controls 3-channel vs 6-channel
        // mode
        let fmmask = if bitfield(u32::from(self.irq_enable), 7, 1) != 0 {
            0x3f
        } else {
            0x07
        };

        // clock the system
        let env_counter = self.fm.clock(Self::ALL_CHANNELS);

        // clock the ADPCM-A engine on every envelope cycle (channels 4 and 5
        // clock every 2 envelope clocks)
        if bitfield(env_counter, 0, 2) == 0 {
            let mask = if bitfield(env_counter, 2, 1) != 0 {
                0x0f
            } else {
                0x3f
            };
            self.adpcm_a.clock(mask, self.fm.intf_mut());
        }

        // update the FM content; OPNA is 13-bit with no intermediate
        // clipping
        self.fm
            .output(&mut self.last_fm.clear().data, 1, 32767, fmmask);

        // mix in the ADPCM
        self.adpcm_a.output(&mut self.last_fm, 0x3f);
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

    /// Returns the SSG engine, and through it its override.
    pub fn ssg(&self) -> &SsgEngine<O> {
        &self.ssg
    }

    /// Returns the SSG engine mutably, and through it its override.
    pub fn ssg_mut(&mut self) -> &mut SsgEngine<O> {
        &mut self.ssg
    }

    /// Returns the FM register file as last written, for a diagnostic: both
    /// banks, 0x000 to 0x1FF, with the frequency latches at 0xB8 and 0xB9.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}

//*********************************************************
//  YM2610, YM2610B
//*********************************************************

/// The Yamaha YM2610 (OPNB): the OPNA's FM engine with four of its six
/// channels, 2, 3, 5 and 6 (ymfm's channel mask 0x36), two timers, an SSG
/// without I/O ports, six ADPCM-A channels and an ADPCM-B channel, both
/// playing from external memory, on three outputs: FM and ADPCM left and
/// right, then the SSG mixed.
///
/// ADPCM-A memory is the interface's `AdpcmA` class and ADPCM-B memory the
/// `AdpcmB` class, both addressed in 256-byte units. Register 0x1C resets
/// and masks the end-of-sample flags, which the extended status reads.
#[derive(Clone)]
pub struct Ym2610<I: Interface = NullInterface, O: SsgOverride = NoSsgOverride> {
    /// The configured fidelity.
    fidelity: OpnFidelity,
    /// The address register.
    address: u16,
    /// The FM channel mask.
    fm_mask: u8,
    /// How many output samples each FM sample is repeated for.
    fm_samples_per_output: u8,
    /// The end-of-sample signals.
    eos_status: u8,
    /// The flag mask control.
    flag_mask: u8,
    /// The last FM output.
    last_fm: Output<2>,
    /// The core FM engine.
    fm: FmEngine<OpnaRegisters, I>,
    /// The SSG engine.
    ssg: SsgEngine<O>,
    /// The SSG resampler.
    ssg_resampler: SsgResampler<2, true>,
    /// The ADPCM-A engine.
    adpcm_a: AdpcmAEngine,
    /// The ADPCM-B engine.
    adpcm_b: AdpcmBEngine,
}

impl<I: Interface> Ym2610<I> {
    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        Self::build(intf, 0x36, None)
    }
}

impl<I: Interface, O: SsgOverride> Ym2610<I, O> {
    const EOS_FLAGS_MASK: u8 = 0xbf;

    /// The number of FM outputs.
    pub const FM_OUTPUTS: usize = 2;
    /// The number of SSG outputs: the three channels mixed into one.
    pub const SSG_OUTPUTS: usize = 1;
    /// The number of outputs: left, right, then the SSG.
    pub const OUTPUTS: usize = Self::FM_OUTPUTS + Self::SSG_OUTPUTS;

    /// Creates a chip with `ssg_override` in place of its SSG: as ymfm's
    /// `ssg_override` followed by `set_fidelity` at the default, which
    /// configures the resampler to leave the SSG output alone, then a reset
    /// (which resets the override).
    pub fn with_ssg_override(intf: I, ssg_override: O) -> Self {
        Self::build(intf, 0x36, Some(ssg_override))
    }

    fn build(intf: I, channel_mask: u8, ssg_override: Option<O>) -> Self {
        let mut chip = Ym2610 {
            fidelity: OpnFidelity::Max,
            address: 0,
            fm_mask: channel_mask,
            fm_samples_per_output: 0,
            eos_status: 0x00,
            flag_mask: Self::EOS_FLAGS_MASK,
            last_fm: Output::default(),
            fm: FmEngine::new(intf),
            ssg: SsgEngine::new(),
            ssg_resampler: SsgResampler::new(),
            adpcm_a: AdpcmAEngine::new(8),
            adpcm_b: AdpcmBEngine::new(8),
        };
        chip.update_prescale();
        if let Some(ssg_override) = ssg_override {
            chip.ssg_override(ssg_override);
            chip.set_fidelity(chip.fidelity);
        }
        chip.reset();
        chip
    }

    /// Installs an override, which from now on takes the SSG's resets,
    /// reads and writes. As in ymfm, the resampler keeps its configuration
    /// until the fidelity next changes.
    pub fn ssg_override(&mut self, ssg_override: O) {
        self.ssg.set_override(ssg_override);
    }

    /// Sets the fidelity, which sets the output rate.
    pub fn set_fidelity(&mut self, fidelity: OpnFidelity) {
        self.fidelity = fidelity;
        self.update_prescale();
    }

    /// Returns the configured fidelity.
    pub fn fidelity(&self) -> OpnFidelity {
        self.fidelity
    }

    /// Resets the chip.
    pub fn reset(&mut self) {
        // reset the engines
        self.fm.reset();
        self.ssg.reset();
        self.adpcm_a.reset();
        self.adpcm_b.reset();

        // initialize our special interrupt states
        self.eos_status = 0x00;
        self.flag_mask = Self::EOS_FLAGS_MASK;
    }

    /// Returns the output sample rate for an input clock.
    pub fn sample_rate(&self, input_clock: u32) -> u32 {
        match self.fidelity {
            OpnFidelity::Min => input_clock / 144,
            OpnFidelity::Med => input_clock / 144,
            OpnFidelity::Max => input_clock / 16,
        }
    }

    /// Returns the clock the SSG runs at for an input clock, as a stand-alone
    /// SSG would be clocked.
    pub fn ssg_effective_clock(&self, input_clock: u32) -> u32 {
        input_clock / 4
    }

    /// Invalidates the cached operator data.
    pub fn invalidate_caches(&mut self) {
        self.fm.invalidate_caches();
    }

    /// Reads the status register: the timers and busy.
    pub fn read_status(&mut self) -> u8 {
        let mut result = self.fm.status()
            & (FmEngine::<OpnaRegisters, I>::STATUS_TIMERA
                | FmEngine::<OpnaRegisters, I>::STATUS_TIMERB);
        if self.fm.intf_mut().is_busy() {
            result |= FmEngine::<OpnaRegisters, I>::STATUS_BUSY;
        }
        result
    }

    /// Reads the data register: an SSG register, 0xFF for the missing I/O
    /// ports, the ID code (1) at 0xFF, or 0.
    pub fn read_data(&mut self) -> u8 {
        let mut result = 0;
        if self.address < 0x0e {
            // 00-0D: Read from SSG
            result = self
                .ssg
                .read(self.fm.intf_mut(), u32::from(self.address & 0x0f));
        } else if self.address < 0x10 {
            // 0E-0F: I/O ports not supported
            result = 0xff;
        } else if self.address == 0xff {
            // FF: ID code
            result = 1;
        }
        result
    }

    /// Reads the extended status register: the end-of-sample flags, ADPCM-A
    /// channels in bits 0 to 5 and ADPCM-B in bit 7, less the masked ones.
    pub fn read_status_hi(&mut self) -> u8 {
        self.eos_status & self.flag_mask
    }

    /// Reads the upper data register, which reads 0.
    pub fn read_data_hi(&mut self) -> u8 {
        0
    }

    /// Reads from the chip: the status at offset 0, the data at 1, the
    /// extended status at 2 and the upper data at 3.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 3 {
            0 => self.read_status(),    // status port, YM2203 compatible
            1 => self.read_data(),      // data port (only SSG)
            2 => self.read_status_hi(), // status port, extended
            _ => self.read_data_hi(),   // ADPCM-B data
        }
    }

    /// Writes the address register.
    pub fn write_address(&mut self, data: u8) {
        // just set the address
        self.address = u16::from(data);
    }

    /// Writes the data register.
    pub fn write_data(&mut self, mut data: u8) {
        // ignore if paired with upper address
        if bitfield(u32::from(self.address), 8, 1) != 0 {
            return;
        }

        if self.address < 0x0e {
            // 00-0D: write to SSG
            self.ssg
                .write(self.fm.intf_mut(), u32::from(self.address & 0x0f), data);
        } else if self.address < 0x10 {
            // 0E-0F: I/O ports not supported
        } else if self.address < 0x1c {
            // 10-1B: write to ADPCM-B; the YM2610 effectively forces external
            // mode on, and disables recording
            if self.address == 0x10 {
                data = (data | 0x20) & !0x40;
            }
            self.adpcm_b
                .write(u32::from(self.address & 0x0f), data, self.fm.intf_mut());
        } else if self.address == 0x1c {
            // 1C: EOS flag reset
            self.flag_mask = !data & Self::EOS_FLAGS_MASK;
            self.eos_status &= !(data & Self::EOS_FLAGS_MASK);
        } else {
            // 1D-FF: write to FM
            self.fm.write(self.address, data);
        }

        // mark busy for a bit
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);
    }

    /// Writes the upper address register.
    pub fn write_address_hi(&mut self, data: u8) {
        // just set the address
        self.address = 0x100 | u16::from(data);
    }

    /// Writes the upper data register.
    pub fn write_data_hi(&mut self, data: u8) {
        // ignore if paired with upper address
        if bitfield(u32::from(self.address), 8, 1) == 0 {
            return;
        }

        if self.address < 0x130 {
            // 100-12F: write to ADPCM-A
            self.adpcm_a.write(u32::from(self.address & 0x3f), data);
        } else {
            // 130-1FF: write to FM
            self.fm.write(self.address, data);
        }

        // mark busy for a bit
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);
    }

    /// Writes to the chip: the address port at offset 0, the data port at 1,
    /// the upper address port at 2 and the upper data port at 3.
    pub fn write(&mut self, offset: u32, data: u8) {
        match offset & 3 {
            0 => self.write_address(data),    // address port
            1 => self.write_data(data),       // data port
            2 => self.write_address_hi(data), // upper address port
            _ => self.write_data_hi(data),    // upper data port
        }
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<3>]) {
        let base = self.ssg_resampler.sampindex();

        // FM output is just repeated the prescale number of times
        let repeat = u32::from(self.fm_samples_per_output);
        for (samp, out) in output.iter_mut().enumerate() {
            if base.wrapping_add(samp as u32).is_multiple_of(repeat) {
                self.clock_fm_and_adpcm();
            }
            out.data[0] = self.last_fm.data[0];
            out.data[1] = self.last_fm.data[1];
        }

        // resample the SSG as configured
        self.ssg_resampler.resample(&mut self.ssg, output);
    }

    /// Updates the derived values for the fidelity; the prescale is fixed.
    ///
    /// ```text
    /// Fidelity:   ---- minimum ----    ---- medium -----    ---- maximum-----
    ///              rate = clock/144     rate = clock/144     rate = clock/16
    /// Prescale    FM rate  SSG rate    FM rate  SSG rate    FM rate  SSG rate
    ///     6          1:1     2:9          1:1     2:9         9:1     2:1
    /// ```
    fn update_prescale(&mut self) {
        // compute the number of FM samples per output sample, and select the
        // resampler function
        if self.fidelity == OpnFidelity::Min || self.fidelity == OpnFidelity::Med {
            self.fm_samples_per_output = 1;
            self.ssg_resampler.configure(2, 9);
        } else {
            self.fm_samples_per_output = 9;
            self.ssg_resampler.configure(2, 1);
        }

        // if overriding the SSG, override the configuration with the nop
        // resampler to at least keep the sample index moving forward
        if self.ssg.overridden() {
            self.ssg_resampler.configure(0, 0);
        }
    }

    /// Clocks the FM and ADPCM state.
    fn clock_fm_and_adpcm(&mut self) {
        // clock the system
        let env_counter = self.fm.clock(u32::from(self.fm_mask));

        // clock the ADPCM-A engine on every envelope cycle
        if bitfield(env_counter, 0, 2) == 0 {
            self.eos_status |= self.adpcm_a.clock(0x3f, self.fm.intf_mut()) as u8;
        }

        // clock the ADPCM-B engine every cycle
        self.adpcm_b.clock(self.fm.intf_mut());

        // we track the last ADPCM-B EOS value in bit 6 (which is hidden from
        // callers); if it changed since the last sample, update the visible
        // EOS state in bit 7
        let live_eos = if (self.adpcm_b.status() & AdpcmBEngine::STATUS_EOS) != 0 {
            0x40
        } else {
            0x00
        };
        if ((live_eos ^ self.eos_status) & 0x40) != 0 {
            self.eos_status = (self.eos_status & !0xc0) | live_eos | (live_eos << 1);
        }

        // update the FM content; OPNB is 13-bit with no intermediate clipping
        self.fm.output(
            &mut self.last_fm.clear().data,
            1,
            32767,
            u32::from(self.fm_mask),
        );

        // mix in the ADPCM and clamp
        self.adpcm_a.output(&mut self.last_fm, 0x3f);
        self.adpcm_b.output(&mut self.last_fm, 1);
        self.last_fm.clamp16();
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

    /// Returns the SSG engine, and through it its override.
    pub fn ssg(&self) -> &SsgEngine<O> {
        &self.ssg
    }

    /// Returns the SSG engine mutably, and through it its override.
    pub fn ssg_mut(&mut self) -> &mut SsgEngine<O> {
        &mut self.ssg
    }

    /// Returns the FM register file as last written, for a diagnostic: both
    /// banks, 0x000 to 0x1FF, with the frequency latches at 0xB8 and 0xB9.
    /// The SSG, ADPCM and flag registers are not kept there.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}

/// The Yamaha YM2610B (OPNB2): the YM2610 with all six FM channels.
///
/// ymfm derives it from the YM2610 with a channel mask of 0x3F; this wraps
/// one.
#[derive(Clone)]
pub struct Ym2610b<I: Interface = NullInterface, O: SsgOverride = NoSsgOverride>(Ym2610<I, O>);

impl<I: Interface> Ym2610b<I> {
    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        Ym2610b(Ym2610::build(intf, 0x3f, None))
    }
}

impl<I: Interface, O: SsgOverride> Ym2610b<I, O> {
    /// The number of FM outputs.
    pub const FM_OUTPUTS: usize = 2;
    /// The number of SSG outputs: the three channels mixed into one.
    pub const SSG_OUTPUTS: usize = 1;
    /// The number of outputs: left, right, then the SSG.
    pub const OUTPUTS: usize = Self::FM_OUTPUTS + Self::SSG_OUTPUTS;

    /// Creates a chip with `ssg_override` in place of its SSG, as
    /// [`Ym2610::with_ssg_override`] does.
    pub fn with_ssg_override(intf: I, ssg_override: O) -> Self {
        Ym2610b(Ym2610::build(intf, 0x3f, Some(ssg_override)))
    }

    /// Installs an override; see [`Ym2610::ssg_override`].
    pub fn ssg_override(&mut self, ssg_override: O) {
        self.0.ssg_override(ssg_override);
    }

    /// Sets the fidelity, which sets the output rate.
    pub fn set_fidelity(&mut self, fidelity: OpnFidelity) {
        self.0.set_fidelity(fidelity);
    }

    /// Returns the configured fidelity.
    pub fn fidelity(&self) -> OpnFidelity {
        self.0.fidelity()
    }

    /// Resets the chip.
    pub fn reset(&mut self) {
        self.0.reset();
    }

    /// Returns the output sample rate for an input clock.
    pub fn sample_rate(&self, input_clock: u32) -> u32 {
        self.0.sample_rate(input_clock)
    }

    /// Returns the clock the SSG runs at for an input clock.
    pub fn ssg_effective_clock(&self, input_clock: u32) -> u32 {
        self.0.ssg_effective_clock(input_clock)
    }

    /// Invalidates the cached operator data.
    pub fn invalidate_caches(&mut self) {
        self.0.invalidate_caches();
    }

    /// Reads the status register; see [`Ym2610::read_status`].
    pub fn read_status(&mut self) -> u8 {
        self.0.read_status()
    }

    /// Reads the data register; see [`Ym2610::read_data`].
    pub fn read_data(&mut self) -> u8 {
        self.0.read_data()
    }

    /// Reads the extended status register; see [`Ym2610::read_status_hi`].
    pub fn read_status_hi(&mut self) -> u8 {
        self.0.read_status_hi()
    }

    /// Reads the upper data register, which reads 0.
    pub fn read_data_hi(&mut self) -> u8 {
        self.0.read_data_hi()
    }

    /// Reads from the chip; see [`Ym2610::read`].
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

    /// Writes the upper address register.
    pub fn write_address_hi(&mut self, data: u8) {
        self.0.write_address_hi(data);
    }

    /// Writes the upper data register.
    pub fn write_data_hi(&mut self, data: u8) {
        self.0.write_data_hi(data);
    }

    /// Writes to the chip; see [`Ym2610::write`].
    pub fn write(&mut self, offset: u32, data: u8) {
        self.0.write(offset, data);
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<3>]) {
        self.0.generate(output);
    }

    /// Reports the expiry of timer `tnum` (0 or 1), which the interface's
    /// `set_timer` asked for.
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

    /// Returns the SSG engine, and through it its override.
    pub fn ssg(&self) -> &SsgEngine<O> {
        self.0.ssg()
    }

    /// Returns the SSG engine mutably, and through it its override.
    pub fn ssg_mut(&mut self) -> &mut SsgEngine<O> {
        self.0.ssg_mut()
    }

    /// Returns the FM register file as last written, for a diagnostic.
    pub fn registers(&self) -> &[u8] {
        self.0.registers()
    }
}

//*********************************************************
//  YM2612
//*********************************************************

/// The Yamaha YM2612 (OPN2): six 4-operator channels with the OPNA's LFO and
/// SSG envelopes, channel 3's special and CSM modes, two timers, and a DAC
/// that can replace channel 6, through a 9-bit multiplexed DAC with the
/// "ladder effect" discontinuity around zero.
#[derive(Clone)]
pub struct Ym2612<I: Interface = NullInterface> {
    /// The address register.
    address: u16,
    /// The 9-bit DAC data.
    dac_data: u16,
    /// Whether the DAC is enabled.
    dac_enable: u8,
    /// The core FM engine.
    fm: FmEngine<OpnaRegisters, I>,
}

impl<I: Interface> Ym2612<I> {
    /// The number of outputs: left and right.
    pub const OUTPUTS: usize = 2;

    const ALL_CHANNELS: u32 = FmEngine::<OpnaRegisters, I>::ALL_CHANNELS;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        let mut chip = Ym2612 {
            address: 0,
            dac_data: 0,
            dac_enable: 0,
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
        let mut result = self.fm.status();
        if self.fm.intf_mut().is_busy() {
            result |= FmEngine::<OpnaRegisters, I>::STATUS_BUSY;
        }
        result
    }

    /// Reads from the chip: the status at offset 0; the other three offsets
    /// read 0.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 3 {
            0 => self.read_status(), // status port, YM2203 compatible
            _ => 0,                  // data port (unused), status port extended, data port
        }
    }

    /// Writes the address register.
    pub fn write_address(&mut self, data: u8) {
        // just set the address
        self.address = u16::from(data);
    }

    /// Writes the data register.
    pub fn write_data(&mut self, data: u8) {
        // ignore if paired with upper address
        if bitfield(u32::from(self.address), 8, 1) != 0 {
            return;
        }

        if self.address == 0x2a {
            // 2A: DAC data (most significant 8 bits)
            self.dac_data = (self.dac_data & !0x1fe) | (u16::from(data ^ 0x80) << 1);
        } else if self.address == 0x2b {
            // 2B: DAC enable (bit 7)
            self.dac_enable = bitfield(u32::from(data), 7, 1) as u8;
        } else if self.address == 0x2c {
            // 2C: test/low DAC bit
            self.dac_data = (self.dac_data & !1) | bitfield(u32::from(data), 3, 1) as u16;
        } else {
            // 00-29, 2D-FF: write to FM
            self.fm.write(self.address, data);
        }

        // mark busy for a bit
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);
    }

    /// Writes the upper address register.
    pub fn write_address_hi(&mut self, data: u8) {
        // just set the address
        self.address = 0x100 | u16::from(data);
    }

    /// Writes the upper data register.
    pub fn write_data_hi(&mut self, data: u8) {
        // ignore if paired with lower address
        if bitfield(u32::from(self.address), 8, 1) == 0 {
            return;
        }

        // 100-1FF: write to FM
        self.fm.write(self.address, data);

        // mark busy for a bit
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);
    }

    /// Writes to the chip: the address port at offset 0, the data port at 1,
    /// the upper address port at 2 and the upper data port at 3.
    pub fn write(&mut self, offset: u32, data: u8) {
        match offset & 3 {
            0 => self.write_address(data),    // address port
            1 => self.write_data(data),       // data port
            2 => self.write_address_hi(data), // upper address port
            _ => self.write_data_hi(data),    // upper data port
        }
    }

    /// Simulates the DAC discontinuity, the YM2612's "ladder effect".
    const fn dac_discontinuity(value: i32) -> i32 {
        if value < 0 {
            value - 3
        } else {
            value + 4
        }
    }

    /// Returns the DAC data as a signed 9-bit value.
    fn dac_value(&self) -> i32 {
        // `int16_t(m_dac_data << 7) >> 7`
        i32::from(((u32::from(self.dac_data) << 7) as i16) >> 7)
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<2>]) {
        for out in output.iter_mut() {
            // clock the system
            self.fm.clock(Self::ALL_CHANNELS);

            // sum individual channels to apply DAC discontinuity on each
            out.clear();
            let mut temp = Output::<2>::default();

            // first do FM-only channels; OPN2 is 9-bit with intermediate
            // clipping
            let last_fm_channel = if self.dac_enable != 0 { 5 } else { 6 };
            for chan in 0..last_fm_channel {
                self.fm.output(&mut temp.clear().data, 5, 256, 1 << chan);
                out.data[0] += Self::dac_discontinuity(temp.data[0]);
                out.data[1] += Self::dac_discontinuity(temp.data[1]);
            }

            // add in DAC
            if self.dac_enable != 0 {
                // DAC enabled: start with DAC value then add the first 5
                // channels only
                let dacval = Self::dac_discontinuity(self.dac_value());
                let regs = self.fm.regs();
                out.data[0] += if regs.ch_output_0(0x102) != 0 {
                    dacval
                } else {
                    Self::dac_discontinuity(0)
                };
                out.data[1] += if regs.ch_output_1(0x102) != 0 {
                    dacval
                } else {
                    Self::dac_discontinuity(0)
                };
            }

            // output is technically multiplexed rather than mixed, but that
            // requires a better sound mixer than we usually have, so just
            // average over the six channels; also apply a 64/65 factor to
            // account for the discontinuity adjustment above
            out.data[0] = (out.data[0] * 128) * 64 / (6 * 65);
            out.data[1] = (out.data[1] * 128) * 64 / (6 * 65);
        }
    }

    /// Starts a sample the YM3438 and YMF276 way: clocks the system, then
    /// sums the FM channels and the DAC with `rshift` and `clipmax`.
    fn generate_mixed(&mut self, out: &mut Output<2>, rshift: u32, clipmax: i32) {
        // clock the system
        self.fm.clock(Self::ALL_CHANNELS);

        if self.dac_enable == 0 {
            // DAC disabled: all 6 channels sum together
            self.fm
                .output(&mut out.clear().data, rshift, clipmax, Self::ALL_CHANNELS);
        } else {
            // DAC enabled: start with DAC value then add the first 5 channels
            // only
            let dacval = self.dac_value();
            let regs = self.fm.regs();
            out.data[0] = if regs.ch_output_0(0x102) != 0 {
                dacval
            } else {
                0
            };
            out.data[1] = if regs.ch_output_1(0x102) != 0 {
                dacval
            } else {
                0
            };
            self.fm.output(
                &mut out.data,
                rshift,
                clipmax,
                Self::ALL_CHANNELS ^ (1 << 5),
            );
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

    /// Returns the register file as last written, for a diagnostic: both
    /// banks, 0x000 to 0x1FF, with the frequency latches at 0xB8 and 0xB9.
    /// The DAC registers 0x2A to 0x2C are not kept there.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}

/// Implements the YM2612's interface on a chip that wraps it, `generate`
/// apart.
macro_rules! ym2612_wrapper {
    ($chip:ident) => {
        impl<I: Interface> $chip<I> {
            /// The number of outputs: left and right.
            pub const OUTPUTS: usize = 2;

            /// Creates a chip that talks to the system through `intf`, reset.
            pub fn new(intf: I) -> Self {
                $chip(Ym2612::new(intf))
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

            /// Reads the status register.
            pub fn read_status(&mut self) -> u8 {
                self.0.read_status()
            }

            /// Reads from the chip: the status at offset 0; the other three
            /// offsets read 0.
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

            /// Writes the upper address register.
            pub fn write_address_hi(&mut self, data: u8) {
                self.0.write_address_hi(data);
            }

            /// Writes the upper data register.
            pub fn write_data_hi(&mut self, data: u8) {
                self.0.write_data_hi(data);
            }

            /// Writes to the chip: the address port at offset 0, the data
            /// port at 1, the upper address port at 2 and the upper data
            /// port at 3.
            pub fn write(&mut self, offset: u32, data: u8) {
                self.0.write(offset, data);
            }

            /// Reports the expiry of timer `tnum` (0 or 1), which the
            /// interface's `set_timer` asked for.
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
//  YM3438
//*********************************************************

/// The Yamaha YM3438 (OPN2C), the CMOS YM2612: the same chip, without the
/// DAC discontinuity, its channels summed with intermediate clipping.
#[derive(Clone)]
pub struct Ym3438<I: Interface = NullInterface>(Ym2612<I>);

ym2612_wrapper!(Ym3438);

impl<I: Interface> Ym3438<I> {
    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<2>]) {
        for out in output.iter_mut() {
            // first do FM-only channels; OPN2C is 9-bit with intermediate
            // clipping
            self.0.generate_mixed(out, 5, 256);

            // YM3438 doesn't have the same DAC discontinuity, though its
            // output is multiplexed like the YM2612
            out.data[0] = (out.data[0] * 128) / 6;
            out.data[1] = (out.data[1] * 128) / 6;
        }
    }
}

//*********************************************************
//  YMF276
//*********************************************************

/// The Yamaha YMF276 (OPN2L), the low-voltage YM3438: the same chip at 14
/// bits, properly mixed, for an external DAC.
#[derive(Clone)]
pub struct Ymf276<I: Interface = NullInterface>(Ym2612<I>);

ym2612_wrapper!(Ymf276);

impl<I: Interface> Ymf276<I> {
    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<2>]) {
        for out in output.iter_mut() {
            // first do FM-only channels; OPN2L is 14-bit with intermediate
            // clipping
            self.0.generate_mixed(out, 0, 8191);

            // YMF276 is properly mixed; it shifts down 1 bit before clamping
            out.data[0] = clamp(out.data[0] >> 1, -32768, 32767);
            out.data[1] = clamp(out.data[1] >> 1, -32768, 32767);
        }
    }
}
