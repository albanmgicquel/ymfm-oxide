// This file is a Rust translation of ymfm's `ymfm_opz.h` and `ymfm_opz.cpp`
// at commit 81aec25 (https://github.com/aaronsgiles/ymfm), with changes: the
// eight waveform tables are computed once at compile time and shared rather
// than held per chip; a register write returns its key-on target rather than
// writing through output parameters; key-on logging, the temporary debug
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

//! The OPZ: the YM2414.
//!
//! ymfm describes the chip as basically an OPM with a set of extra features,
//! pieced together from an unofficial register description and the TX81Z
//! operator manual. Over the OPM it adds a second LFO, with its own rate,
//! depths, waveform and sensitivities, whose effects are added to the
//! first's; an LFO sync option, which resets an LFO on each key-on; eight
//! operator waveforms; a "reverb" envelope stage, a slower decay that takes
//! over from the release at -18dB; a 2-bit envelope shift, which reduces
//! the envelope's effect; a fine frequency control, which makes the multiple
//! an x.4 value; a fixed-frequency mode per operator; and preset registers,
//! which save and restore the sustain level, release rate, envelope shift
//! and reverb rate. Keys are turned on and off through a bit of each
//! channel's 0x20 register, for the channel register 0x08 selects.
//!
//! The emulation is ymfm's best guess at an undocumented chip, and this
//! module reproduces that guess as it is: the channel volume is stored and
//! unused, the panning is a guess, and the preset logic follows the writes
//! a TX81Z makes rather than any known hardware rule.
//!
//! The register map, as ymfm documents it:
//!
//! ```text
//! System-wide registers:
//!      08 -----xxx Load preset (not sure how it gets saved)
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
//!      16 xxxxxxxx LFO #2 frequency
//!      17 0xxxxxxx AM LFO #2 depth
//!         1xxxxxxx PM LFO #2 depth
//!      18 xxxxxxxx LFO frequency
//!      19 0xxxxxxx AM LFO depth
//!         1xxxxxxx PM LFO depth
//!      1B xx------ CT (2 output data lines)
//!         --x----- LFO #2 sync
//!         ---x---- LFO sync
//!         ----xx-- LFO #2 waveform
//!         ------xx LFO waveform
//!
//! Per-channel registers (channel in address bits 0-2)
//!   00-07 xxxxxxxx Channel volume
//!   20-27 x------- Pan right
//!         -x------ Key on (0)/off(1)
//!         --xxx--- Feedback level for operator 1 (0-7)
//!         -----xxx Operator connection algorithm (0-7)
//!   28-2F -xxxxxxx Key code
//!   30-37 xxxxxx-- Key fraction
//!         -------x Mono? mode
//!   38-3F 0xxx---- LFO PM sensitivity
//!         -----0xx LFO AM shift
//!         1xxx---- LFO #2 PM sensitivity
//!         -----1xx LFO #2 AM shift
//!
//! Per-operator registers (channel in address bits 0-2, operator in bits 3-4)
//!   40-5F 0xxx---- Detune value (0-7)
//!         0---xxxx Multiple value (0-15)
//!         0xxx---- Fix range (0-15)
//!         0---xxxx Fix frequency (0-15)
//!         1xxx---- Oscillator waveform (0-7)
//!         1---xxxx Fine? (0-15)
//!   60-7F -xxxxxxx Total level (0-127)
//!   80-9F xx------ Key scale rate (0-3)
//!         --x----- Fix frequency mode
//!         ---xxxxx Attack rate (0-31)
//!   A0-BF x------- LFO AM enable
//!         ---xxxxx Decay rate (0-31)
//!   C0-DF xx0----- Detune 2 value (0-3)
//!         --0xxxxx Sustain rate (0-31)
//!         xx1----- Envelope generator shift? (0-3)
//!         --1--xxx Rev? (0-7)
//!   E0-FF xxxx---- Sustain level (0-15)
//!         ----xxxx Release rate (0-15)
//!
//! Internal (fake) registers:
//!  100-11F -xxx---- Oscillator waveform (0-7)
//!          ----xxxx Fine? (0-15)
//!  120-13F xx------ Envelope generator shift (0-3)
//!          -----xxx Reverb rate (0-7)
//!  140-15F xxxx---- Preset sustain level (0-15)
//!          ----xxxx Preset release rate (0-15)
//!  160-17F xx------ Envelope generator shift (0-3)
//!          -----xxx Reverb rate (0-7)
//!  180-187 -xxx---- LFO #2 PM sensitivity
//!          ---- xxx LFO #2 AM shift
//!      188 -xxxxxxx LFO #2 PM depth
//!      189 -xxxxxxx LFO PM depth
//! ```
//!
//! The map gives 0x16 as the register redirected to 0x188, but ymfm's code
//! redirects 0x17, where the LFO #2 depths are; this module does as the
//! code does.

use crate::fm::{
    abs_sin_attenuation, detune_adjustment, effective_rate, operator_list,
    opm_key_code_to_phase_step, FmEngine, FmRegisters, OpdataCache, MAX_CHANNELS, WAVEFORM_LENGTH,
};
use crate::ymfm::{
    bitfield, AccessClass, Interface, NullInterface, Output, EG_ATTACK, EG_DECAY, EG_RELEASE,
    EG_REVERB, EG_SUSTAIN,
};

//*********************************************************
//  OPZ REGISTERS
//*********************************************************

/// LFO waveforms are 256 entries long.
const LFO_WAVEFORM_LENGTH: usize = 256;

/// The number of operator waveforms.
const WAVEFORMS: usize = 8;

/// The OPZ's eight waveforms; ymfm builds them in the register set's
/// constructor.
static OPZ_WAVEFORMS: [[u16; WAVEFORM_LENGTH]; WAVEFORMS] = {
    let mut table = [[0u16; WAVEFORM_LENGTH]; WAVEFORMS];

    // create the waveforms
    let mut index = 0;
    while index < WAVEFORM_LENGTH {
        let i = index as u32;
        table[0][index] = (abs_sin_attenuation(i) | (bitfield(i, 9, 1) << 15)) as u16;
        index += 1;
    }

    // we only have the diagrams to judge from, but suspecting waveform 1 (and
    // derived waveforms) are sin^2, based on OPX description of similar
    // waveforms; since our sin table is logarithmic, this ends up just being
    // 2*existing value
    let zeroval = table[0][0];
    let mut index = 0;
    while index < WAVEFORM_LENGTH {
        let doubled = 2 * (table[0][index] & 0x7fff);
        let min = if doubled < zeroval { doubled } else { zeroval };
        table[1][index] = min | ((bitfield(index as u32, 9, 1) as u16) << 15);
        index += 1;
    }

    // remaining waveforms are just derivations of the 2 main ones
    let mut index = 0;
    while index < WAVEFORM_LENGTH {
        if bitfield(index as u32, 9, 1) != 0 {
            table[2][index] = zeroval;
            table[3][index] = zeroval;
            table[4][index] = zeroval;
            table[5][index] = zeroval;
            table[6][index] = zeroval;
            table[7][index] = zeroval;
        } else {
            table[2][index] = table[0][index];
            table[3][index] = table[1][index];
            table[4][index] = table[0][index * 2];
            table[5][index] = table[1][index * 2];
            table[6][index] = table[0][(index * 2) & 0x1ff];
            table[7][index] = table[1][(index * 2) & 0x1ff];
        }
        index += 1;
    }
    table
};

/// The OPZ register set, with its two LFOs, its noise generator, and the
/// fixed-frequency phase substeps.
#[derive(Clone)]
pub(crate) struct OpzRegisters {
    /// The LFO counters.
    lfo_counter: [u32; 2],
    /// The noise LFSR state.
    noise_lfsr: u32,
    /// The noise counter.
    noise_counter: u8,
    /// The latched noise state.
    noise_state: u8,
    /// The latched LFO noise value.
    #[allow(dead_code)] // ymfm saves it but never reads it
    noise_lfo: u8,
    /// The current LFO AM values.
    lfo_am: [u8; 2],
    /// The register data.
    regdata: [u8; OpzRegisters::REGISTERS],
    /// The phase substep of each operator, for fixed frequency.
    phase_substep: [u16; OpzRegisters::OPERATORS],
    /// The LFO waveforms: AM in the low 8 bits, PM in the upper 8.
    lfo_waveform: [[i16; LFO_WAVEFORM_LENGTH]; 4],
}

impl OpzRegisters {
    /// The number of 8-bit registers, the internal ones included.
    const REGISTERS: usize = 0x190;

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
    fn noise_frequency(&self) -> u32 {
        self.byte(0x0f, 0, 5, 0)
    }
    fn lfo2_pm_depth(&self) -> u32 {
        self.byte(0x188, 0, 7, 0) // fake
    }
    fn lfo2_rate(&self) -> u32 {
        self.byte(0x16, 0, 8, 0)
    }
    fn lfo2_am_depth(&self) -> u32 {
        self.byte(0x17, 0, 7, 0)
    }
    fn lfo_rate(&self) -> u32 {
        self.byte(0x18, 0, 8, 0)
    }
    fn lfo_am_depth(&self) -> u32 {
        self.byte(0x19, 0, 7, 0)
    }
    fn lfo_pm_depth(&self) -> u32 {
        self.byte(0x189, 0, 7, 0) // fake
    }
    #[allow(dead_code)]
    fn output_bits(&self) -> u32 {
        self.byte(0x1b, 6, 2, 0)
    }
    fn lfo2_sync(&self) -> u32 {
        self.byte(0x1b, 5, 1, 0)
    }
    fn lfo_sync(&self) -> u32 {
        self.byte(0x1b, 4, 1, 0)
    }
    fn lfo2_waveform(&self) -> u32 {
        self.byte(0x1b, 2, 2, 0)
    }
    fn lfo_waveform(&self) -> u32 {
        self.byte(0x1b, 0, 2, 0)
    }

    // per-channel registers
    #[allow(dead_code)] // ymfm reads the volume nowhere
    fn ch_volume(&self, choffs: u32) -> u32 {
        self.byte(0x00, 0, 8, choffs)
    }
    fn ch_key_on(&self, choffs: u32) -> u32 {
        self.byte(0x20, 6, 1, choffs)
    }
    fn ch_block_freq(&self, choffs: u32) -> u32 {
        self.word(0x28, 0, 7, 0x30, 2, 6, choffs)
    }
    fn ch_lfo_pm_sens(&self, choffs: u32) -> u32 {
        self.byte(0x38, 4, 3, choffs)
    }
    fn ch_lfo_am_sens(&self, choffs: u32) -> u32 {
        self.byte(0x38, 0, 2, choffs)
    }
    fn ch_lfo2_pm_sens(&self, choffs: u32) -> u32 {
        self.byte(0x180, 4, 3, choffs) // fake
    }
    fn ch_lfo2_am_sens(&self, choffs: u32) -> u32 {
        self.byte(0x180, 0, 2, choffs) // fake
    }

    // per-operator registers
    fn op_detune(&self, opoffs: u32) -> u32 {
        self.byte(0x40, 4, 3, opoffs)
    }
    fn op_multiple(&self, opoffs: u32) -> u32 {
        self.byte(0x40, 0, 4, opoffs)
    }
    fn op_fix_range(&self, opoffs: u32) -> u32 {
        self.byte(0x40, 4, 3, opoffs)
    }
    fn op_fix_frequency(&self, opoffs: u32) -> u32 {
        self.byte(0x40, 0, 4, opoffs)
    }
    fn op_waveform(&self, opoffs: u32) -> u32 {
        self.byte(0x100, 4, 3, opoffs) // fake
    }
    fn op_fine(&self, opoffs: u32) -> u32 {
        self.byte(0x100, 0, 4, opoffs) // fake
    }
    fn op_total_level(&self, opoffs: u32) -> u32 {
        self.byte(0x60, 0, 7, opoffs)
    }
    fn op_ksr(&self, opoffs: u32) -> u32 {
        self.byte(0x80, 6, 2, opoffs)
    }
    fn op_fix_mode(&self, opoffs: u32) -> u32 {
        self.byte(0x80, 5, 1, opoffs)
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
    fn op_eg_shift(&self, opoffs: u32) -> u32 {
        self.byte(0x120, 6, 2, opoffs) // fake
    }
    fn op_reverb_rate(&self, opoffs: u32) -> u32 {
        self.byte(0x120, 0, 3, opoffs) // fake
    }
    fn op_sustain_level(&self, opoffs: u32) -> u32 {
        self.byte(0xe0, 4, 4, opoffs)
    }
    fn op_release_rate(&self, opoffs: u32) -> u32 {
        self.byte(0xe0, 0, 4, opoffs)
    }
}

impl FmRegisters for OpzRegisters {
    const OUTPUTS: usize = 2;
    const CHANNELS: usize = 8;
    const ALL_CHANNELS: u32 = (1 << Self::CHANNELS) - 1;
    const OPERATORS: usize = Self::CHANNELS * 4;
    const DEFAULT_PRESCALE: u32 = 2;
    const EG_CLOCK_DIVIDER: u32 = 3;
    const EG_HAS_REVERB: bool = true;
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
        OpzRegisters {
            lfo_counter: [0, 0],
            noise_lfsr: 1,
            noise_counter: 0,
            noise_state: 0,
            noise_lfo: 0,
            lfo_am: [0, 0],
            regdata: [0; Self::REGISTERS],
            phase_substep: [0; Self::OPERATORS],
            lfo_waveform,
        }
    }

    fn reset(&mut self) {
        self.regdata = [0; Self::REGISTERS];
        self.phase_substep = [0; Self::OPERATORS];

        // enable output on both channels by default
        self.regdata[0x30..0x38].fill(0x01);
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
        let bit7 = bitfield(u32::from(data), 7, 1) != 0;
        let bit5 = bitfield(u32::from(data), 5, 1) != 0;

        // special mappings:
        //   0x17 -> 0x188 if bit 7 is set
        //   0x19 -> 0x189 if bit 7 is set
        //   0x38..0x3F -> 0x180..0x187 if bit 7 is set
        //   0x40..0x5F -> 0x100..0x11F if bit 7 is set
        //   0xC0..0xDF -> 0x120..0x13F if bit 5 is set
        if index == 0x17 && bit7 {
            self.regdata[0x188] = data;
        } else if index == 0x19 && bit7 {
            self.regdata[0x189] = data;
        } else if (index & 0xf8) == 0x38 && bit7 {
            self.regdata[0x180 + (index & 7)] = data;
        } else if (index & 0xe0) == 0x40 && bit7 {
            self.regdata[0x100 + (index & 0x1f)] = data;
        } else if (index & 0xe0) == 0xc0 && bit5 {
            self.regdata[0x120 + (index & 0x1f)] = data;
        } else if index < 0x100 {
            self.regdata[index] = data;
        }

        // preset writes restore some values from a preset memory; not sure
        // how this really works but the TX81Z will overwrite the sustain
        // level/release rate register and the envelope shift/reverb rate
        // register to dampen sound, then write the preset number to register
        // 8 to restore them
        if index == 0x08 {
            let chan = bitfield(u32::from(data), 0, 3) as usize;
            for op in [0, 8, 16, 24] {
                self.regdata[0xe0 + chan + op] = self.regdata[0x140 + chan + op];
            }
            for op in [0, 8, 16, 24] {
                self.regdata[0x120 + chan + op] = self.regdata[0x160 + chan + op];
            }
        }

        // store the presets under some unknown condition; the pattern of
        // writes when setting a new preset is:
        //
        //   08 (0-7), 80-9F, A0-BF, C0-DF, C0-DF (alt), 20-27, 40-5F,
        //   40-5F (alt), C0-DF (alt -- again?), 38-3F, 1B, 18, E0-FF
        //
        // So it writes 0-7 to 08 to either reset all presets or to indicate
        // that we're going to be loading them. Immediately after all the
        // writes above, the very next write will be temporary values to blow
        // away the values loaded into E0-FF, so somehow it also knows that
        // anything after that point is not part of the preset.
        //
        // For now, ymfm tries using the 40-5F (alt) writes as flags that
        // presets are being loaded until the E0-FF writes happen; the flag
        // is read for every register, as ymfm reads it.
        let is_setting_preset =
            bitfield(u32::from(self.regdata[0x100 + (index & 0x1f)]), 7, 1) != 0;
        if is_setting_preset {
            if (index & 0xe0) == 0xe0 {
                self.regdata[0x140 + (index & 0x1f)] = data;
                self.regdata[0x100 + (index & 0x1f)] &= 0x7f;
            } else if (index & 0xe0) == 0xc0 && bit5 {
                self.regdata[0x160 + (index & 0x1f)] = data;
            }
        }

        // handle writes to the key on index
        if (index & 0xf8) == 0x20
            && bitfield(index as u32, 0, 3) == bitfield(u32::from(self.regdata[0x08]), 0, 3)
        {
            let channel = bitfield(index as u32, 0, 3);
            let opmask = if self.ch_key_on(channel) != 0 { 0xf } else { 0 };

            // according to the TX81Z manual, the sync option causes the LFOs
            // to reset at each note on
            if opmask != 0 {
                if self.lfo_sync() != 0 {
                    self.lfo_counter[0] = 0;
                }
                if self.lfo2_sync() != 0 {
                    self.lfo_counter[1] = 0;
                }
            }
            return Some((channel, opmask));
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
        let rate0 = self.lfo_rate();
        let rate1 = self.lfo2_rate();
        self.lfo_counter[0] = self.lfo_counter[0]
            .wrapping_add((0x10 | bitfield(rate0, 0, 4)) << bitfield(rate0, 4, 4));
        self.lfo_counter[1] = self.lfo_counter[1]
            .wrapping_add((0x10 | bitfield(rate1, 0, 4)) << bitfield(rate1, 4, 4));
        let lfo0 = bitfield(self.lfo_counter[0], 22, 8);
        let lfo1 = bitfield(self.lfo_counter[1], 22, 8);

        // fill in the noise entry 1 ahead of our current position; this
        // ensures the current value remains stable for a full LFO clock and
        // effectively latches the running value when the LFO advances
        let lfo_noise = bitfield(self.noise_lfsr, 17, 8);
        let noise = (lfo_noise | (lfo_noise << 8)) as i16;
        self.lfo_waveform[3][((lfo0 + 1) & 0xff) as usize] = noise;
        self.lfo_waveform[3][((lfo1 + 1) & 0xff) as usize] = noise;

        // fetch the AM/PM values based on the waveform; AM is unsigned and
        // encoded in the low 8 bits, while PM signed and encoded in the upper
        // 8 bits
        let ampm0 = i32::from(self.lfo_waveform[self.lfo_waveform() as usize][lfo0 as usize]);
        let ampm1 = i32::from(self.lfo_waveform[self.lfo2_waveform() as usize][lfo1 as usize]);

        // apply depth to the AM values and store for later
        self.lfo_am[0] = (((ampm0 & 0xff) as u32 * self.lfo_am_depth()) >> 7) as u8;
        self.lfo_am[1] = (((ampm1 & 0xff) as u32 * self.lfo2_am_depth()) >> 7) as u8;

        // apply depth to the PM values and return them combined into two
        // bytes, LFO 1 in the low and LFO 2 above it
        let pm0 = ((ampm0 >> 8) * self.lfo_pm_depth() as i32) >> 7;
        let pm1 = ((ampm1 >> 8) * self.lfo2_pm_depth() as i32) >> 7;
        (pm0 & 0xff) | (pm1 << 8)
    }

    fn lfo_am_offset(&self, choffs: u32) -> u32 {
        // not sure how this works for real, but just adding the two AM LFOs
        // together
        let mut result = 0;

        // shift value for AM sensitivity is [*, 0, 1, 2], mapping to values
        // of [0, 23.9, 47.8, and 95.6dB]
        let am_sensitivity = self.ch_lfo_am_sens(choffs);
        if am_sensitivity != 0 {
            result = u32::from(self.lfo_am[0]) << (am_sensitivity - 1);
        }

        // raw LFO AM value on OPZ is 0-FF, which is already a factor of 2
        // larger than the OPN, putting our staring point at 2x theirs; this
        // works out since our minimum is 2x their maximum
        let am_sensitivity2 = self.ch_lfo2_am_sens(choffs);
        if am_sensitivity2 != 0 {
            result += u32::from(self.lfo_am[1]) << (am_sensitivity2 - 1);
        }
        result
    }

    fn noise_state(&self) -> u32 {
        u32::from(self.noise_state)
    }

    fn cache_operator_data(&mut self, choffs: u32, opoffs: u32, cache: &mut OpdataCache) {
        // set up the easy stuff
        cache.waveform = self.op_waveform(opoffs) as u8;

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

        // multiple value, as an x.4 value (0 means 0.5); the "fine" control
        // provides the fractional bits
        cache.multiple = self.op_multiple(opoffs) << 4;
        if cache.multiple == 0 {
            cache.multiple = 0x08;
        }
        cache.multiple |= self.op_fine(opoffs);

        // phase step, or PHASE_STEP_DYNAMIC if PM is active; this depends on
        // block_freq, detune, and multiple, so compute it after we've done
        // those; note that fix frequency mode is also treated as dynamic
        cache.phase_step = if self.op_fix_mode(opoffs) == 0
            && (self.lfo_pm_depth() == 0 || self.ch_lfo_pm_sens(choffs) == 0)
            && (self.lfo2_pm_depth() == 0 || self.ch_lfo2_pm_sens(choffs) == 0)
        {
            self.compute_phase_step(choffs, opoffs, cache, 0)
        } else {
            OpdataCache::PHASE_STEP_DYNAMIC
        };

        // total level, scaled by 8; ymfm leaves the channel volume out
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
        cache.eg_rate[EG_REVERB] = cache.eg_rate[EG_RELEASE];
        let reverb = self.op_reverb_rate(opoffs);
        if reverb != 0 {
            cache.eg_rate[EG_REVERB] =
                effective_rate(reverb * 4 + 2, ksrval).min(cache.eg_rate[EG_REVERB]);
        }

        // set the envelope shift; TX81Z manual says operator 1 shift is fixed
        // at "off"
        cache.eg_shift = if (opoffs & 0x18) == 0 {
            0
        } else {
            self.op_eg_shift(opoffs) as u8
        };
    }

    fn compute_phase_step(
        &mut self,
        choffs: u32,
        opoffs: u32,
        cache: &OpdataCache,
        lfo_raw_pm: i32,
    ) -> u32 {
        // OPZ has a fixed frequency mode; it is unclear whether the detune
        // and multiple parameters affect things
        if self.op_fix_mode(opoffs) != 0 {
            // the baseline frequency in hz comes from the fix frequency and
            // fine registers, which can specify values 8-255Hz in 1Hz
            // increments; that value is then shifted up by the 3-bit range
            let mut freq = self.op_fix_frequency(opoffs) << 4;
            if freq == 0 {
                freq = 8;
            }
            freq |= self.op_fine(opoffs);
            freq <<= self.op_fix_range(opoffs);

            // there is not enough resolution in the plain phase step to track
            // the full range of frequencies, so we keep a per-operator sub
            // step with an additional 12 bits of resolution; this calculation
            // gives us, for example, a frequency of 8.0009Hz when 8Hz is
            // requested
            let mut substep = u32::from(self.phase_substep[opoffs as usize]);
            substep += 75 * freq;
            let phase_step = substep >> 12;
            self.phase_substep[opoffs as usize] = (substep & 0xfff) as u16;

            // detune/multiple occupy the same space as fix_range/
            // fix_frequency so don't apply them in addition
            return phase_step;
        }

        // start with coarse detune delta; table uses cents value from manual,
        // converted into 1/64ths
        const DETUNE2_DELTA: [i16; 4] = [
            0,
            ((600 * 64 + 50) / 100) as i16,
            ((781 * 64 + 50) / 100) as i16,
            ((950 * 64 + 50) / 100) as i16,
        ];
        let mut delta = i32::from(DETUNE2_DELTA[self.op_detune2(opoffs) as usize]);

        // add in the PM deltas; each LFO's raw PM value is a byte of
        // `lfo_raw_pm`, read back as an int8_t
        //
        // raw PM value is -127..128 which is +/- 200 cents; manual gives
        // these magnitudes in cents:
        //    0, +/-5, +/-10, +/-20, +/-50, +/-100, +/-400, +/-700
        // this roughly corresponds to shifting the 200-cent value:
        //    0  >> 5,  >> 4,  >> 3,  >> 2,  >> 1,   << 1,   << 2
        let pm_sensitivity = self.ch_lfo_pm_sens(choffs);
        if pm_sensitivity != 0 {
            let pm = i32::from(lfo_raw_pm as i8);
            if pm_sensitivity < 6 {
                delta += pm >> (6 - pm_sensitivity);
            } else {
                delta += pm << (pm_sensitivity - 5);
            }
        }
        let pm_sensitivity2 = self.ch_lfo2_pm_sens(choffs);
        if pm_sensitivity2 != 0 {
            let pm = i32::from((lfo_raw_pm >> 8) as i8);
            if pm_sensitivity2 < 6 {
                delta += pm >> (6 - pm_sensitivity2);
            } else {
                delta += pm << (pm_sensitivity2 - 5);
            }
        }

        // apply delta and convert to a frequency number; this translation is
        // the same as OPM so just re-use that helper
        let mut phase_step = opm_key_code_to_phase_step(cache.block_freq, delta);

        // apply detune based on the keycode
        phase_step = phase_step.wrapping_add(cache.detune as u32);

        // apply frequency multiplier (which is cached as an x.4 value)
        phase_step.wrapping_mul(cache.multiple) >> 4
    }

    fn waveform(&self, index: u8) -> &[u16; WAVEFORM_LENGTH] {
        &OPZ_WAVEFORMS[index as usize]
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
        self.byte(0x20, 7, 1, choffs) | self.byte(0x30, 0, 1, choffs)
    }
    fn ch_output_0(&self, choffs: u32) -> u32 {
        self.byte(0x30, 0, 1, choffs)
    }
    fn ch_output_1(&self, choffs: u32) -> u32 {
        self.byte(0x20, 7, 1, choffs) | self.byte(0x30, 0, 1, choffs)
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
//  IMPLEMENTATION CLASSES
//*********************************************************

/// The Yamaha YM2414 (OPZ): eight 4-operator channels with noise, two LFOs,
/// eight operator waveforms, a reverb envelope stage, fixed-frequency
/// operators, two timers, and a stereo pair of outputs, which ymfm assumes
/// go through a YM2151-like floating-point DAC.
#[derive(Clone)]
pub struct Ym2414<I: Interface = NullInterface> {
    /// The address register.
    address: u8,
    /// The core FM engine.
    fm: FmEngine<OpzRegisters, I>,
}

impl<I: Interface> Ym2414<I> {
    /// The number of outputs: left and right.
    pub const OUTPUTS: usize = 2;

    const ALL_CHANNELS: u32 = FmEngine::<OpzRegisters, I>::ALL_CHANNELS;

    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        let mut chip = Ym2414 {
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
            result |= FmEngine::<OpzRegisters, I>::STATUS_BUSY;
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

            // update the FM content; YM2414 is full 14-bit with no
            // intermediate clipping
            self.fm
                .output(&mut out.clear().data, 0, 32767, Self::ALL_CHANNELS);

            // unsure about YM2414 outputs; assume it is like YM2151
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

    /// Returns the register file, 0x190 bytes: the 256 registers as last
    /// written, then the internal ones the alternate writes, the presets and
    /// the second LFO's settings are kept in (see the module's map).
    pub fn registers(&self) -> &[u8] {
        self.fm.regs().data()
    }
}
