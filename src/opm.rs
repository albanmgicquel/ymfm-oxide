// This file is a Rust translation of ymfm's `ymfm_opm.h` and `ymfm_opm.cpp`
// at commit 81aec25 (https://github.com/aaronsgiles/ymfm), with changes: the
// sine table is computed once at compile time and shared rather than held
// per chip; a register write returns its key-on target rather than writing
// through output parameters; key-on logging and save states are gone; a new
// chip is reset, where ymfm leaves its register file undefined until
// `reset()`; and a chip owns its interface. Every computation that reaches a
// sample, a status bit or a timer is kept as ymfm writes it.
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

//! The OPM: the YM2151 and the YM2164 (OPP).
//!
//! The register map, as ymfm documents it:
//!
//! ```text
//! System-wide registers:
//!      01 xxxxxx-x Test register
//!         ------x- LFO reset
//!      08 -x------ Key on/off operator 4
//!         --x----- Key on/off operator 3
//!         ---x---- Key on/off operator 2
//!         ----x--- Key on/off operator 1
//!         -----xxx Channel select
//!      0F x------- Noise enable
//!         ---xxxxx Noise frequency
//!      10 xxxxxxxx Timer A value (upper 8 bits)
//!      11 ------xx Timer A value (lower 2 bits)
//!      12 xxxxxxxx Timer B value
//!      14 x------- CSM mode
//!         --x----- Reset timer B
//!         ---x---- Reset timer A
//!         ----x--- Enable timer B
//!         -----x-- Enable timer A
//!         ------x- Load timer B
//!         -------x Load timer A
//!      18 xxxxxxxx LFO frequency
//!      19 0xxxxxxx AM LFO depth
//!         1xxxxxxx PM LFO depth
//!      1B xx------ CT (2 output data lines)
//!         ------xx LFO waveform
//!
//! Per-channel registers (channel in address bits 0-2)
//!   20-27 x------- Pan right
//!         -x------ Pan left
//!         --xxx--- Feedback level for operator 1 (0-7)
//!         -----xxx Operator connection algorithm (0-7)
//!   28-2F -xxxxxxx Key code
//!   30-37 xxxxxx-- Key fraction
//!   38-3F -xxx---- LFO PM sensitivity
//!         ------xx LFO AM shift
//!
//! Per-operator registers (channel in address bits 0-2, operator in bits 3-4)
//!   40-5F -xxx---- Detune value (0-7)
//!         ----xxxx Multiple value (0-15)
//!   60-7F -xxxxxxx Total level (0-127)
//!   80-9F xx------ Key scale rate (0-3)
//!         ---xxxxx Attack rate (0-31)
//!   A0-BF x------- LFO AM enable
//!         ---xxxxx Decay rate (0-31)
//!   C0-DF xx------ Detune 2 value (0-3)
//!         ---xxxxx Sustain rate (0-31)
//!   E0-FF xxxx---- Sustain level (0-15)
//!         ----xxxx Release rate (0-15)
//!
//! Internal (fake) registers:
//!      1A -xxxxxxx PM depth
//! ```

use crate::fm::{
    abs_sin_attenuation, detune_adjustment, effective_rate, operator_list,
    opm_key_code_to_phase_step, FmEngine, FmRegisters, OpdataCache, MAX_CHANNELS, WAVEFORM_LENGTH,
};
use crate::ymfm::{
    bitfield, AccessClass, Interface, NullInterface, Output, EG_ATTACK, EG_DECAY, EG_RELEASE,
    EG_SUSTAIN,
};

//*********************************************************
//  OPM REGISTERS
//*********************************************************

/// LFO waveforms are 256 entries long.
const LFO_WAVEFORM_LENGTH: usize = 256;

/// The OPM's one waveform, the sine; ymfm builds it in the register set's
/// constructor.
static OPM_WAVEFORMS: [[u16; WAVEFORM_LENGTH]; 1] = {
    let mut table = [[0u16; WAVEFORM_LENGTH]; 1];
    let mut index = 0;
    while index < WAVEFORM_LENGTH {
        let i = index as u32;
        table[0][index] = (abs_sin_attenuation(i) | (bitfield(i, 9, 1) << 15)) as u16;
        index += 1;
    }
    table
};

/// The OPM register set, with its LFO and noise generator.
#[derive(Clone)]
pub(crate) struct OpmRegisters {
    /// The LFO counter.
    lfo_counter: u32,
    /// The noise LFSR state.
    noise_lfsr: u32,
    /// The noise counter.
    noise_counter: u8,
    /// The latched noise state.
    noise_state: u8,
    /// The latched LFO noise value.
    #[allow(dead_code)] // ymfm saves it but never reads it
    noise_lfo: u8,
    /// The current LFO AM value.
    lfo_am: u8,
    /// The register data.
    regdata: [u8; OpmRegisters::REGISTERS],
    /// The LFO waveforms: AM in the low 8 bits, PM in the upper 8.
    lfo_waveform: [[i16; LFO_WAVEFORM_LENGTH]; 4],
}

impl OpmRegisters {
    /// The number of 8-bit registers.
    const REGISTERS: usize = 0x100;

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
    #[allow(dead_code)]
    fn test(&self) -> u32 {
        self.byte(0x01, 0, 8, 0)
    }
    fn lfo_reset(&self) -> u32 {
        self.byte(0x01, 1, 1, 0)
    }
    fn noise_frequency(&self) -> u32 {
        self.byte(0x0f, 0, 5, 0) ^ 0x1f
    }
    fn lfo_rate(&self) -> u32 {
        self.byte(0x18, 0, 8, 0)
    }
    fn lfo_am_depth(&self) -> u32 {
        self.byte(0x19, 0, 7, 0)
    }
    fn lfo_pm_depth(&self) -> u32 {
        self.byte(0x1a, 0, 7, 0)
    }
    #[allow(dead_code)]
    fn output_bits(&self) -> u32 {
        self.byte(0x1b, 6, 2, 0)
    }
    fn lfo_waveform(&self) -> u32 {
        self.byte(0x1b, 0, 2, 0)
    }

    // per-channel registers
    fn ch_block_freq(&self, choffs: u32) -> u32 {
        self.word(0x28, 0, 7, 0x30, 2, 6, choffs)
    }
    fn ch_lfo_pm_sens(&self, choffs: u32) -> u32 {
        self.byte(0x38, 4, 3, choffs)
    }
    fn ch_lfo_am_sens(&self, choffs: u32) -> u32 {
        self.byte(0x38, 0, 2, choffs)
    }

    // per-operator registers
    fn op_detune(&self, opoffs: u32) -> u32 {
        self.byte(0x40, 4, 3, opoffs)
    }
    fn op_multiple(&self, opoffs: u32) -> u32 {
        self.byte(0x40, 0, 4, opoffs)
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
    fn op_decay_rate(&self, opoffs: u32) -> u32 {
        self.byte(0xa0, 0, 5, opoffs)
    }
    fn op_detune2(&self, opoffs: u32) -> u32 {
        self.byte(0xc0, 6, 2, opoffs)
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

impl FmRegisters for OpmRegisters {
    const OUTPUTS: usize = 2;
    const CHANNELS: usize = 8;
    const ALL_CHANNELS: u32 = (1 << Self::CHANNELS) - 1;
    const OPERATORS: usize = Self::CHANNELS * 4;
    const DEFAULT_PRESCALE: u32 = 2;
    const EG_CLOCK_DIVIDER: u32 = 3;
    const CSM_TRIGGER_MASK: u32 = Self::ALL_CHANNELS;
    const REG_MODE: u32 = 0x14;
    const STATUS_TIMERA: u8 = 0x01;
    const STATUS_TIMERB: u8 = 0x02;
    const STATUS_BUSY: u8 = 0x80;
    const STATUS_IRQ: u8 = 0;

    fn new() -> Self {
        // create the LFO waveforms; AM in the low 8 bits, PM in the upper 8;
        // waveforms are adjusted to match the pictures in the application
        // manual
        let mut lfo_waveform = [[0i16; LFO_WAVEFORM_LENGTH]; 4];
        #[allow(clippy::needless_range_loop)] // ymfm fills four tables per index
        for index in 0..LFO_WAVEFORM_LENGTH {
            let i = index as u32;
            // the C++ packs `am | (pm << 8)` into an int16_t
            let pack = |am: u8, pm: u8| (u16::from(am) | (u16::from(pm) << 8)) as i16;

            // waveform 0 is a sawtooth
            let am = (i ^ 0xff) as u8;
            let pm = i as u8;
            lfo_waveform[0][index] = pack(am, pm);

            // waveform 1 is a square wave
            let am = if bitfield(i, 7, 1) != 0 { 0 } else { 0xff };
            let pm = am ^ 0x80;
            lfo_waveform[1][index] = pack(am, pm);

            // waveform 2 is a triangle wave
            let am = if bitfield(i, 7, 1) != 0 {
                (i << 1) as u8
            } else {
                ((i ^ 0xff) << 1) as u8
            };
            let pm = if bitfield(i, 6, 1) != 0 { am } else { !am };
            lfo_waveform[2][index] = pack(am, pm);

            // waveform 3 is noise; it is filled in dynamically
            lfo_waveform[3][index] = 0;
        }
        OpmRegisters {
            lfo_counter: 0,
            noise_lfsr: 1,
            noise_counter: 0,
            noise_state: 0,
            noise_lfo: 0,
            lfo_am: 0,
            regdata: [0; Self::REGISTERS],
            lfo_waveform,
        }
    }

    fn reset(&mut self) {
        self.regdata = [0; Self::REGISTERS];

        // enable output on both channels by default
        self.regdata[0x20..0x28].fill(0xc0);
    }

    fn channel_offset(chnum: u32) -> u32 {
        chnum
    }

    fn operator_offset(opnum: u32) -> u32 {
        opnum
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
        dest[..8].copy_from_slice(&[
            operator_list(0, 16, 8, 24),  // Channel 0 operators
            operator_list(1, 17, 9, 25),  // Channel 1 operators
            operator_list(2, 18, 10, 26), // Channel 2 operators
            operator_list(3, 19, 11, 27), // Channel 3 operators
            operator_list(4, 20, 12, 28), // Channel 4 operators
            operator_list(5, 21, 13, 29), // Channel 5 operators
            operator_list(6, 22, 14, 30), // Channel 6 operators
            operator_list(7, 23, 15, 31), // Channel 7 operators
        ]);
    }

    fn write(&mut self, index: u16, data: u8) -> Option<(u32, u32)> {
        let index = usize::from(index);

        // LFO AM/PM depth are written to the same register (0x19); redirect
        // the PM depth to an unused neighbor (0x1a)
        if index == 0x19 {
            self.regdata[index + bitfield(u32::from(data), 7, 1) as usize] = data;
        } else if index != 0x1a {
            self.regdata[index] = data;
        }

        // handle writes to the key on index
        if index == 0x08 {
            let data = u32::from(data);
            return Some((bitfield(data, 0, 3), bitfield(data, 3, 4)));
        }
        None
    }

    fn clock_noise_and_lfo(&mut self) -> i32 {
        // base noise frequency is measured at 2x 1/2 FM frequency; this means
        // each tick counts as two steps against the noise counter
        let freq = self.noise_frequency();
        for _ in 0..2 {
            // evidence seems to suggest the LFSR is clocked continually and
            // just sampled at the noise frequency for output purposes; note
            // that the low 8 bits are the most recent 8 bits of history while
            // bits 8-24 contain the 17 bit LFSR state
            self.noise_lfsr <<= 1;
            self.noise_lfsr |=
                bitfield(self.noise_lfsr, 17, 1) ^ bitfield(self.noise_lfsr, 14, 1) ^ 1;

            // compare against the frequency and latch when we exceed it
            let count = self.noise_counter;
            self.noise_counter = self.noise_counter.wrapping_add(1);
            if u32::from(count) >= freq {
                self.noise_counter = 0;
                self.noise_state = bitfield(self.noise_lfsr, 17, 1) as u8;
            }
        }

        // treat the rate as a 4.4 floating-point step value with implied
        // leading 1; this matches exactly the frequencies in the application
        // manual, though it might not be implemented exactly this way on chip
        let rate = self.lfo_rate();
        self.lfo_counter = self
            .lfo_counter
            .wrapping_add((0x10 | bitfield(rate, 0, 4)) << bitfield(rate, 4, 4));

        // bit 1 of the test register is officially undocumented but has been
        // discovered to hold the LFO in reset while active
        if self.lfo_reset() != 0 {
            self.lfo_counter = 0;
        }

        // now pull out the non-fractional LFO value
        let lfo = bitfield(self.lfo_counter, 22, 8);

        // fill in the noise entry 1 ahead of our current position; this
        // ensures the current value remains stable for a full LFO clock and
        // effectively latches the running value when the LFO advances
        let lfo_noise = bitfield(self.noise_lfsr, 17, 8);
        self.lfo_waveform[3][((lfo + 1) & 0xff) as usize] = (lfo_noise | (lfo_noise << 8)) as i16;

        // fetch the AM/PM values based on the waveform; AM is unsigned and
        // encoded in the low 8 bits, while PM signed and encoded in the upper
        // 8 bits
        let ampm = i32::from(self.lfo_waveform[self.lfo_waveform() as usize][lfo as usize]);

        // apply depth to the AM value and store for later
        self.lfo_am = (((ampm & 0xff) * self.lfo_am_depth() as i32) >> 7) as u8;

        // apply depth to the PM value and return it
        ((ampm >> 8) * self.lfo_pm_depth() as i32) >> 7
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
        u32::from(self.noise_state)
    }

    fn cache_operator_data(&mut self, choffs: u32, opoffs: u32, cache: &mut OpdataCache) {
        // set up the easy stuff
        cache.waveform = 0;

        // get frequency from the channel
        let block_freq = self.ch_block_freq(choffs);
        cache.block_freq = block_freq;

        // compute the keycode: block_freq is:
        //
        //     BBBCCCCFFFFFF
        //     ^^^^^
        //
        // the 5-bit keycode is just the top 5 bits (block + top 2 bits of the
        // key code)
        let keycode = bitfield(block_freq, 8, 5);

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
        cache.phase_step = if self.lfo_pm_depth() == 0 || self.ch_lfo_pm_sens(choffs) == 0 {
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
        opoffs: u32,
        cache: &OpdataCache,
        lfo_raw_pm: i32,
    ) -> u32 {
        // OPM logic is rather unique here, due to extra detune and the use of
        // key codes (not to be confused with keycode)

        // start with coarse detune delta; table uses cents value from manual,
        // converted into 1/64ths
        const DETUNE2_DELTA: [i16; 4] = [
            0,
            ((600 * 64 + 50) / 100) as i16,
            ((781 * 64 + 50) / 100) as i16,
            ((950 * 64 + 50) / 100) as i16,
        ];
        let mut delta = i32::from(DETUNE2_DELTA[self.op_detune2(opoffs) as usize]);

        // add in the PM delta
        let pm_sensitivity = self.ch_lfo_pm_sens(choffs);
        if pm_sensitivity != 0 {
            // raw PM value is -127..128 which is +/- 200 cents; manual gives
            // these magnitudes in cents:
            //    0, +/-5, +/-10, +/-20, +/-50, +/-100, +/-400, +/-700
            // this roughly corresponds to shifting the 200-cent value:
            //    0  >> 5,  >> 4,  >> 3,  >> 2,  >> 1,   << 1,   << 2
            if pm_sensitivity < 6 {
                delta += lfo_raw_pm >> (6 - pm_sensitivity);
            } else {
                // `delta += uint32_t(lfo_raw_pm) << n` adds unsigned bits
                delta = delta.wrapping_add(((lfo_raw_pm as u32) << (pm_sensitivity - 5)) as i32);
            }
        }

        // apply delta and convert to a frequency number
        let mut phase_step = opm_key_code_to_phase_step(cache.block_freq, delta);

        // apply detune based on the keycode
        phase_step = phase_step.wrapping_add(cache.detune as u32);

        // apply frequency multiplier (which is cached as an x.1 value)
        phase_step.wrapping_mul(cache.multiple) >> 1
    }

    fn waveform(&self, index: u8) -> &[u16; WAVEFORM_LENGTH] {
        &OPM_WAVEFORMS[index as usize]
    }

    fn timer_a_value(&self) -> u32 {
        self.word(0x10, 0, 8, 0x11, 0, 2, 0)
    }
    fn timer_b_value(&self) -> u32 {
        self.byte(0x12, 0, 8, 0)
    }
    fn csm(&self) -> u32 {
        self.byte(0x14, 7, 1, 0)
    }
    fn reset_timer_b(&self) -> u32 {
        self.byte(0x14, 5, 1, 0)
    }
    fn reset_timer_a(&self) -> u32 {
        self.byte(0x14, 4, 1, 0)
    }
    fn enable_timer_b(&self) -> u32 {
        self.byte(0x14, 3, 1, 0)
    }
    fn enable_timer_a(&self) -> u32 {
        self.byte(0x14, 2, 1, 0)
    }
    fn load_timer_b(&self) -> u32 {
        self.byte(0x14, 1, 1, 0)
    }
    fn load_timer_a(&self) -> u32 {
        self.byte(0x14, 0, 1, 0)
    }
    fn noise_enable(&self) -> u32 {
        self.byte(0x0f, 7, 1, 0)
    }

    fn ch_output_any(&self, choffs: u32) -> u32 {
        self.byte(0x20, 6, 2, choffs)
    }
    fn ch_output_0(&self, choffs: u32) -> u32 {
        self.byte(0x20, 6, 1, choffs)
    }
    fn ch_output_1(&self, choffs: u32) -> u32 {
        self.byte(0x20, 7, 1, choffs)
    }
    fn ch_output_2(&self, _choffs: u32) -> u32 {
        0
    }
    fn ch_output_3(&self, _choffs: u32) -> u32 {
        0
    }
    fn ch_feedback(&self, choffs: u32) -> u32 {
        self.byte(0x20, 3, 3, choffs)
    }
    fn ch_algorithm(&self, choffs: u32) -> u32 {
        self.byte(0x20, 0, 3, choffs)
    }

    fn op_lfo_am_enable(&self, opoffs: u32) -> u32 {
        self.byte(0xa0, 7, 1, opoffs)
    }
}

//*********************************************************
//  OPM IMPLEMENTATION CLASSES
//*********************************************************

/// The chip variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpmVariant {
    Ym2151,
    Ym2164,
}

/// The Yamaha YM2151 (OPM): eight 4-operator channels with noise, an LFO of
/// four waveforms, two timers, and a stereo pair of outputs through a
/// floating-point DAC.
#[derive(Clone)]
pub struct Ym2151<I: Interface = NullInterface> {
    /// The chip variant.
    #[allow(dead_code)] // ymfm records it but nothing reads it yet
    variant: OpmVariant,
    /// The address register.
    address: u8,
    /// The core FM engine.
    fm: FmEngine<OpmRegisters, I>,
}

impl<I: Interface> Ym2151<I> {
    /// The number of outputs: left and right.
    pub const OUTPUTS: usize = 2;

    const ALL_CHANNELS: u32 = FmEngine::<OpmRegisters, I>::ALL_CHANNELS;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        Self::with_variant(intf, OpmVariant::Ym2151)
    }

    fn with_variant(intf: I, variant: OpmVariant) -> Self {
        let mut chip = Ym2151 {
            variant,
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
        let mut result = self.fm.status();
        if self.fm.intf_mut().is_busy() {
            result |= FmEngine::<OpmRegisters, I>::STATUS_BUSY;
        }
        result
    }

    /// Reads from the chip: the status at offset 1.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 1 {
            0 => 0xff,               // data port (unused)
            _ => self.read_status(), // status port, YM2203 compatible
        }
    }

    /// Writes the address register.
    pub fn write_address(&mut self, data: u8) {
        // just set the address
        self.address = data;
    }

    /// Writes the data register.
    pub fn write_data(&mut self, data: u8) {
        // write the FM register
        self.fm.write(u16::from(self.address), data);

        // special cases
        if self.address == 0x1b {
            // writes to register 0x1B send the upper 2 bits to the output
            // lines
            self.fm
                .intf_mut()
                .external_write(AccessClass::Io, 0, data >> 6);
        }

        // mark busy for a bit
        let busy = 32 * self.fm.clock_prescale();
        self.fm.intf_mut().set_busy_end(busy);
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
            self.fm.clock(Self::ALL_CHANNELS);

            // update the FM content; OPM is full 14-bit with no intermediate
            // clipping
            self.fm
                .output(&mut out.clear().data, 0, 32767, Self::ALL_CHANNELS);

            // YM2151 uses an external DAC (YM3012) with mantissa/exponent
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

    /// Returns the register file as last written, for a diagnostic. The PM
    /// depth, which shares 0x19 with the AM depth, is kept at 0x1A.
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}

//*********************************************************
//  OPP IMPLEMENTATION CLASSES
//*********************************************************

/// The Yamaha YM2164 (OPP). ymfm describes it as almost 100% functionally
/// identical to the YM2151, except that it apparently has some mystery
/// registers in the 00-07 range, and timer B's frequency is half that of the
/// 2151; ymfm models neither difference, and nor does this.
#[derive(Clone)]
pub struct Ym2164<I: Interface = NullInterface>(Ym2151<I>);

impl<I: Interface> Ym2164<I> {
    /// The number of outputs: left and right.
    pub const OUTPUTS: usize = 2;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        Ym2164(Ym2151::with_variant(intf, OpmVariant::Ym2164))
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

    /// Reads from the chip: the status at offset 1.
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

    /// Writes to the chip: the address port at offset 0, the data port at 1.
    pub fn write(&mut self, offset: u32, data: u8) {
        self.0.write(offset, data);
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<2>]) {
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

    /// Returns the register file as last written, for a diagnostic.
    pub fn registers(&self) -> &[u8] {
        self.0.registers()
    }
}
