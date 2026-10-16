// This file is a Rust translation of ymfm's `ymfm_pcm.h` and `ymfm_pcm.cpp`
// at commit 81aec25 (https://github.com/aaronsgiles/ymfm), with changes: the
// channels refer to the engine's registers and interface through arguments
// rather than through references they hold, and the engine takes the
// interface as an argument rather than owning it, the chip owning it; the
// active-channel mask, which ymfm computes and never reads, is gone, as are
// the channel-isolation debug mask, key-on logging and save states; and a new
// engine's registers start at zero and its prepare counter at zero, where
// ymfm leaves both undefined until `reset()` and the first clock. Every
// computation that reaches a sample, a register or the interface is kept as
// ymfm writes it, including its integer widths.
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

//! The PCM wavetable engine of the YMF278B (OPL4): 24 channels that play 8-,
//! 12- or 16-bit samples from external memory, each with its own envelope,
//! LFO, level and panning. The chip, [`Ymf278b`](crate::opl::Ymf278b), lives
//! in [`opl`](crate::opl) beside its FM side; this module is the engine it
//! drives.
//!
//! The register map, as ymfm documents it:
//!
//! ```text
//! System-wide registers:
//!   00-01 xxxxxxxx LSI Test
//!      02 -------x Memory access mode (0=sound gen, 1=read/write)
//!         ------x- Memory type (0=ROM, 1=ROM+SRAM)
//!         ---xxx-- Wave table header
//!         xxx----- Device ID (=1 for YMF278B)
//!      03 --xxxxxx Memory address high
//!      04 xxxxxxxx Memory address mid
//!      05 xxxxxxxx Memory address low
//!      06 xxxxxxxx Memory data
//!      F8 --xxx--- Mix control (FM_R)
//!         -----xxx Mix control (FM_L)
//!      F9 --xxx--- Mix control (PCM_R)
//!         -----xxx Mix control (PCM_L)
//!
//! Channel-specific registers:
//!   08-1F xxxxxxxx Wave table number low
//!   20-37 -------x Wave table number high
//!         xxxxxxx- F-number low
//!   38-4F -----xxx F-number high
//!         ----x--- Pseudo-reverb
//!         xxxx---- Octave
//!   50-67 xxxxxxx- Total level
//!         -------x Level direct
//!   68-7F x------- Key on
//!         -x------ Damp
//!         --x----- LFO reset
//!         ---x---- Output channel
//!         ----xxxx Panpot
//!   80-97 --xxx--- LFO speed
//!         -----xxx Vibrato
//!   98-AF xxxx---- Attack rate
//!         ----xxxx Decay rate
//!   B0-C7 xxxx---- Sustain level
//!         ----xxxx Sustain rate
//!   C8-DF xxxx---- Rate correction
//!         ----xxxx Release rate
//!   E0-F7 -----xxx AM depth
//! ```
//!
//! A wave table header in memory is 12 bytes, at 12 times the wave table
//! number (from 384 on, in the bank the wave table header field names, if it
//! is not zero):
//!
//! ```text
//! +00: format (2 bits: 0 = 8-bit, 1 = 12-bit, 2 = 16-bit) and start high
//! +01: start mid
//! +02: start low
//! +03: loop hi
//! +04: loop low
//! +05: -end hi
//! +06: -end low
//! +07: vibrato (reg 80)
//! +08: attack/decay (reg 98)
//! +09: sustain level/rate (reg B0)
//! +0A: ksr/release (reg C8)
//! +0B: LFO amplitude (reg E0)
//! ```

use crate::fm::{attenuation_increment, attenuation_to_volume};
use crate::ymfm::{
    bitfield, clamp, AccessClass, Interface, EG_ATTACK, EG_DECAY, EG_RELEASE, EG_REVERB, EG_STATES,
    EG_SUSTAIN,
};

//*********************************************************
//  PCM CACHE
//*********************************************************

/// The data computed once at the start of clocking, which remains static
/// during subsequent sound generation.
#[derive(Clone, Copy, Debug, Default)]
struct PcmCache {
    /// The sample position step, as a .16 value.
    step: u32,
    /// The target total level, as a .10 value.
    total_level: u32,
    /// The left panning attenuation.
    pan_left: u32,
    /// The right panning attenuation.
    pan_right: u32,
    /// The sustain level, shifted up to envelope values.
    eg_sustain: u32,
    /// The envelope rates, including KSR.
    eg_rate: [u8; EG_STATES],
    /// The stepping value for the LFO.
    lfo_step: u8,
    /// The scale value for the AM LFO.
    am_depth: u8,
    /// The scale value for the PM LFO.
    pm_depth: u8,
}

//*********************************************************
//  PCM REGISTERS
//*********************************************************

/// The register set of the PCM engine.
#[derive(Clone)]
pub(crate) struct PcmRegisters {
    /// The register data.
    regdata: [u8; Self::REGISTERS],
}

impl PcmRegisters {
    /// The number of outputs.
    pub(crate) const OUTPUTS: usize = 4;
    /// The number of channels.
    pub(crate) const CHANNELS: usize = 24;
    /// The number of 8-bit registers.
    pub(crate) const REGISTERS: usize = 0x100;
    /// The mask of every channel.
    pub(crate) const ALL_CHANNELS: u32 = (1 << Self::CHANNELS) - 1;

    /// Creates a register set; ymfm leaves it undefined until `reset`.
    fn new() -> Self {
        PcmRegisters {
            regdata: [0; Self::REGISTERS],
        }
    }

    /// Resets to the initial state.
    fn reset(&mut self) {
        self.regdata = [0; Self::REGISTERS];
        self.regdata[0xf8] = 0x1b;
    }

    /// Updates the cache with data from the registers.
    fn cache_channel_data(&self, choffs: u32, cache: &mut PcmCache) {
        // compute step from octave and fnumber; the math here implies a .18
        // fraction but .16 should be perfectly fine
        let octave = i32::from(((self.ch_octave(choffs) << 4) as u8 as i8) >> 4);
        let fnum = self.ch_fnumber(choffs);
        // an octave of -8 makes the shift -1, which C leaves undefined; the
        // hardware ymfm runs on masks a shift count to 5 bits, as
        // `wrapping_shl` does
        cache.step = (0x400 | fnum).wrapping_shl((octave + 7) as u32) >> 2;

        // total level is computed as a .10 value for interpolation
        cache.total_level = self.ch_total_level(choffs) << 10;

        // compute panning values in terms of envelope attenuation
        let panpot = i32::from(((self.ch_panpot(choffs) << 4) as u8 as i8) >> 4);
        if panpot >= 0 {
            cache.pan_left = if panpot == 7 {
                0x3ff
            } else {
                0x20 * panpot as u32
            };
            cache.pan_right = 0;
        } else if panpot >= -7 {
            cache.pan_left = 0;
            cache.pan_right = if panpot == -7 {
                0x3ff
            } else {
                (-0x20 * panpot) as u32
            };
        } else {
            cache.pan_left = 0x3ff;
            cache.pan_right = 0x3ff;
        }

        // determine the LFO stepping value; this how much to add to a
        // running x.18 value for the LFO; steps were derived from frequencies
        // in the manual and come out very close with these values
        const LFO_STEPS: [u8; 8] = [1, 12, 19, 25, 31, 35, 37, 42];
        cache.lfo_step = LFO_STEPS[self.ch_lfo_speed(choffs) as usize];

        // AM LFO depth values, derived from the manual; note each has at most
        // 2 bits to make the "multiply" easy in hardware
        const AM_DEPTH: [u8; 8] = [0, 0x14, 0x20, 0x28, 0x30, 0x40, 0x50, 0x80];
        cache.am_depth = AM_DEPTH[self.ch_am_depth(choffs) as usize];

        // PM LFO depth values; these are converted from the manual's cents
        // values into f-numbers; the computations come out quite cleanly so
        // pretty sure these are correct
        const PM_DEPTH: [u8; 8] = [0, 2, 3, 4, 6, 12, 24, 48];
        cache.pm_depth = PM_DEPTH[self.ch_vibrato(choffs) as usize];

        // 4-bit sustain level, but 15 means 31 so effectively 5 bits
        cache.eg_sustain = self.ch_sustain_level(choffs);
        cache.eg_sustain |= (cache.eg_sustain + 1) & 0x10;
        cache.eg_sustain <<= 5;

        // compute the key scaling correction factor; 15 means don't do any
        // correction
        let mut correction = self.ch_rate_correction(choffs) as i32;
        if correction == 15 {
            correction = 0;
        } else {
            correction = (octave + correction) * 2 + bitfield(fnum, 9, 1) as i32;
        }

        // compute the envelope generator rates
        cache.eg_rate[EG_ATTACK] = Self::effective_rate(self.ch_attack_rate(choffs), correction);
        cache.eg_rate[EG_DECAY] = Self::effective_rate(self.ch_decay_rate(choffs), correction);
        cache.eg_rate[EG_SUSTAIN] = Self::effective_rate(self.ch_sustain_rate(choffs), correction);
        cache.eg_rate[EG_RELEASE] = Self::effective_rate(self.ch_release_rate(choffs), correction);
        cache.eg_rate[EG_REVERB] = 5;

        // if damping is on, override some things; essentially decay at a
        // hardcoded rate of 48 until -12db (0x80), then at maximum rate for
        // the rest
        if self.ch_damp(choffs) != 0 {
            cache.eg_rate[EG_DECAY] = 48;
            cache.eg_rate[EG_SUSTAIN] = 63;
            cache.eg_rate[EG_RELEASE] = 63;
            cache.eg_sustain = 0x80;
        }
    }

    /// Returns the effective rate, clamping and applying corrections as
    /// needed.
    fn effective_rate(raw: u32, correction: i32) -> u8 {
        // raw rates of 0 and 15 just pin to min/max
        if raw == 0 {
            return 0;
        }
        if raw == 15 {
            return 63;
        }

        // otherwise add the correction and clamp to range; ymfm passes the
        // signed correction as a uint32_t and clamps the sum as an int32_t,
        // which is the signed sum
        clamp((raw * 4) as i32 + correction, 0, 63) as u8
    }

    /// Reads a register directly.
    pub(crate) fn read(&self, index: u32) -> u8 {
        self.regdata[index as usize]
    }

    /// Writes a register directly.
    fn write(&mut self, index: u32, data: u8) {
        self.regdata[index as usize] = data;
    }

    /// Returns the register data, for a diagnostic.
    pub(crate) fn data(&self) -> &[u8] {
        &self.regdata
    }

    /// Returns `regdata[index]` as a `u32`.
    fn reg(&self, index: u32) -> u32 {
        u32::from(self.regdata[index as usize])
    }

    // system-wide registers
    fn memory_access_mode(&self) -> u32 {
        bitfield(self.reg(0x02), 0, 1)
    }
    #[allow(dead_code)] // ymfm reads no part of the engine from it
    fn memory_type(&self) -> u32 {
        bitfield(self.reg(0x02), 1, 1)
    }
    fn wave_table_header(&self) -> u32 {
        bitfield(self.reg(0x02), 2, 3)
    }
    #[allow(dead_code)] // ymfm reads no part of the engine from it
    fn device_id(&self) -> u32 {
        bitfield(self.reg(0x02), 5, 3)
    }
    fn memory_address(&self) -> u32 {
        (bitfield(self.reg(0x03), 0, 6) << 16) | (self.reg(0x04) << 8) | self.reg(0x05)
    }
    #[allow(dead_code)] // ymfm reads no part of the engine from it
    fn memory_data(&self) -> u32 {
        self.reg(0x06)
    }
    /// Returns the FM right mix control.
    pub(crate) fn mix_fm_r(&self) -> u32 {
        bitfield(self.reg(0xf8), 3, 3)
    }
    /// Returns the FM left mix control.
    pub(crate) fn mix_fm_l(&self) -> u32 {
        bitfield(self.reg(0xf8), 0, 3)
    }
    /// Returns the PCM right mix control.
    pub(crate) fn mix_pcm_r(&self) -> u32 {
        bitfield(self.reg(0xf9), 3, 3)
    }
    /// Returns the PCM left mix control.
    pub(crate) fn mix_pcm_l(&self) -> u32 {
        bitfield(self.reg(0xf9), 0, 3)
    }

    // per-channel registers
    fn ch_wave_table_num(&self, choffs: u32) -> u32 {
        self.reg(choffs + 0x08) | (bitfield(self.reg(choffs + 0x20), 0, 1) << 8)
    }
    fn ch_fnumber(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x20), 1, 7) | (bitfield(self.reg(choffs + 0x38), 0, 3) << 7)
    }
    fn ch_pseudo_reverb(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x38), 3, 1)
    }
    fn ch_octave(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x38), 4, 4)
    }
    fn ch_total_level(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x50), 1, 7)
    }
    fn ch_level_direct(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x50), 0, 1)
    }
    #[allow(dead_code)] // ymfm reads the key on from the write itself
    fn ch_keyon(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x68), 7, 1)
    }
    fn ch_damp(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x68), 6, 1)
    }
    fn ch_lfo_reset(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x68), 5, 1)
    }
    fn ch_output_channel(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x68), 4, 1)
    }
    fn ch_panpot(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x68), 0, 4)
    }
    fn ch_lfo_speed(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x80), 3, 3)
    }
    fn ch_vibrato(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x80), 0, 3)
    }
    fn ch_attack_rate(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x98), 4, 4)
    }
    fn ch_decay_rate(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x98), 0, 4)
    }
    fn ch_sustain_level(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0xb0), 4, 4)
    }
    fn ch_sustain_rate(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0xb0), 0, 4)
    }
    fn ch_rate_correction(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0xc8), 4, 4)
    }
    fn ch_release_rate(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0xc8), 0, 4)
    }
    fn ch_am_depth(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0xe0), 0, 3)
    }

    /// Returns the memory address and increments it.
    fn memory_address_autoinc(&mut self) -> u32 {
        let result = self.memory_address();
        let newval = result.wrapping_add(1);
        self.regdata[0x05] = newval as u8;
        self.regdata[0x04] = (newval >> 8) as u8;
        self.regdata[0x03] = ((newval >> 16) & 0x3f) as u8;
        result
    }
}

//*********************************************************
//  PCM CHANNEL
//*********************************************************

/// The key is on.
const KEY_ON: u8 = 0x01;
/// A key on is pending.
const KEY_PENDING_ON: u8 = 0x02;
/// A key change is pending.
const KEY_PENDING: u8 = 0x04;

/// The "quiet" value, used to optimize when we can skip doing work.
const EG_QUIET: u32 = 0x200;

/// One PCM channel.
#[derive(Clone, Copy, Debug)]
struct PcmChannel {
    /// The channel offset.
    choffs: u32,
    /// The base address.
    baseaddr: u32,
    /// The ending position.
    endpos: u32,
    /// The loop position.
    looppos: u32,
    /// The current position.
    curpos: u32,
    /// The next position.
    nextpos: u32,
    /// The LFO counter.
    lfo_counter: u32,
    /// The envelope state.
    eg_state: usize,
    /// The computed envelope attenuation.
    env_attenuation: u16,
    /// The total level, as 7.10 for interpolation.
    total_level: u32,
    /// The sample format.
    format: u8,
    /// The current key state.
    key_state: u8,
    /// The cached data.
    cache: PcmCache,
}

impl PcmChannel {
    /// Creates a channel in the state ymfm's constructor leaves it.
    fn new(choffs: u32) -> Self {
        PcmChannel {
            choffs,
            baseaddr: 0,
            endpos: 0,
            looppos: 0,
            curpos: 0,
            nextpos: 0,
            lfo_counter: 0,
            eg_state: EG_RELEASE,
            env_attenuation: 0x3ff,
            total_level: 0x7f << 10,
            format: 0,
            key_state: 0,
            cache: PcmCache::default(),
        }
    }

    /// Resets the channel state.
    fn reset(&mut self) {
        self.baseaddr = 0;
        self.endpos = 0;
        self.looppos = 0;
        self.curpos = 0;
        self.nextpos = 0;
        self.lfo_counter = 0;
        self.eg_state = EG_RELEASE;
        self.env_attenuation = 0x3ff;
        self.total_level = 0x7f << 10;
        self.format = 0;
        self.key_state = 0;
    }

    /// Prepares for clocking, and returns whether the channel is active.
    fn prepare(&mut self, regs: &PcmRegisters) -> bool {
        // cache the data
        regs.cache_channel_data(self.choffs, &mut self.cache);

        // clock the key state
        if (self.key_state & KEY_PENDING) != 0 {
            let oldstate = self.key_state;
            self.key_state = (self.key_state >> 1) & KEY_ON;
            if ((oldstate ^ self.key_state) & KEY_ON) != 0 {
                if (self.key_state & KEY_ON) != 0 {
                    self.start_attack(regs);
                } else {
                    self.start_release();
                }
            }
        }

        // set the total level directly if not interpolating
        if regs.ch_level_direct(self.choffs) != 0 {
            self.total_level = self.cache.total_level;
        }

        // we're active until we're quiet after the release
        self.eg_state < EG_RELEASE || u32::from(self.env_attenuation) < EG_QUIET
    }

    /// Clocks the channel forward one sample.
    fn clock(&mut self, regs: &PcmRegisters, env_counter: u32) {
        // clock the LFO, which is an x.18 value incremented based on the LFO
        // speed value
        self.lfo_counter = self
            .lfo_counter
            .wrapping_add(u32::from(self.cache.lfo_step));

        // clock the envelope
        self.clock_envelope(regs, env_counter);

        // determine the step after applying vibrato
        let mut step = self.cache.step;
        if self.cache.pm_depth != 0 {
            // shift the LFO by 1/4 cycle for PM so that it starts at 0
            let lfo_shifted = self.lfo_counter.wrapping_add(1 << 16);
            let mut lfo_value = bitfield(lfo_shifted, 10, 7) as i32;
            if bitfield(lfo_shifted, 17, 1) != 0 {
                lfo_value ^= 0x7f;
            }
            lfo_value -= 0x40;
            step = step.wrapping_add(((lfo_value * i32::from(self.cache.pm_depth)) >> 7) as u32);
        }

        // advance the sample step and loop as needed
        self.curpos = self.nextpos;
        self.nextpos = self.curpos.wrapping_add(step);
        if self.nextpos >= self.endpos {
            self.nextpos = self
                .nextpos
                .wrapping_add(self.looppos.wrapping_sub(self.endpos));
        }

        // interpolate total level if needed
        if self.total_level != self.cache.total_level {
            // max->min volume takes 156.4ms, or pretty close to 19/1024 per
            // 44.1kHz sample; min->max volume is half that, so advance by
            // 38/1024 per sample
            if self.total_level < self.cache.total_level {
                self.total_level = (self.total_level + 19).min(self.cache.total_level);
            } else {
                self.total_level =
                    (self.total_level as i32 - 38).max(self.cache.total_level as i32) as u32;
            }
        }
    }

    /// Adds the channel's output, with panning applied, to `output`.
    fn output<I: Interface>(&self, regs: &PcmRegisters, output: &mut [i32; 4], intf: &mut I) {
        // early out if the envelope is effectively off
        let mut envelope = u32::from(self.env_attenuation);
        if envelope > EG_QUIET {
            return;
        }

        // add in LFO AM modulation
        if self.cache.am_depth != 0 {
            let mut lfo_value = bitfield(self.lfo_counter, 10, 7);
            if bitfield(self.lfo_counter, 17, 1) != 0 {
                lfo_value ^= 0x7f;
            }
            envelope += (lfo_value * u32::from(self.cache.am_depth)) >> 7;
        }

        // add in the current interpolated total level value, which is a .10
        // value shifted left by 2
        envelope += self.total_level >> 8;

        // add in panning effect and clamp
        let lenv = (envelope + self.cache.pan_left).min(0x3ff);
        let renv = (envelope + self.cache.pan_right).min(0x3ff);

        // convert to volume as a .11 fraction
        let lvol = attenuation_to_volume(lenv << 2) as i32;
        let rvol = attenuation_to_volume(renv << 2) as i32;

        // fetch current sample and add
        let sample = i32::from(self.fetch_sample(intf));
        let outnum = (regs.ch_output_channel(self.choffs) * 2) as usize;
        output[outnum] += (lvol * sample) >> 15;
        output[outnum + 1] += (rvol * sample) >> 15;
    }

    /// Signals a key on or off, which the next prepare applies.
    fn keyonoff(&mut self, on: bool) {
        // mark the key state as pending
        self.key_state |= KEY_PENDING | if on { KEY_PENDING_ON } else { 0 };
    }

    /// Reads a byte of the external PCM memory.
    fn read_pcm<I: Interface>(intf: &mut I, address: u32) -> u8 {
        intf.external_read(AccessClass::Pcm, address)
    }

    /// Starts the attack phase.
    fn start_attack(&mut self, regs: &PcmRegisters) {
        // don't change anything if already in attack state
        if self.eg_state == EG_ATTACK {
            return;
        }
        self.eg_state = EG_ATTACK;

        // reset the LFO if requested
        if regs.ch_lfo_reset(self.choffs) != 0 {
            self.lfo_counter = 0;
        }

        // if the attack rate == 63 then immediately go to max attenuation
        if self.cache.eg_rate[EG_ATTACK] == 63 {
            self.env_attenuation = 0;
        }

        // reset the positions
        self.curpos = 0;
        self.nextpos = 0;
    }

    /// Starts the release phase.
    fn start_release(&mut self) {
        // don't change anything if already in release or reverb state
        if self.eg_state >= EG_RELEASE {
            return;
        }
        self.eg_state = EG_RELEASE;
    }

    /// Clocks the envelope generator.
    fn clock_envelope(&mut self, regs: &PcmRegisters, env_counter: u32) {
        // handle attack->decay transitions
        if self.eg_state == EG_ATTACK && self.env_attenuation == 0 {
            self.eg_state = EG_DECAY;
        }

        // handle decay->sustain transitions
        if self.eg_state == EG_DECAY && u32::from(self.env_attenuation) >= self.cache.eg_sustain {
            self.eg_state = EG_SUSTAIN;
        }

        // fetch the appropriate 6-bit rate value from the cache
        let rate = u32::from(self.cache.eg_rate[self.eg_state]);

        // compute the rate shift value; this is the shift needed to apply to
        // the env_counter such that it becomes a 5.11 fixed point number
        let rate_shift = rate >> 2;
        let env_counter = env_counter << rate_shift;

        // see if the fractional part is 0; if not, it's not time to clock
        if bitfield(env_counter, 0, 11) != 0 {
            return;
        }

        // determine the increment based on the non-fractional part of
        // env_counter
        let relevant_bits = bitfield(
            env_counter,
            if rate_shift <= 11 { 11 } else { rate_shift },
            3,
        );
        let increment = attenuation_increment(rate, relevant_bits);

        // attack is the only one that increases
        if self.eg_state == EG_ATTACK {
            // `att += (~att * increment) >> 4` on a uint16_t, which C
            // evaluates in 32 bits, unsigned once the increment joins, and
            // truncates back
            let att = u32::from(self.env_attenuation);
            self.env_attenuation = att.wrapping_add((!att).wrapping_mul(increment) >> 4) as u16;
        }
        // all other cases are similar
        else {
            // apply the increment
            self.env_attenuation = self.env_attenuation.wrapping_add(increment as u16);

            // clamp the final attenuation
            if self.env_attenuation >= 0x400 {
                self.env_attenuation = 0x3ff;
            }

            // transition to reverb at -18dB if enabled
            if self.env_attenuation >= 0xc0
                && self.eg_state < EG_REVERB
                && regs.ch_pseudo_reverb(self.choffs) != 0
            {
                self.eg_state = EG_REVERB;
            }
        }
    }

    /// Fetches the sample at the current position.
    fn fetch_sample<I: Interface>(&self, intf: &mut I) -> i16 {
        let mut addr = self.baseaddr;
        let pos = self.curpos >> 16;

        // C assembles each sample as an int and returns it as an int16_t,
        // which keeps the low 16 bits
        let byte = |intf: &mut I, address: u32| u32::from(Self::read_pcm(intf, address));

        // 8-bit PCM: shift up by 8
        if self.format == 0 {
            return (byte(intf, addr.wrapping_add(pos)) << 8) as u16 as i16;
        }

        // 16-bit PCM: assemble from 2 halves
        if self.format == 2 {
            addr = addr.wrapping_add(pos.wrapping_mul(2));
            let high = byte(intf, addr) << 8;
            return (high | byte(intf, addr.wrapping_add(1))) as u16 as i16;
        }

        // 12-bit PCM: assemble out of half of 3 bytes
        addr = addr.wrapping_add((pos / 2).wrapping_mul(3));
        if (pos & 1) == 0 {
            let high = byte(intf, addr) << 8;
            (high | ((byte(intf, addr.wrapping_add(1)) << 4) & 0xf0)) as u16 as i16
        } else {
            let high = byte(intf, addr.wrapping_add(2)) << 8;
            (high | (byte(intf, addr.wrapping_add(1)) & 0xf0)) as u16 as i16
        }
    }
}

//*********************************************************
//  PCM ENGINE
//*********************************************************

/// The PCM engine: 24 channels and their registers. It reaches the external
/// memory through an interface its methods take, which the chip owns.
#[derive(Clone)]
pub(crate) struct PcmEngine {
    /// The envelope counter.
    env_counter: u32,
    /// The bitmask of modified channels.
    modified_channels: u32,
    /// The counter to do periodic prepare sweeps.
    prepare_count: u32,
    /// The channels.
    channels: [PcmChannel; PcmRegisters::CHANNELS],
    /// The registers.
    regs: PcmRegisters,
}

impl PcmEngine {
    /// The number of outputs.
    pub(crate) const OUTPUTS: usize = PcmRegisters::OUTPUTS;
    /// The mask of every channel.
    pub(crate) const ALL_CHANNELS: u32 = PcmRegisters::ALL_CHANNELS;

    /// Creates an engine in the state ymfm's constructor leaves it.
    pub(crate) fn new() -> Self {
        let mut channels = [PcmChannel::new(0); PcmRegisters::CHANNELS];
        for (chnum, channel) in channels.iter_mut().enumerate() {
            *channel = PcmChannel::new(chnum as u32);
        }
        PcmEngine {
            env_counter: 0,
            modified_channels: Self::ALL_CHANNELS,
            prepare_count: 0,
            channels,
            regs: PcmRegisters::new(),
        }
    }

    /// Resets the engine state.
    pub(crate) fn reset(&mut self) {
        // reset register state
        self.regs.reset();

        // reset each channel
        for channel in self.channels.iter_mut() {
            channel.reset();
        }
    }

    /// Clocks the channels in `chanmask` forward one sample.
    pub(crate) fn clock(&mut self, chanmask: u32) {
        // if something was modified, prepare; also prepare every 4k samples
        // to catch ending notes. The count only moves when nothing was
        // modified, as C's short-circuit `||` leaves it.
        let mut due = self.modified_channels != 0;
        if !due {
            due = self.prepare_count >= 4096;
            self.prepare_count = self.prepare_count.wrapping_add(1);
        }
        if due {
            // call each channel to prepare; ymfm gathers the active
            // channels here and never reads them
            for (chnum, channel) in self.channels.iter_mut().enumerate() {
                if bitfield(chanmask, chnum as u32, 1) != 0 {
                    channel.prepare(&self.regs);
                }
            }

            // reset the modified channels and prepare count
            self.modified_channels = 0;
            self.prepare_count = 0;
        }

        // increment the envelope counter; the envelope generator only clocks
        // every other sample in order to make the PCM envelopes line up with
        // the FM envelopes (after taking into account the different FM
        // sampling rate)
        self.env_counter = self.env_counter.wrapping_add(1);

        // now update the state of all the channels and operators
        for (chnum, channel) in self.channels.iter_mut().enumerate() {
            if bitfield(chanmask, chnum as u32, 1) != 0 {
                channel.clock(&self.regs, self.env_counter >> 1);
            }
        }
    }

    /// Adds the sum of the channels in `chanmask` to `output`.
    pub(crate) fn output<I: Interface>(
        &self,
        output: &mut [i32; PcmRegisters::OUTPUTS],
        chanmask: u32,
        intf: &mut I,
    ) {
        // compute the output of each channel
        for (chnum, channel) in self.channels.iter().enumerate() {
            if bitfield(chanmask, chnum as u32, 1) != 0 {
                channel.output(&self.regs, output, intf);
            }
        }
    }

    /// Reads a PCM register; the memory data register, in memory access
    /// mode, reads external memory and moves the address on.
    pub(crate) fn read<I: Interface>(&mut self, regnum: u32, intf: &mut I) -> u8 {
        // handle reads from the data register
        if regnum == 0x06 && self.regs.memory_access_mode() != 0 {
            let address = self.regs.memory_address_autoinc();
            return intf.external_read(AccessClass::Pcm, address);
        }

        self.regs.read(regnum)
    }

    /// Writes a PCM register; the memory data register, in memory access
    /// mode, writes external memory and moves the address on.
    pub(crate) fn write<I: Interface>(&mut self, regnum: u32, data: u8, intf: &mut I) {
        // handle writes to the data register
        if regnum == 0x06 && self.regs.memory_access_mode() != 0 {
            let address = self.regs.memory_address_autoinc();
            intf.external_write(AccessClass::Pcm, address, data);
            return;
        }

        // for now just mark all channels as modified
        self.modified_channels = Self::ALL_CHANNELS;

        // most writes are passive, consumed only when needed
        self.regs.write(regnum, data);

        // however, process keyons immediately
        if (0x68..=0x7f).contains(&regnum) {
            self.channels[(regnum - 0x68) as usize].keyonoff(bitfield(u32::from(data), 7, 1) != 0);
        }
        // and also wavetable writes
        else if (0x08..=0x1f).contains(&regnum) {
            self.load_wavetable(regnum - 0x08, intf);
        }
    }

    /// Loads a channel's wavetable entry by fetching its data from external
    /// memory.
    fn load_wavetable<I: Interface>(&mut self, choffs: u32, intf: &mut I) {
        let read_pcm = PcmChannel::read_pcm::<I>;

        // determine the address of the wave table header
        let wavnum = self.regs.ch_wave_table_num(choffs);
        let mut wavheader = 12 * wavnum;

        // above 384 it may be in a different bank
        if wavnum >= 384 {
            let bank = self.regs.wave_table_header();
            if bank != 0 {
                wavheader = 512 * 1024 * bank + (wavnum - 384) * 12;
            }
        }

        // fetch the 22-bit base address and 2-bit format
        let byte = u32::from(read_pcm(intf, wavheader));
        let format = bitfield(byte, 6, 2) as u8;
        let mut baseaddr = bitfield(byte, 0, 6) << 16;
        baseaddr |= u32::from(read_pcm(intf, wavheader + 1)) << 8;
        baseaddr |= u32::from(read_pcm(intf, wavheader + 2));

        // fetch the 16-bit loop position
        let mut looppos = u32::from(read_pcm(intf, wavheader + 3)) << 8;
        looppos |= u32::from(read_pcm(intf, wavheader + 4));
        looppos <<= 16;

        // fetch the 16-bit end position, which is stored as a negative value
        // for some reason that is unclear
        let mut endpos = u32::from(read_pcm(intf, wavheader + 5)) << 8;
        endpos |= u32::from(read_pcm(intf, wavheader + 6));
        endpos = endpos.wrapping_neg() << 16;

        let channel = &mut self.channels[choffs as usize];
        channel.format = format;
        channel.baseaddr = baseaddr;
        channel.looppos = looppos;
        channel.endpos = endpos;

        // remaining data values set registers
        for (register, offset) in [(0x80, 7), (0x98, 8), (0xb0, 9), (0xc8, 10), (0xe0, 11)] {
            let value = read_pcm(intf, wavheader + offset);
            self.write(register + choffs, value, intf);
        }

        // reset the envelope so we don't continue playing mid-sample from
        // previous key ons
        self.channels[choffs as usize].env_attenuation = 0x3ff;
    }

    /// Returns the registers.
    pub(crate) fn regs(&self) -> &PcmRegisters {
        &self.regs
    }
}
