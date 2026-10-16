// This file is a Rust translation of ymfm's `ymfm_ssg.h` and `ymfm_ssg.cpp`
// at commit 81aec25 (https://github.com/aaronsgiles/ymfm), with changes: the
// `ssg_override` class is the `SsgOverride` trait, and the engine owns its
// override as a type parameter (`NoSsgOverride`, the default, cannot be
// made, so an engine without one never calls it) rather than pointing to
// one; the engine does not hold the interface, so the methods that reach it
// take it as an argument; save states are gone (the engine is `Clone`); and
// a new engine is reset, where ymfm leaves its register file undefined until
// `reset()`. Every computation that reaches a sample or an I/O access is
// kept as ymfm writes it.
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

//! The SSG, the tone, noise and envelope generator of the YM2203, YM2608 and
//! YM2610 and of the YM2149: three square-wave tone channels, one noise
//! generator, one envelope generator, and two 8-bit I/O ports.
//!
//! The register map, as ymfm documents it:
//!
//! ```text
//! System-wide registers:
//!      06 ---xxxxx Noise period
//!      07 x------- I/O B in(0) or out(1)
//!         -x------ I/O A in(0) or out(1)
//!         --x----- Noise enable(0) or disable(1) for channel C
//!         ---x---- Noise enable(0) or disable(1) for channel B
//!         ----x--- Noise enable(0) or disable(1) for channel A
//!         -----x-- Tone enable(0) or disable(1) for channel C
//!         ------x- Tone enable(0) or disable(1) for channel B
//!         -------x Tone enable(0) or disable(1) for channel A
//!      0B xxxxxxxx Envelope period fine
//!      0C xxxxxxxx Envelope period coarse
//!      0D ----x--- Envelope shape: continue
//!         -----x-- Envelope shape: attack/decay
//!         ------x- Envelope shape: alternate
//!         -------x Envelope shape: hold
//!      0E xxxxxxxx 8-bit parallel I/O port A
//!      0F xxxxxxxx 8-bit parallel I/O port B
//!
//! Per-channel registers:
//! 00,02,04 xxxxxxxx Tone period (fine) for channel A,B,C
//! 01,03,05 ----xxxx Tone period (coarse) for channel A,B,C
//! 08,09,0A ---x---- Mode: fixed(0) or variable(1) for channel A,B,C
//!          ----xxxx Amplitude for channel A,B,C
//! ```
//!
//! The engine runs at a fixed division of its input clock
//! ([`SsgEngine::CLOCK_DIVIDER`]); one call to [`SsgEngine::clock`] is one
//! step of it, and [`SsgEngine::output`] gives the three channels' levels.

use crate::ymfm::{bitfield, AccessClass, Interface, Output};

//*********************************************************
//  OVERRIDE INTERFACE
//*********************************************************

/// Lets another implementation take the place of the internal SSG: once an
/// engine has an override, its resets, reads and writes go to the override
/// instead, and it is told when the prescale changes.
pub trait SsgOverride {
    /// Resets the override's state.
    fn ssg_reset(&mut self);

    /// Reads SSG register `regnum` (0 to 15).
    fn ssg_read(&mut self, regnum: u32) -> u8;

    /// Writes SSG register `regnum` (0 to 15).
    fn ssg_write(&mut self, regnum: u32, data: u8);

    /// Reports that the chip's prescale, and so the SSG's clock, has changed.
    fn ssg_prescale_changed(&mut self);
}

impl<T: SsgOverride + ?Sized> SsgOverride for &mut T {
    fn ssg_reset(&mut self) {
        (**self).ssg_reset()
    }
    fn ssg_read(&mut self, regnum: u32) -> u8 {
        (**self).ssg_read(regnum)
    }
    fn ssg_write(&mut self, regnum: u32, data: u8) {
        (**self).ssg_write(regnum, data)
    }
    fn ssg_prescale_changed(&mut self) {
        (**self).ssg_prescale_changed()
    }
}

/// The override of an engine that has none. It has no values, so it can
/// never be installed and is never called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoSsgOverride {}

impl SsgOverride for NoSsgOverride {
    fn ssg_reset(&mut self) {
        match *self {}
    }
    fn ssg_read(&mut self, _regnum: u32) -> u8 {
        match *self {}
    }
    fn ssg_write(&mut self, _regnum: u32, _data: u8) {
        match *self {}
    }
    fn ssg_prescale_changed(&mut self) {
        match *self {}
    }
}

//*********************************************************
//  REGISTER CLASS
//*********************************************************

/// The SSG register file, with its fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SsgRegisters {
    /// The register data.
    regdata: [u8; SsgRegisters::REGISTERS],
}

impl Default for SsgRegisters {
    fn default() -> Self {
        Self::new()
    }
}

impl SsgRegisters {
    /// The number of outputs.
    pub const OUTPUTS: usize = 3;
    /// The number of channels.
    pub const CHANNELS: usize = 3;
    /// The number of 8-bit registers.
    pub const REGISTERS: usize = 0x10;
    /// A mask of every channel.
    pub const ALL_CHANNELS: u32 = (1 << Self::CHANNELS) - 1;

    /// Creates a register file of zeros.
    pub fn new() -> Self {
        SsgRegisters {
            regdata: [0; Self::REGISTERS],
        }
    }

    /// Resets every register to zero.
    pub fn reset(&mut self) {
        self.regdata = [0; Self::REGISTERS];
    }

    /// Reads register `index`, which must be below 16.
    pub fn read(&self, index: u32) -> u8 {
        self.regdata[index as usize]
    }

    /// Writes register `index`, which must be below 16.
    pub fn write(&mut self, index: u32, data: u8) {
        self.regdata[index as usize] = data;
    }

    /// Returns the register data.
    pub fn data(&self) -> &[u8] {
        &self.regdata
    }

    fn reg(&self, index: usize) -> u32 {
        u32::from(self.regdata[index])
    }

    // system-wide registers

    /// The noise period.
    pub fn noise_period(&self) -> u32 {
        bitfield(self.reg(0x06), 0, 5)
    }
    /// Whether I/O port B is an output.
    pub fn io_b_out(&self) -> u32 {
        bitfield(self.reg(0x07), 7, 1)
    }
    /// Whether I/O port A is an output.
    pub fn io_a_out(&self) -> u32 {
        bitfield(self.reg(0x07), 6, 1)
    }
    /// The envelope period.
    pub fn envelope_period(&self) -> u32 {
        self.reg(0x0b) | (self.reg(0x0c) << 8)
    }
    /// The envelope shape's continue bit.
    pub fn envelope_continue(&self) -> u32 {
        bitfield(self.reg(0x0d), 3, 1)
    }
    /// The envelope shape's attack bit.
    pub fn envelope_attack(&self) -> u32 {
        bitfield(self.reg(0x0d), 2, 1)
    }
    /// The envelope shape's alternate bit.
    pub fn envelope_alternate(&self) -> u32 {
        bitfield(self.reg(0x0d), 1, 1)
    }
    /// The envelope shape's hold bit.
    pub fn envelope_hold(&self) -> u32 {
        bitfield(self.reg(0x0d), 0, 1)
    }
    /// The data of I/O port A.
    pub fn io_a_data(&self) -> u32 {
        self.reg(0x0e)
    }
    /// The data of I/O port B.
    pub fn io_b_data(&self) -> u32 {
        self.reg(0x0f)
    }

    // per-channel registers

    /// Whether noise is disabled on channel `choffs`.
    pub fn ch_noise_enable_n(&self, choffs: u32) -> u32 {
        bitfield(self.reg(0x07), 3 + choffs, 1)
    }
    /// Whether the tone is disabled on channel `choffs`.
    pub fn ch_tone_enable_n(&self, choffs: u32) -> u32 {
        bitfield(self.reg(0x07), choffs, 1)
    }
    /// The tone period of channel `choffs`.
    pub fn ch_tone_period(&self, choffs: u32) -> u32 {
        let index = 2 * choffs as usize;
        self.reg(index) | (bitfield(self.reg(index + 1), 0, 4) << 8)
    }
    /// Whether channel `choffs` takes its level from the envelope.
    pub fn ch_envelope_enable(&self, choffs: u32) -> u32 {
        bitfield(self.reg(0x08 + choffs as usize), 4, 1)
    }
    /// The fixed amplitude of channel `choffs`.
    pub fn ch_amplitude(&self, choffs: u32) -> u32 {
        bitfield(self.reg(0x08 + choffs as usize), 0, 4)
    }
}

//*********************************************************
//  CORE ENGINE
//*********************************************************

/// Volume to amplitude table, taken from MAME's implementation but biased so
/// that 0 == 0.
const AMPLITUDES: [i16; 32] = [
    0, 32, 78, 141, 178, 222, 262, 306, 369, 441, 509, 585, 701, 836, 965, 1112, 1334, 1595, 1853,
    2146, 2576, 3081, 3576, 4135, 5000, 6006, 7023, 8155, 9963, 11976, 14132, 16382,
];

/// The SSG engine: the registers, the three tone counters, the noise
/// generator and the envelope generator, with an optional override `O`.
///
/// The engine does not own the chip's interface: [`read`](Self::read) and
/// [`write`](Self::write), which reach the I/O ports, take it as an argument.
#[derive(Clone, Debug)]
pub struct SsgEngine<O: SsgOverride = NoSsgOverride> {
    /// The current tone counters.
    tone_count: [u32; 3],
    /// The current tone states.
    tone_state: [u32; 3],
    /// The envelope counter.
    envelope_count: u32,
    /// The envelope state.
    envelope_state: u32,
    /// The current noise counter.
    noise_count: u32,
    /// The current noise state.
    noise_state: u32,
    /// The registers.
    regs: SsgRegisters,
    /// The override, if one is installed.
    ssg_override: Option<O>,
}

impl<O: SsgOverride> Default for SsgEngine<O> {
    fn default() -> Self {
        Self::new()
    }
}

impl<O: SsgOverride> SsgEngine<O> {
    /// The number of outputs, one per channel.
    pub const OUTPUTS: usize = SsgRegisters::OUTPUTS;
    /// The number of channels.
    pub const CHANNELS: usize = SsgRegisters::CHANNELS;
    /// The engine steps once per this many input clocks.
    pub const CLOCK_DIVIDER: u32 = 8;

    /// Creates an engine with no override, reset.
    pub fn new() -> Self {
        SsgEngine {
            tone_count: [0; 3],
            tone_state: [0; 3],
            envelope_count: 0,
            envelope_state: 0,
            noise_count: 0,
            noise_state: 1,
            regs: SsgRegisters::new(),
            ssg_override: None,
        }
    }

    /// Installs an override, which from now on takes the engine's resets,
    /// reads and writes.
    pub fn set_override(&mut self, ssg_override: O) {
        self.ssg_override = Some(ssg_override);
    }

    /// Returns the override, if one is installed.
    pub fn ssg_override(&self) -> Option<&O> {
        self.ssg_override.as_ref()
    }

    /// Returns the override mutably, if one is installed.
    pub fn ssg_override_mut(&mut self) -> Option<&mut O> {
        self.ssg_override.as_mut()
    }

    /// Resets the engine, or the override if one is installed.
    pub fn reset(&mut self) {
        // defer to the override if present
        if let Some(ssg_override) = self.ssg_override.as_mut() {
            return ssg_override.ssg_reset();
        }

        // reset register state
        self.regs.reset();

        // reset engine state
        self.tone_count = [0; 3];
        self.tone_state = [0; 3];
        self.envelope_count = 0;
        self.envelope_state = 0;
        self.noise_count = 0;
        self.noise_state = 1;
    }

    /// Steps the engine once: the tones, the noise and the envelope.
    pub fn clock(&mut self) {
        // clock tones; tone period units are clock/16 but since we run at
        // clock/8 that works out for us to toggle the state (50% duty cycle)
        // at twice the programmed period
        for chan in 0..3 {
            self.tone_count[chan] = self.tone_count[chan].wrapping_add(1);
            if self.tone_count[chan] >= self.regs.ch_tone_period(chan as u32) {
                self.tone_state[chan] ^= 1;
                self.tone_count[chan] = 0;
            }
        }

        // clock noise; noise period units are clock/16 but since we run at
        // clock/8, our counter needs a right shift prior to compare; note
        // that a period of 0 should produce an indentical result to a period
        // of 1, so add a special check against that case
        self.noise_count = self.noise_count.wrapping_add(1);
        if (self.noise_count >> 1) >= self.regs.noise_period() && self.noise_count != 1 {
            self.noise_state ^=
                (bitfield(self.noise_state, 0, 1) ^ bitfield(self.noise_state, 3, 1)) << 17;
            self.noise_state >>= 1;
            self.noise_count = 0;
        }

        // clock envelope; envelope period units are clock/8 (manual says
        // clock/256 but that's for all 32 steps)
        self.envelope_count = self.envelope_count.wrapping_add(1);
        if self.envelope_count >= self.regs.envelope_period() {
            self.envelope_state = self.envelope_state.wrapping_add(1);
            self.envelope_count = 0;
        }
    }

    /// Computes the three channels' outputs into `output`.
    pub fn output(&mut self, output: &mut Output<3>) {
        // compute the envelope volume
        let envelope_volume = if (self.regs.envelope_hold() | (self.regs.envelope_continue() ^ 1))
            != 0
            && self.envelope_state >= 32
        {
            self.envelope_state = 32;
            if ((self.regs.envelope_attack() ^ self.regs.envelope_alternate())
                & self.regs.envelope_continue())
                != 0
            {
                31
            } else {
                0
            }
        } else {
            let mut attack = self.regs.envelope_attack();
            if self.regs.envelope_alternate() != 0 {
                attack ^= bitfield(self.envelope_state, 5, 1);
            }
            (self.envelope_state & 31) ^ if attack != 0 { 0 } else { 31 }
        };

        // iterate over channels
        for chan in 0..3u32 {
            // noise depends on the noise state, which is the LSB of
            // noise_state
            let noise_on = self.regs.ch_noise_enable_n(chan) | self.noise_state;

            // tone depends on the current tone state
            let tone_on = self.regs.ch_tone_enable_n(chan) | self.tone_state[chan as usize];

            let volume = if (noise_on & tone_on) == 0 {
                // if neither tone nor noise enabled, return 0
                0
            } else if self.regs.ch_envelope_enable(chan) != 0 {
                // if the envelope is enabled, use its amplitude
                envelope_volume
            } else {
                // otherwise, scale the tone amplitude up to match envelope
                // values according to the datasheet, amplitude 15 maps to
                // envelope 31
                let mut volume = self.regs.ch_amplitude(chan) * 2;
                if volume != 0 {
                    volume |= 1;
                }
                volume
            };

            // convert to amplitude
            output.data[chan as usize] = i32::from(AMPLITUDES[volume as usize]);
        }
    }

    /// Reads SSG register `regnum` (0 to 15), from the override if one is
    /// installed; an I/O port set as an input reads through `intf`.
    pub fn read<I: Interface + ?Sized>(&mut self, intf: &mut I, regnum: u32) -> u8 {
        // defer to the override if present
        if let Some(ssg_override) = self.ssg_override.as_mut() {
            return ssg_override.ssg_read(regnum);
        }

        // read from the I/O ports call the handlers if they are configured
        // for input
        if regnum == 0x0e && self.regs.io_a_out() == 0 {
            return intf.external_read(AccessClass::Io, 0);
        } else if regnum == 0x0f && self.regs.io_b_out() == 0 {
            return intf.external_read(AccessClass::Io, 1);
        }

        // otherwise just return the register value
        self.regs.read(regnum)
    }

    /// Writes SSG register `regnum` (0 to 15), to the override if one is
    /// installed; a write to an I/O port set as an output goes out through
    /// `intf`.
    pub fn write<I: Interface + ?Sized>(&mut self, intf: &mut I, regnum: u32, data: u8) {
        // defer to the override if present
        if let Some(ssg_override) = self.ssg_override.as_mut() {
            return ssg_override.ssg_write(regnum, data);
        }

        // store the raw value to the register array; most writes are
        // passive, consumed only when needed
        self.regs.write(regnum, data);

        if regnum == 0x0d {
            // writes to the envelope shape register reset the state
            self.envelope_state = 0;
        } else if regnum == 0x0e && self.regs.io_a_out() != 0 {
            // writes to the I/O ports call the handlers if they are
            // configured for output
            intf.external_write(AccessClass::Io, 0, data);
        } else if regnum == 0x0f && self.regs.io_b_out() != 0 {
            intf.external_write(AccessClass::Io, 1, data);
        }
    }

    /// Returns the registers.
    pub fn regs(&self) -> &SsgRegisters {
        &self.regs
    }

    /// Returns the registers, mutably.
    pub fn regs_mut(&mut self) -> &mut SsgRegisters {
        &mut self.regs
    }

    /// Whether an override is installed.
    pub fn overridden(&self) -> bool {
        self.ssg_override.is_some()
    }

    /// Reports a change of the prescale to the override, if one is
    /// installed.
    pub fn prescale_changed(&mut self) {
        if let Some(ssg_override) = self.ssg_override.as_mut() {
            ssg_override.ssg_prescale_changed();
        }
    }
}
