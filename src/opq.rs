// This file is a Rust translation of ymfm's `ymfm_opq.h` and `ymfm_opq.cpp`
// at commit 81aec25 (https://github.com/aaronsgiles/ymfm), with changes: the
// two waveforms are computed once at compile time and shared rather than held
// per chip; a register write returns its key-on target rather than writing
// through output parameters; a write past the end of the register file,
// which ymfm asserts against, is ignored; key-on logging, the temporary debug
// prints and save states are gone; a new chip is reset, where ymfm leaves its
// register file undefined until `reset()`; and a chip owns its interface.
// Every computation that reaches a sample, a status bit or a timer is kept as
// ymfm writes it.
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

//! The OPQ: the YM3806 and the YM3533.
//!
//! ymfm notes that the chip is not officially documented; what it models
//! comes from Jari Kangas's reverse engineering of the PSR-70, and the OPQ
//! appears to be basically a mixture of OPM and OPN. Several parts are
//! guesses upstream (the timer, the LFO, the key code, the detune range),
//! and this translation keeps them exactly as ymfm guesses them.
//!
//! The register map, as ymfm documents it:
//!
//! ```text
//! System-wide registers:
//!      03 xxxxxxxx Timer control (unknown; 0x71 causes interrupts at ~10ms)
//!      04 ----x--- LFO disable
//!         -----xxx LFO frequency (0=~4Hz, 6=~10Hz, 7=~47Hz)
//!      05 -x------ Key on/off operator 4
//!         --x----- Key on/off operator 3
//!         ---x---- Key on/off operator 2
//!         ----x--- Key on/off operator 1
//!         -----xxx Channel select
//!
//! Per-channel registers (channel in address bits 0-2)
//!   10-17 x------- Pan right
//!         -x------ Pan left
//!         --xxx--- Feedback level for operator 1 (0-7)
//!         -----xxx Operator connection algorithm (0-7)
//!   18-1F x------- Reverb
//!         -xxx---- PM sensitivity
//!         ------xx AM shift
//!   20-27 -xxx---- Block (0-7), Operator 2 & 4
//!         ----xxxx Frequency number upper 4 bits, Operator 2 & 4
//!   28-2F -xxx---- Block (0-7), Operator 1 & 3
//!         ----xxxx Frequency number upper 4 bits, Operator 1 & 3
//!   30-37 xxxxxxxx Frequency number lower 8 bits, Operator 2 & 4
//!   38-3F xxxxxxxx Frequency number lower 8 bits, Operator 1 & 3
//!
//! Per-operator registers (channel in address bits 0-2, operator in bits 3-4)
//!   40-5F 0-xxxxxx Detune value (0-63)
//!         1---xxxx Multiple value (0-15)
//!   60-7F -xxxxxxx Total level (0-127)
//!   80-9F xx------ Key scale rate (0-3)
//!         ---xxxxx Attack rate (0-31)
//!   A0-BF x------- LFO AM enable, retrigger disable
//!          x------ Waveform select
//!         ---xxxxx Decay rate (0-31)
//!   C0-DF ---xxxxx Sustain rate (0-31)
//!   E0-FF xxxx---- Sustain level (0-15)
//!         ----xxxx Release rate (0-15)
//!
//! Internal (fake) registers:
//!  100-11F ----xxxx Multiple value, remapped from 40-5F when bit 7 is set
//!
//! Diffs from OPM:
//!  - 2 frequencies/channel
//!  - retrigger disable
//!  - 2 waveforms
//!  - uses FNUM
//!  - reverb behavior
//!  - larger detune range
//! ```
//!
//! The chip has no address port: [`Ym3806::write`] takes the register
//! number as its offset. Of the timers only timer B exists, all of it
//! driven by bit 0 of register 0x03 with its period in bits 2-7 (ymfm marks
//! this as a guess); it sets status bit 2 and raises the IRQ line. There is
//! no noise generator, and writes never mark the chip busy.

use crate::fm::{
    abs_sin_attenuation, detune_adjustment, effective_rate, operator_list,
    opn_lfo_pm_phase_adjustment, FmEngine, FmRegisters, OpdataCache, MAX_CHANNELS, WAVEFORM_LENGTH,
};
use crate::ymfm::{
    bitfield, Interface, NullInterface, Output, EG_ATTACK, EG_DECAY, EG_RELEASE, EG_REVERB,
    EG_SUSTAIN,
};

//*********************************************************
//  OPQ REGISTERS
//*********************************************************

/// The OPQ's two waveforms, which ymfm builds in the register set's
/// constructor: a sine, and the sine's positive half with the negative half
/// held at the attenuation of phase 0.
static OPQ_WAVEFORMS: [[u16; WAVEFORM_LENGTH]; 2] = {
    let mut table = [[0u16; WAVEFORM_LENGTH]; 2];
    let mut index = 0;
    while index < WAVEFORM_LENGTH {
        let i = index as u32;
        table[0][index] = (abs_sin_attenuation(i) | (bitfield(i, 9, 1) << 15)) as u16;
        index += 1;
    }
    let zeroval = table[0][0];
    let mut index = 0;
    while index < WAVEFORM_LENGTH {
        table[1][index] = if bitfield(index as u32, 9, 1) != 0 {
            zeroval
        } else {
            table[0][index]
        };
        index += 1;
    }
    table
};

/// The OPQ register set, with its LFO.
#[derive(Clone)]
pub(crate) struct OpqRegisters {
    /// The LFO counter.
    lfo_counter: u32,
    /// The current LFO AM value.
    lfo_am: u8,
    /// The register data.
    regdata: [u8; OpqRegisters::REGISTERS],
}

impl OpqRegisters {
    /// The number of 8-bit registers, the remapped multiples included.
    const REGISTERS: usize = 0x120;

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
        &self.regdata
    }

    // system-wide registers
    fn lfo_enable(&self) -> u32 {
        self.byte(0x04, 3, 1, 0) ^ 1
    }
    fn lfo_rate(&self) -> u32 {
        self.byte(0x04, 0, 3, 0)
    }

    // per-channel registers
    fn ch_reverb(&self, choffs: u32) -> u32 {
        self.byte(0x18, 7, 1, choffs)
    }
    fn ch_lfo_pm_sens(&self, choffs: u32) -> u32 {
        self.byte(0x18, 4, 3, choffs)
    }
    fn ch_lfo_am_sens(&self, choffs: u32) -> u32 {
        self.byte(0x18, 0, 2, choffs)
    }
    fn ch_block_freq_24(&self, choffs: u32) -> u32 {
        self.word(0x20, 0, 7, 0x30, 0, 8, choffs)
    }
    fn ch_block_freq_13(&self, choffs: u32) -> u32 {
        self.word(0x28, 0, 7, 0x38, 0, 8, choffs)
    }

    // per-operator registers
    fn op_detune(&self, opoffs: u32) -> u32 {
        self.byte(0x40, 0, 6, opoffs)
    }
    fn op_multiple(&self, opoffs: u32) -> u32 {
        self.byte(0x100, 0, 4, opoffs)
    }
    fn op_total_level(&self, opoffs: u32) -> u32 {
        self.byte(0x60, 0, 7, opoffs)
    }
    fn op_ksr(&self, opoffs: u32) -> u32 {
        self.byte(0x80, 6, 2, opoffs)
    }
    fn op_attack_rate(&self, opoffs: u32) -> u32 {
        self.byte(0x80, 0, 5, opoffs)
    }
    fn op_waveform(&self, opoffs: u32) -> u32 {
        self.byte(0xa0, 6, 1, opoffs)
    }
    fn op_decay_rate(&self, opoffs: u32) -> u32 {
        self.byte(0xa0, 0, 5, opoffs)
    }
    fn op_sustain_rate(&self, opoffs: u32) -> u32 {
        self.byte(0xc0, 0, 5, opoffs)
    }
    fn op_sustain_level(&self, opoffs: u32) -> u32 {
        self.byte(0xe0, 4, 4, opoffs)
    }
    fn op_release_rate(&self, opoffs: u32) -> u32 {
        self.byte(0xe0, 0, 4, opoffs)
    }
}

impl FmRegisters for OpqRegisters {
    const OUTPUTS: usize = 2;
    const CHANNELS: usize = 8;
    const ALL_CHANNELS: u32 = (1 << Self::CHANNELS) - 1;
    const OPERATORS: usize = Self::CHANNELS * 4;
    const DEFAULT_PRESCALE: u32 = 2;
    const EG_CLOCK_DIVIDER: u32 = 3;
    const EG_HAS_REVERB: bool = true;
    const MODULATOR_DELAY: bool = false;
    const CSM_TRIGGER_MASK: u32 = Self::ALL_CHANNELS;
    const REG_MODE: u32 = 0x03;
    const STATUS_TIMERA: u8 = 0;
    const STATUS_TIMERB: u8 = 0x04;
    const STATUS_BUSY: u8 = 0x80;
    const STATUS_IRQ: u8 = 0;

    fn new() -> Self {
        OpqRegisters {
            lfo_counter: 0,
            lfo_am: 0,
            regdata: [0; Self::REGISTERS],
        }
    }

    fn reset(&mut self) {
        self.regdata = [0; Self::REGISTERS];

        // enable output on both channels by default
        self.regdata[0x10..0x18].fill(0xc0);
    }

    fn channel_offset(chnum: u32) -> u32 {
        chnum
    }

    fn operator_offset(opnum: u32) -> u32 {
        opnum
    }

    fn operator_map(&self, dest: &mut [u32; MAX_CHANNELS]) {
        // seems like the operators are not swizzled like they are on
        // OPM/OPN? (ymfm's question)
        dest[..8].copy_from_slice(&[
            operator_list(0, 8, 16, 24),  // Channel 0 operators
            operator_list(1, 9, 17, 25),  // Channel 1 operators
            operator_list(2, 10, 18, 26), // Channel 2 operators
            operator_list(3, 11, 19, 27), // Channel 3 operators
            operator_list(4, 12, 20, 28), // Channel 4 operators
            operator_list(5, 13, 21, 29), // Channel 5 operators
            operator_list(6, 14, 22, 30), // Channel 6 operators
            operator_list(7, 15, 23, 31), // Channel 7 operators
        ]);
    }

    fn write(&mut self, index: u16, data: u8) -> Option<(u32, u32)> {
        let mut index = usize::from(index);

        // ymfm asserts the index is in range; past the end it would write
        // out of bounds, and here the write is dropped
        if index >= Self::REGISTERS {
            return None;
        }

        // detune/multiple share a register based on the MSB of what is
        // written; remap the multiple values to 100-11F
        if (index & 0xe0) == 0x40 && bitfield(u32::from(data), 7, 1) != 0 {
            index += 0xc0;
        }

        self.regdata[index] = data;

        // handle writes to the key on index
        if index == 0x05 {
            let data = u32::from(data);
            return Some((bitfield(data, 0, 3), bitfield(data, 3, 4)));
        }
        None
    }

    fn clock_noise_and_lfo(&mut self) -> i32 {
        // OPQ LFO is not well-understood, but the enable and rate values
        // look a lot like OPN, so ymfm cribs from there as a starting point

        // if LFO not enabled (not present on OPN), quick exit with 0s
        if self.lfo_enable() == 0 {
            self.lfo_counter = 0;
            self.lfo_am = 0;
            return 0;
        }

        // this table is based on converting the frequencies in the
        // applications manual to clock dividers, based on the assumption of
        // a 7-bit LFO value
        const LFO_MAX_COUNT: [u8; 8] = [109, 78, 72, 68, 63, 45, 9, 6];
        let subcount = u32::from(self.lfo_counter as u8);
        self.lfo_counter = self.lfo_counter.wrapping_add(1);

        // when we cross the divider count, add enough to zero it and cause
        // an increment at bit 8; the 7-bit value lives from bits 8-14
        if subcount >= u32::from(LFO_MAX_COUNT[self.lfo_rate() as usize]) {
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
        // OPM maps AM quite differently from OPN

        // shift value for AM sensitivity is [*, 0, 1, 2], mapping to values
        // of [0, 23.9, 47.8, and 95.6dB]
        let am_sensitivity = self.ch_lfo_am_sens(choffs);
        if am_sensitivity == 0 {
            return 0;
        }

        // raw LFO AM value on OPM is 0-FF, which is already a factor of 2
        // larger than the OPN, putting our staring point at 2x theirs; this
        // works out since our minimum is 2x their maximum
        u32::from(self.lfo_am) << (am_sensitivity - 1)
    }

    fn noise_state(&self) -> u32 {
        0
    }

    fn cache_operator_data(&mut self, choffs: u32, opoffs: u32, cache: &mut OpdataCache) {
        // set up the easy stuff
        cache.waveform = self.op_waveform(opoffs) as u8;

        // get frequency from the appropriate registers
        let block_freq = if (opoffs & 8) != 0 {
            self.ch_block_freq_24(choffs)
        } else {
            self.ch_block_freq_13(choffs)
        };
        cache.block_freq = block_freq;

        // compute the keycode: block_freq is:
        //
        //     BBBFFFFFFFFFFFF
        //     ^^^^???
        //
        // keycode is not understood, so ymfm guesses it is like OPN: the
        // 5-bit keycode uses the top 4 bits plus a magic formula for the
        // final bit
        let mut keycode = bitfield(block_freq, 11, 4) << 1;

        // lowest bit is determined by a mix of next lower FNUM bits
        // according to this equation from the YM2608 manual:
        //
        //   (F11 & (F10 | F9 | F8)) | (!F11 & F10 & F9 & F8)
        //
        // for speed, we just look it up in a 16-bit constant
        keycode |= bitfield(0xfe80, bitfield(block_freq, 8, 4), 1);

        // detune adjustment: the detune values supported by the OPQ are a
        // much larger range (6 bits vs 3 bits) compared to any other known
        // FM chip; based on experiments, it seems that the extra bits
        // provide a bigger detune range rather than finer control, so until
        // we get true measurements just assemble a net detune value by
        // summing smaller detunes
        let detune = self.op_detune(opoffs) as i32 - 0x20;
        let abs_detune = detune.abs();
        let adjust = (abs_detune / 3) * detune_adjustment(3, keycode)
            + detune_adjustment((abs_detune % 3) as u32, keycode);
        cache.detune = if detune >= 0 { adjust } else { -adjust };

        // multiple value, as an x.1 value (0 means 0.5)
        const MULTIPLE_MAP: [u8; 16] = [1, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 24, 30, 32, 34, 36];
        cache.multiple = u32::from(MULTIPLE_MAP[self.op_multiple(opoffs) as usize]);

        // phase step, or PHASE_STEP_DYNAMIC if PM is active; this depends on
        // block_freq, detune, and multiple, so compute it after we've done
        // those
        cache.phase_step = if self.lfo_enable() == 0 || self.ch_lfo_pm_sens(choffs) == 0 {
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
        cache.eg_rate[EG_REVERB] = if self.ch_reverb(choffs) != 0 {
            5 * 4
        } else {
            cache.eg_rate[EG_RELEASE]
        };
        cache.eg_shift = 0;
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

        // extract frequency number (low 12 bits of block_freq)
        let mut fnum = bitfield(cache.block_freq, 0, 12);

        // if there's a non-zero PM sensitivity, compute the adjustment
        let pm_sensitivity = self.ch_lfo_pm_sens(choffs);
        if pm_sensitivity != 0 {
            // apply the phase adjustment based on the upper 7 bits of FNUM
            // and the PM depth parameters
            fnum = fnum.wrapping_add(opn_lfo_pm_phase_adjustment(
                bitfield(cache.block_freq, 5, 7),
                pm_sensitivity,
                lfo_raw_pm,
            ) as u32);

            // keep fnum to 12 bits
            fnum &= 0xfff;
        }

        // apply block shift to compute phase step
        let block = bitfield(cache.block_freq, 12, 3);
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
        &OPQ_WAVEFORMS[index as usize]
    }

    // the timer bits are ymfm's guesses ("???"): only timer B exists, and
    // bit 0 of 0x03 loads, enables and resets it at once
    fn timer_a_value(&self) -> u32 {
        0
    }
    fn timer_b_value(&self) -> u32 {
        self.byte(0x03, 2, 6, 0) | 0xc0
    }
    fn csm(&self) -> u32 {
        0
    }
    fn reset_timer_b(&self) -> u32 {
        self.byte(0x03, 0, 1, 0)
    }
    fn reset_timer_a(&self) -> u32 {
        0
    }
    fn enable_timer_b(&self) -> u32 {
        self.byte(0x03, 0, 1, 0)
    }
    fn enable_timer_a(&self) -> u32 {
        0
    }
    fn load_timer_b(&self) -> u32 {
        self.byte(0x03, 0, 1, 0)
    }
    fn load_timer_a(&self) -> u32 {
        0
    }

    fn ch_output_any(&self, choffs: u32) -> u32 {
        self.byte(0x10, 6, 2, choffs)
    }
    fn ch_output_0(&self, choffs: u32) -> u32 {
        self.byte(0x10, 6, 1, choffs)
    }
    fn ch_output_1(&self, choffs: u32) -> u32 {
        self.byte(0x10, 7, 1, choffs)
    }
    fn ch_output_2(&self, _choffs: u32) -> u32 {
        0
    }
    fn ch_output_3(&self, _choffs: u32) -> u32 {
        0
    }
    fn ch_feedback(&self, choffs: u32) -> u32 {
        self.byte(0x10, 3, 3, choffs)
    }
    fn ch_algorithm(&self, choffs: u32) -> u32 {
        self.byte(0x10, 0, 3, choffs)
    }

    fn op_lfo_am_enable(&self, opoffs: u32) -> u32 {
        self.byte(0xa0, 7, 1, opoffs)
    }
}

//*********************************************************
//  IMPLEMENTATION CLASSES
//*********************************************************

/// The Yamaha YM3806 (OPQ): eight 4-operator channels with two frequencies
/// each, two waveforms, a faux reverb envelope stage, an LFO, one timer, and
/// a stereo pair of outputs through a floating-point DAC.
#[derive(Clone)]
pub struct Ym3806<I: Interface = NullInterface> {
    /// The core FM engine.
    fm: FmEngine<OpqRegisters, I>,
}

impl<I: Interface> Ym3806<I> {
    /// The number of outputs: left and right.
    pub const OUTPUTS: usize = 2;

    const ALL_CHANNELS: u32 = FmEngine::<OpqRegisters, I>::ALL_CHANNELS;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        let mut chip = Ym3806 {
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
            result |= FmEngine::<OpqRegisters, I>::STATUS_BUSY;
        }
        result
    }

    /// Reads from the chip: the status at offset 0, and 0xFF anywhere else.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset {
            0 => self.read_status(), // status port
            _ => 0xff,               // unknown
        }
    }

    /// Does nothing: the chip has no address port, only direct writes.
    pub fn write_address(&mut self, _data: u8) {}

    /// Does nothing: the chip has no data port, only direct writes.
    pub fn write_data(&mut self, _data: u8) {}

    /// Writes register `offset` directly. As in ymfm, the offset is cut to
    /// 16 bits; a register past 0x11F is ignored.
    pub fn write(&mut self, offset: u32, data: u8) {
        // write the FM register
        self.fm.write(offset as u16, data);
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<2>]) {
        for out in output.iter_mut() {
            // clock the system
            self.fm.clock(Self::ALL_CHANNELS);

            // update the FM content; YM3806 is full 14-bit with no
            // intermediate clipping
            self.fm
                .output(&mut out.clear().data, 0, 32767, Self::ALL_CHANNELS);

            // YM3806 appears to go through a YM3012 DAC, which means we want
            // to apply the FP truncation logic to the outputs
            out.roundtrip_fp();
        }
    }

    /// Reports the expiry of timer `tnum` (0 or 1), which the interface's
    /// `set_timer` asked for; only timer B (1) is ever set.
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

    /// Returns the register file as last written, for a diagnostic: 0x120
    /// bytes, with the multiples written through 0x40-0x5F at 0x100-0x11F.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}

/// The Yamaha YM3533, which ymfm models as a YM3806.
#[derive(Clone)]
pub struct Ym3533<I: Interface = NullInterface>(Ym3806<I>);

impl<I: Interface> Ym3533<I> {
    /// The number of outputs: left and right.
    pub const OUTPUTS: usize = 2;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        Ym3533(Ym3806::new(intf))
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

    /// Reads from the chip: the status at offset 0, and 0xFF anywhere else.
    pub fn read(&mut self, offset: u32) -> u8 {
        self.0.read(offset)
    }

    /// Does nothing: the chip has no address port, only direct writes.
    pub fn write_address(&mut self, data: u8) {
        self.0.write_address(data);
    }

    /// Does nothing: the chip has no data port, only direct writes.
    pub fn write_data(&mut self, data: u8) {
        self.0.write_data(data);
    }

    /// Writes register `offset` directly. As in ymfm, the offset is cut to
    /// 16 bits; a register past 0x11F is ignored.
    pub fn write(&mut self, offset: u32, data: u8) {
        self.0.write(offset, data);
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<2>]) {
        self.0.generate(output);
    }

    /// Reports the expiry of timer `tnum` (0 or 1), which the interface's
    /// `set_timer` asked for; only timer B (1) is ever set.
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
