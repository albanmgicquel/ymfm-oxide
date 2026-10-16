// This file is a Rust translation of ymfm's `ymfm_adpcm.h` and
// `ymfm_adpcm.cpp` at commit 81aec25 (https://github.com/aaronsgiles/ymfm),
// with changes: a channel no longer holds references to its engine and its
// registers, so the engine passes its registers to the channel's methods;
// the engines do not hold the interface either, so every method that reaches
// external memory takes it as `intf`, which the chip passes from its FM
// engine; the channels are held by value rather than through `unique_ptr`;
// the output functions are generic over the number of outputs rather than
// instantiated for one and two; the debug channel masks (all ones) and
// key-on logging are gone, and save states are replaced by `Clone`; and a new
// engine is reset, where ymfm leaves its register file undefined until
// `reset()`. Every computation that reaches a sample, a status bit or an
// external access is kept as ymfm writes it, including its integer widths.
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

//! ADPCM-A and ADPCM-B, the sample engines of the YM2608, YM2610 and Y8950.
//!
//! ADPCM-A is six channels of 4-bit ADPCM played from ROM at a fixed rate,
//! each with its own level and pan; ADPCM-B is one channel at a variable
//! rate, played from external ROM or RAM or fed by the CPU, which can also
//! read and write that memory through the chip. Neither engine owns the
//! interface: the chip that holds an engine passes its own to every method
//! that reaches external memory.
//!
//! The ADPCM-A register map, as ymfm documents it:
//!
//! ```text
//! System-wide registers:
//!      00 x------- Dump (disable=1) or keyon (0) control
//!         --xxxxxx Mask of channels to dump or keyon
//!      01 --xxxxxx Total level
//!      02 xxxxxxxx Test register
//!   08-0D x------- Pan left
//!         -x------ Pan right
//!         ---xxxxx Instrument level
//!   10-15 xxxxxxxx Start address (low)
//!   18-1D xxxxxxxx Start address (high)
//!   20-25 xxxxxxxx End address (low)
//!   28-2D xxxxxxxx End address (high)
//! ```
//!
//! The ADPCM-B register map:
//!
//! ```text
//! System-wide registers:
//!      00 x------- Start of synthesis/analysis
//!         -x------ Record
//!         --x----- External/manual driving
//!         ---x---- Repeat playback
//!         ----x--- Speaker off
//!         -------x Reset
//!      01 x------- Pan left
//!         -x------ Pan right
//!         ----x--- Start conversion
//!         -----x-- DAC enable
//!         ------x- DRAM access (1=8-bit granularity; 0=1-bit)
//!         -------x RAM/ROM (1=ROM, 0=RAM)
//!      02 xxxxxxxx Start address (low)
//!      03 xxxxxxxx Start address (high)
//!      04 xxxxxxxx End address (low)
//!      05 xxxxxxxx End address (high)
//!      06 xxxxxxxx Prescale value (low)
//!      07 -----xxx Prescale value (high)
//!      08 xxxxxxxx CPU data/buffer
//!      09 xxxxxxxx Delta-N frequency scale (low)
//!      0a xxxxxxxx Delta-N frequency scale (high)
//!      0b xxxxxxxx Level control
//!      0c xxxxxxxx Limit address (low)
//!      0d xxxxxxxx Limit address (high)
//!      0e xxxxxxxx DAC data [YM2608/10]
//!      0f xxxxxxxx PCM data [YM2608/10]
//!      0e xxxxxxxx DAC data high [Y8950]
//!      0f xx------ DAC data low [Y8950]
//!      10 -----xxx DAC data exponent [Y8950]
//! ```

use crate::ymfm::{bitfield, clamp, AccessClass, Interface, Output};

//*********************************************************
// ADPCM "A" REGISTERS
//*********************************************************

/// The ADPCM-A register set.
#[derive(Clone)]
pub(crate) struct AdpcmARegisters {
    /// The register data.
    regdata: [u8; Self::REGISTERS],
}

impl AdpcmARegisters {
    /// The number of outputs.
    #[allow(dead_code)] // ymfm declares it; the chips name their own
    pub(crate) const OUTPUTS: u32 = 2;
    /// The number of channels.
    pub(crate) const CHANNELS: usize = 6;
    /// The number of 8-bit registers.
    pub(crate) const REGISTERS: usize = 0x30;
    /// The mask of every channel.
    pub(crate) const ALL_CHANNELS: u32 = (1 << Self::CHANNELS) - 1;

    /// Creates a register set, reset.
    fn new() -> Self {
        let mut regs = AdpcmARegisters {
            regdata: [0; Self::REGISTERS],
        };
        regs.reset();
        regs
    }

    /// Resets to the initial state.
    fn reset(&mut self) {
        self.regdata = [0; Self::REGISTERS];

        // initialize the pans to on by default, and max instrument volume;
        // some neogeo homebrews (for example ffeast) rely on this
        self.regdata[0x08..0x0e].fill(0xdf);
    }

    /// Maps a channel number to its register offset.
    const fn channel_offset(chnum: u32) -> u32 {
        chnum
    }

    /// Writes a register directly.
    fn write(&mut self, index: u32, data: u8) {
        self.regdata[index as usize] = data;
    }

    /// Returns a register.
    fn reg(&self, index: u32) -> u32 {
        u32::from(self.regdata[index as usize])
    }

    /// Returns the register data, for a diagnostic.
    pub(crate) fn data(&self) -> &[u8] {
        &self.regdata
    }

    // system-wide registers
    #[allow(dead_code)] // ymfm declares it; the engine reads the control write itself
    pub(crate) fn dump(&self) -> u32 {
        bitfield(self.reg(0x00), 7, 1)
    }
    #[allow(dead_code)] // ymfm declares it; the engine reads the control write itself
    pub(crate) fn dump_mask(&self) -> u32 {
        bitfield(self.reg(0x00), 0, 6)
    }
    pub(crate) fn total_level(&self) -> u32 {
        bitfield(self.reg(0x01), 0, 6)
    }
    #[allow(dead_code)] // ymfm declares it but nothing reads it
    pub(crate) fn test(&self) -> u32 {
        self.reg(0x02)
    }

    // per-channel registers
    pub(crate) fn ch_pan_left(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x08), 7, 1)
    }
    pub(crate) fn ch_pan_right(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x08), 6, 1)
    }
    pub(crate) fn ch_instrument_level(&self, choffs: u32) -> u32 {
        bitfield(self.reg(choffs + 0x08), 0, 5)
    }
    pub(crate) fn ch_start(&self, choffs: u32) -> u32 {
        self.reg(choffs + 0x10) | (self.reg(choffs + 0x18) << 8)
    }
    pub(crate) fn ch_end(&self, choffs: u32) -> u32 {
        self.reg(choffs + 0x20) | (self.reg(choffs + 0x28) << 8)
    }

    // per-channel writes
    fn write_start(&mut self, choffs: u32, address: u32) {
        self.write(choffs + 0x10, address as u8);
        self.write(choffs + 0x18, (address >> 8) as u8);
    }
    fn write_end(&mut self, choffs: u32, address: u32) {
        self.write(choffs + 0x20, address as u8);
        self.write(choffs + 0x28, (address >> 8) as u8);
    }
}

//*********************************************************
// ADPCM "A" CHANNEL
//*********************************************************

/// One ADPCM-A channel.
#[derive(Clone, Copy)]
pub(crate) struct AdpcmAChannel {
    /// The channel offset.
    choffs: u32,
    /// The address bits shift-left.
    address_shift: u32,
    /// Currently playing?
    playing: u32,
    /// The index of the current nibble.
    curnibble: u32,
    /// The current byte of data.
    curbyte: u32,
    /// The current address.
    curaddress: u32,
    /// The accumulator.
    accumulator: i32,
    /// The index in the stepping table.
    step_index: i32,
}

impl AdpcmAChannel {
    /// Creates a channel in the state ymfm's constructor leaves it.
    const fn new(choffs: u32, addrshift: u32) -> Self {
        AdpcmAChannel {
            choffs,
            address_shift: addrshift,
            playing: 0,
            curnibble: 0,
            curbyte: 0,
            curaddress: 0,
            accumulator: 0,
            step_index: 0,
        }
    }

    /// Resets the channel state.
    fn reset(&mut self) {
        self.playing = 0;
        self.curnibble = 0;
        self.curbyte = 0;
        self.curaddress = 0;
        self.accumulator = 0;
        self.step_index = 0;
    }

    /// Signals a key on or off.
    fn keyonoff(&mut self, regs: &AdpcmARegisters, on: bool) {
        // QUESTION (ymfm's): repeated key ons restart the sample?
        self.playing = u32::from(on);
        if self.playing != 0 {
            self.curaddress = regs.ch_start(self.choffs) << self.address_shift;
            self.curnibble = 0;
            self.curbyte = 0;
            self.accumulator = 0;
            self.step_index = 0;
        }
    }

    /// Clocks the channel; returns true when it has just reached its end.
    fn clock<I: Interface>(&mut self, regs: &AdpcmARegisters, intf: &mut I) -> bool {
        // if not playing, just output 0
        if self.playing == 0 {
            self.accumulator = 0;
            return false;
        }

        // if we're about to read nibble 0, fetch the data
        let data: u8;
        if self.curnibble == 0 {
            // stop when we hit the end address; apparently only low 20 bits
            // are used for comparison on the YM2610: this affects sample
            // playback in some games, for example twinspri character select
            // screen music will skip some samples if this is not correct
            //
            // note also: end address is inclusive, so wait until we are about
            // to fetch the sample just after the end before stopping; this is
            // needed for nitd's jump sound, for example
            let end = (regs.ch_end(self.choffs) + 1) << self.address_shift;
            if ((self.curaddress ^ end) & 0xfffff) == 0 {
                self.playing = 0;
                self.accumulator = 0;
                return true;
            }

            self.curbyte = u32::from(intf.external_read(AccessClass::AdpcmA, self.curaddress));
            self.curaddress = self.curaddress.wrapping_add(1);
            data = (self.curbyte >> 4) as u8;
            self.curnibble = 1;
        }
        // otherwise just extract from the previosuly-fetched byte
        else {
            data = (self.curbyte & 0xf) as u8;
            self.curnibble = 0;
        }
        let data = u32::from(data);

        // compute the ADPCM delta
        const STEPS: [u16; 49] = [
            16, 17, 19, 21, 23, 25, 28, //
            31, 34, 37, 41, 45, 50, 55, //
            60, 66, 73, 80, 88, 97, 107, //
            118, 130, 143, 157, 173, 190, 209, //
            230, 253, 279, 307, 337, 371, 408, //
            449, 494, 544, 598, 658, 724, 796, //
            876, 963, 1060, 1166, 1282, 1411, 1552,
        ];
        let mut delta = ((2 * bitfield(data, 0, 3) + 1)
            * u32::from(STEPS[self.step_index as usize])
            / 8) as i32;
        if bitfield(data, 3, 1) != 0 {
            delta = -delta;
        }

        // the 12-bit accumulator wraps on the ym2610 and ym2608 (like the
        // msm5205)
        self.accumulator = (self.accumulator + delta) & 0xfff;

        // adjust ADPCM step
        const STEP_INC: [i8; 8] = [-1, -1, -1, -1, 2, 5, 7, 9];
        self.step_index = clamp(
            self.step_index + i32::from(STEP_INC[bitfield(data, 0, 3) as usize]),
            0,
            48,
        );

        false
    }

    /// Adds the channel's output, with panning applied.
    fn output<const N: usize>(&self, regs: &AdpcmARegisters, output: &mut Output<N>) {
        // volume combines instrument and total levels
        let vol =
            ((regs.ch_instrument_level(self.choffs) ^ 0x1f) + (regs.total_level() ^ 0x3f)) as i32;

        // if combined is maximum, don't add to outputs
        if vol >= 63 {
            return;
        }

        // convert into a shift and a multiplier
        // QUESTION (ymfm's): verify this from other sources
        let mul = (15 - (vol & 7)) as i8;
        let shift = (4 + 1 + (vol >> 3)) as u8;

        // accumulator is a 12-bit value; shift up to sign-extend; the
        // downshift is incorporated into 'shift'; the result is kept in an
        // int16_t, as ymfm does
        let value =
            (((i32::from((self.accumulator << 4) as i16) * i32::from(mul)) >> shift) & !3) as i16;
        let value = i32::from(value);

        // apply to left/right as appropriate
        if N == 1 || regs.ch_pan_left(self.choffs) != 0 {
            output.data[0] += value;
        }
        if N > 1 && regs.ch_pan_right(self.choffs) != 0 {
            output.data[1] += value;
        }
    }
}

//*********************************************************
// ADPCM "A" ENGINE
//*********************************************************

/// The ADPCM-A engine: six channels of fixed-rate ADPCM from ROM.
#[derive(Clone)]
pub struct AdpcmAEngine {
    /// The channels.
    channels: [AdpcmAChannel; AdpcmARegisters::CHANNELS],
    /// The registers.
    regs: AdpcmARegisters,
}

impl AdpcmAEngine {
    /// The number of channels.
    pub const CHANNELS: usize = AdpcmARegisters::CHANNELS;
    /// The mask of every channel.
    pub const ALL_CHANNELS: u32 = AdpcmARegisters::ALL_CHANNELS;
    /// The number of 8-bit registers.
    pub const REGISTERS: usize = AdpcmARegisters::REGISTERS;

    /// Creates an engine whose addresses are shifted left by `addrshift`
    /// bits (0 on the YM2608, 8 on the YM2610), reset.
    pub fn new(addrshift: u32) -> Self {
        let mut channels = [AdpcmAChannel::new(0, addrshift); AdpcmARegisters::CHANNELS];
        for (chnum, channel) in channels.iter_mut().enumerate() {
            *channel = AdpcmAChannel::new(AdpcmARegisters::channel_offset(chnum as u32), addrshift);
        }
        let mut engine = AdpcmAEngine {
            channels,
            regs: AdpcmARegisters::new(),
        };
        engine.reset();
        engine
    }

    /// Resets the engine state.
    pub fn reset(&mut self) {
        // reset register state
        self.regs.reset();

        // reset each channel
        for channel in self.channels.iter_mut() {
            channel.reset();
        }
    }

    /// Clocks the channels in `chanmask`, reading sample data through
    /// `intf`, and returns the mask of those that just finished.
    pub fn clock<I: Interface>(&mut self, chanmask: u32, intf: &mut I) -> u32 {
        // clock each channel, setting a bit in result if it finished
        let mut result = 0;
        for (chnum, channel) in self.channels.iter_mut().enumerate() {
            if bitfield(chanmask, chnum as u32, 1) != 0 && channel.clock(&self.regs, intf) {
                result |= 1 << chnum;
            }
        }

        // return the bitmask of completed samples
        result
    }

    /// Adds the output of the channels in `chanmask` to `output`.
    pub fn output<const N: usize>(&self, output: &mut Output<N>, chanmask: u32) {
        // compute the output of each channel
        for (chnum, channel) in self.channels.iter().enumerate() {
            if bitfield(chanmask, chnum as u32, 1) != 0 {
                channel.output(&self.regs, output);
            }
        }
    }

    /// Writes an ADPCM-A register; a write to register 0 keys channels on or
    /// dumps them.
    pub fn write(&mut self, regnum: u32, data: u8) {
        // store the raw value to the register array; most writes are passive,
        // consumed only when needed
        self.regs.write(regnum, data);

        // actively handle writes to the control register
        if regnum == 0x00 {
            for (chnum, channel) in self.channels.iter_mut().enumerate() {
                if bitfield(u32::from(data), chnum as u32, 1) != 0 {
                    channel.keyonoff(&self.regs, bitfield(!u32::from(data), 7, 1) != 0);
                }
            }
        }
    }

    /// Sets the start and end address of a channel, as the YM2608 does for
    /// its hardcoded percussion.
    pub fn set_start_end(&mut self, chnum: u8, start: u16, end: u16) {
        let choffs = AdpcmARegisters::channel_offset(u32::from(chnum));
        self.regs.write_start(choffs, u32::from(start));
        self.regs.write_end(choffs, u32::from(end));
    }

    /// Returns the register file as last written, for a diagnostic.
    pub fn registers(&self) -> &[u8] {
        self.regs.data()
    }
}

//*********************************************************
// ADPCM "B" REGISTERS
//*********************************************************

/// The ADPCM-B register set.
#[derive(Clone)]
pub(crate) struct AdpcmBRegisters {
    /// The register data.
    regdata: [u8; Self::REGISTERS],
}

impl AdpcmBRegisters {
    /// The number of 8-bit registers.
    pub(crate) const REGISTERS: usize = 0x11;

    /// Creates a register set, reset.
    fn new() -> Self {
        let mut regs = AdpcmBRegisters {
            regdata: [0; Self::REGISTERS],
        };
        regs.reset();
        regs
    }

    /// Resets to the initial state.
    fn reset(&mut self) {
        self.regdata = [0; Self::REGISTERS];

        // default limit to wide open
        self.regdata[0x0c] = 0xff;
        self.regdata[0x0d] = 0xff;
    }

    /// Writes a register directly.
    fn write(&mut self, index: u32, data: u8) {
        self.regdata[index as usize] = data;
    }

    /// Returns a register.
    fn reg(&self, index: u32) -> u32 {
        u32::from(self.regdata[index as usize])
    }

    /// Returns the register data, for a diagnostic.
    pub(crate) fn data(&self) -> &[u8] {
        &self.regdata
    }

    // system-wide registers
    pub(crate) fn execute(&self) -> u32 {
        bitfield(self.reg(0x00), 7, 1)
    }
    pub(crate) fn record(&self) -> u32 {
        bitfield(self.reg(0x00), 6, 1)
    }
    pub(crate) fn external(&self) -> u32 {
        bitfield(self.reg(0x00), 5, 1)
    }
    pub(crate) fn repeat(&self) -> u32 {
        bitfield(self.reg(0x00), 4, 1)
    }
    #[allow(dead_code)] // ymfm declares it but nothing reads it
    pub(crate) fn speaker(&self) -> u32 {
        bitfield(self.reg(0x00), 3, 1)
    }
    pub(crate) fn resetflag(&self) -> u32 {
        bitfield(self.reg(0x00), 0, 1)
    }
    pub(crate) fn pan_left(&self) -> u32 {
        bitfield(self.reg(0x01), 7, 1)
    }
    pub(crate) fn pan_right(&self) -> u32 {
        bitfield(self.reg(0x01), 6, 1)
    }
    #[allow(dead_code)] // ymfm declares it but nothing reads it
    pub(crate) fn start_conversion(&self) -> u32 {
        bitfield(self.reg(0x01), 3, 1)
    }
    #[allow(dead_code)] // ymfm declares it but nothing reads it
    pub(crate) fn dac_enable(&self) -> u32 {
        bitfield(self.reg(0x01), 2, 1)
    }
    pub(crate) fn dram_8bit(&self) -> u32 {
        bitfield(self.reg(0x01), 1, 1)
    }
    pub(crate) fn rom_ram(&self) -> u32 {
        bitfield(self.reg(0x01), 0, 1)
    }
    pub(crate) fn start(&self) -> u32 {
        self.reg(0x02) | (self.reg(0x03) << 8)
    }
    pub(crate) fn end(&self) -> u32 {
        self.reg(0x04) | (self.reg(0x05) << 8)
    }
    #[allow(dead_code)] // ymfm declares it but nothing reads it
    pub(crate) fn prescale(&self) -> u32 {
        self.reg(0x06) | (bitfield(self.reg(0x07), 0, 3) << 8)
    }
    pub(crate) fn cpudata(&self) -> u32 {
        self.reg(0x08)
    }
    pub(crate) fn delta_n(&self) -> u32 {
        self.reg(0x09) | (self.reg(0x0a) << 8)
    }
    pub(crate) fn level(&self) -> u32 {
        self.reg(0x0b)
    }
    pub(crate) fn limit(&self) -> u32 {
        self.reg(0x0c) | (self.reg(0x0d) << 8)
    }
    #[allow(dead_code)] // ymfm declares it but nothing reads it
    pub(crate) fn dac(&self) -> u32 {
        self.reg(0x0e)
    }
    #[allow(dead_code)] // ymfm declares it but nothing reads it
    pub(crate) fn pcm(&self) -> u32 {
        self.reg(0x0f)
    }
}

//*********************************************************
// ADPCM "B" CHANNEL
//*********************************************************

/// The ADPCM-B channel.
#[derive(Clone, Copy)]
pub(crate) struct AdpcmBChannel {
    /// The address bits shift-left.
    address_shift: u32,
    /// The status: playing, end of sample, buffer ready.
    status: u32,
    /// The index of the current nibble.
    curnibble: u32,
    /// The current byte of data.
    curbyte: u32,
    /// The dummy read tracker.
    dummy_read: u32,
    /// The current fractional position.
    position: u32,
    /// The current address.
    curaddress: u32,
    /// The accumulator.
    accumulator: i32,
    /// The previous accumulator (for linear interpolation).
    prev_accum: i32,
    /// The next forecast.
    adpcm_step: i32,
}

impl AdpcmBChannel {
    const STEP_MIN: i32 = 127;
    const STEP_MAX: i32 = 24576;

    pub(crate) const STATUS_EOS: u8 = 0x01;
    pub(crate) const STATUS_BRDY: u8 = 0x02;
    pub(crate) const STATUS_PLAYING: u8 = 0x04;

    /// Creates a channel in the state ymfm's constructor leaves it.
    const fn new(addrshift: u32) -> Self {
        AdpcmBChannel {
            address_shift: addrshift,
            status: Self::STATUS_BRDY as u32,
            curnibble: 0,
            curbyte: 0,
            dummy_read: 0,
            position: 0,
            curaddress: 0,
            accumulator: 0,
            prev_accum: 0,
            adpcm_step: Self::STEP_MIN,
        }
    }

    /// Resets the channel state.
    fn reset(&mut self) {
        self.status = u32::from(Self::STATUS_BRDY);
        self.curnibble = 0;
        self.curbyte = 0;
        self.dummy_read = 0;
        self.position = 0;
        self.curaddress = 0;
        self.accumulator = 0;
        self.prev_accum = 0;
        self.adpcm_step = Self::STEP_MIN;
    }

    /// Clocks the channel.
    fn clock<I: Interface>(&mut self, regs: &AdpcmBRegisters, intf: &mut I) {
        // only process if active and not recording (which we don't support)
        if regs.execute() == 0
            || regs.record() != 0
            || (self.status & u32::from(Self::STATUS_PLAYING)) == 0
        {
            self.status &= !u32::from(Self::STATUS_PLAYING);
            return;
        }

        // otherwise, advance the step
        let position = self.position + regs.delta_n();
        self.position = u32::from(position as u16);
        if position < 0x10000 {
            return;
        }

        // if we're about to process nibble 0, fetch sample
        if self.curnibble == 0 {
            // playing from RAM/ROM
            if regs.external() != 0 {
                self.curbyte = u32::from(intf.external_read(AccessClass::AdpcmB, self.curaddress));
            }
        }

        // extract the nibble from our current byte
        let data = u32::from(((self.curbyte << (4 * self.curnibble)) as u8) >> 4);
        self.curnibble ^= 1;

        // we just processed the last nibble
        if self.curnibble == 0 {
            // if playing from RAM/ROM, check the end/limit address or advance
            if regs.external() != 0 {
                // handle the sample end, either repeating or stopping
                if self.at_end(regs) {
                    // if repeating, go back to the start
                    if regs.repeat() != 0 {
                        self.load_start(regs);
                    }
                    // otherwise, done; set the EOS bit
                    else {
                        self.accumulator = 0;
                        self.prev_accum = 0;
                        self.status = (self.status & !u32::from(Self::STATUS_PLAYING))
                            | u32::from(Self::STATUS_EOS);
                        return;
                    }
                }
                // wrap at the limit address
                else if self.at_limit(regs) {
                    self.curaddress = 0;
                }
                // otherwise, advance the current address
                else {
                    self.curaddress = self.curaddress.wrapping_add(1);
                    self.curaddress &= 0xffffff;
                }
            }
            // if CPU-driven, copy the next byte and request more
            else {
                self.curbyte = regs.cpudata();
                self.status |= u32::from(Self::STATUS_BRDY);
            }
        }

        // remember previous value for interpolation
        self.prev_accum = self.accumulator;

        // forecast to next forecast: 1/8, 3/8, 5/8, 7/8, 9/8, 11/8, 13/8, 15/8
        let mut delta = (2 * bitfield(data, 0, 3) as i32 + 1) * self.adpcm_step / 8;
        if bitfield(data, 3, 1) != 0 {
            delta = -delta;
        }

        // add and clamp to 16 bits
        self.accumulator = clamp(self.accumulator + delta, -32768, 32767);

        // scale the ADPCM step: 0.9, 0.9, 0.9, 0.9, 1.2, 1.6, 2.0, 2.4
        const STEP_SCALE: [u8; 8] = [57, 57, 57, 57, 77, 102, 128, 153];
        self.adpcm_step = clamp(
            (self.adpcm_step * i32::from(STEP_SCALE[bitfield(data, 0, 3) as usize])) / 64,
            Self::STEP_MIN,
            Self::STEP_MAX,
        );
    }

    /// Adds the channel's output, with panning applied.
    fn output<const N: usize>(&self, regs: &AdpcmBRegisters, output: &mut Output<N>, rshift: u32) {
        // do a linear interpolation between samples; ymfm's int32_t sum
        // cannot overflow, as the two weights add up to 0x10000
        let mut result = (self.prev_accum * ((self.position ^ 0xffff) + 1) as i32
            + self.accumulator * self.position as i32)
            >> 16;

        // apply volume (level) in a linear fashion and reduce
        result = (result * regs.level() as i32) >> (8 + rshift);

        // apply to left/right
        if N == 1 || regs.pan_left() != 0 {
            output.data[0] += result;
        }
        if N > 1 && regs.pan_right() != 0 {
            output.data[1] += result;
        }
    }

    /// Returns the status register.
    fn status(&self) -> u8 {
        self.status as u8
    }

    /// Handles the special register reads: register 8 reads external memory
    /// through `intf` under some conditions.
    fn read<I: Interface>(&mut self, regs: &AdpcmBRegisters, regnum: u32, intf: &mut I) -> u8 {
        let mut result = 0;

        // register 8 reads over the bus under some conditions
        if regnum == 0x08 && regs.execute() == 0 && regs.record() == 0 && regs.external() != 0 {
            // two dummy reads are consumed first
            if self.dummy_read != 0 {
                self.load_start(regs);
                self.dummy_read -= 1;
            }
            // read the data
            else {
                // read from outside of the chip
                result = intf.external_read(AccessClass::AdpcmB, self.curaddress);
                self.curaddress = self.curaddress.wrapping_add(1);

                // did we hit the end? if so, signal EOS
                if self.at_end(regs) {
                    self.status = u32::from(Self::STATUS_EOS | Self::STATUS_BRDY);
                } else {
                    // signal ready
                    self.status = u32::from(Self::STATUS_BRDY);
                }

                // wrap at the limit address
                if self.at_limit(regs) {
                    self.curaddress = 0;
                }
            }
        }
        result
    }

    /// Handles the special register writes: register 0 starts, stops and
    /// resets, and register 8 feeds data or writes external memory through
    /// `intf` under some conditions.
    fn write<I: Interface>(
        &mut self,
        regs: &AdpcmBRegisters,
        regnum: u32,
        value: u8,
        intf: &mut I,
    ) {
        // register 0 can do a reset; also use writes here to reset the dummy
        // read counter
        if regnum == 0x00 {
            if regs.execute() != 0 {
                self.load_start(regs);
            } else {
                self.status &= !u32::from(Self::STATUS_EOS);
            }
            if regs.resetflag() != 0 {
                self.reset();
            }
            if regs.external() != 0 {
                self.dummy_read = 2;
            }
        }
        // register 8 writes over the bus under some conditions
        else if regnum == 0x08 {
            // if writing from the CPU during execute, clear the ready flag
            if regs.execute() != 0 && regs.record() == 0 && regs.external() == 0 {
                self.status &= !u32::from(Self::STATUS_BRDY);
            }
            // if writing during "record", pass through as data
            else if regs.execute() == 0 && regs.record() != 0 && regs.external() != 0 {
                // clear out dummy reads and set start address
                if self.dummy_read != 0 {
                    self.load_start(regs);
                    self.dummy_read = 0;
                }

                // did we hit the end? if so, signal EOS
                if self.at_end(regs) {
                    self.status = u32::from(Self::STATUS_EOS | Self::STATUS_BRDY);
                }
                // otherwise, write the data and signal ready
                else {
                    intf.external_write(AccessClass::AdpcmB, self.curaddress, value);
                    self.curaddress = self.curaddress.wrapping_add(1);
                    self.status = u32::from(Self::STATUS_BRDY);
                }
            }
        }
    }

    /// Returns the current address shift, from the constant one or the
    /// register settings.
    fn address_shift(&self, regs: &AdpcmBRegisters) -> u32 {
        // if a constant address shift, just provide that
        if self.address_shift != 0 {
            return self.address_shift;
        }

        // if ROM or 8-bit DRAM, shift is 5 bits
        if regs.rom_ram() != 0 {
            return 5;
        }
        if regs.dram_8bit() != 0 {
            return 5;
        }

        // otherwise, shift is 2 bits
        2
    }

    /// Loads the start address and initializes the state.
    fn load_start(&mut self, regs: &AdpcmBRegisters) {
        self.status =
            (self.status & !u32::from(Self::STATUS_EOS)) | u32::from(Self::STATUS_PLAYING);
        self.curaddress = if regs.external() != 0 {
            regs.start() << self.address_shift(regs)
        } else {
            0
        };
        self.curnibble = 0;
        self.curbyte = 0;
        self.position = 0;
        self.accumulator = 0;
        self.prev_accum = 0;
        self.adpcm_step = Self::STEP_MIN;
    }

    /// The limit checker; stops at the last byte of the chunk described by
    /// `address_shift()`.
    fn at_limit(&self, regs: &AdpcmBRegisters) -> bool {
        self.curaddress == (((regs.limit() + 1) << self.address_shift(regs)) - 1)
    }

    /// The end checker; stops at the last byte of the chunk described by
    /// `address_shift()`.
    fn at_end(&self, regs: &AdpcmBRegisters) -> bool {
        self.curaddress == (((regs.end() + 1) << self.address_shift(regs)) - 1)
    }
}

//*********************************************************
// ADPCM "B" ENGINE
//*********************************************************

/// The ADPCM-B engine: one channel of variable-rate ADPCM from external
/// memory or the CPU.
#[derive(Clone)]
pub struct AdpcmBEngine {
    /// The channel (only one supported for now, as in ymfm).
    channel: AdpcmBChannel,
    /// The registers.
    regs: AdpcmBRegisters,
}

impl AdpcmBEngine {
    /// The status bit set when a sample reaches its end.
    pub const STATUS_EOS: u8 = AdpcmBChannel::STATUS_EOS;
    /// The status bit set when the buffer is ready for the CPU.
    pub const STATUS_BRDY: u8 = AdpcmBChannel::STATUS_BRDY;
    /// The status bit set while a sample plays.
    pub const STATUS_PLAYING: u8 = AdpcmBChannel::STATUS_PLAYING;
    /// The number of 8-bit registers.
    pub const REGISTERS: usize = AdpcmBRegisters::REGISTERS;

    /// Creates an engine whose addresses are shifted left by `addrshift`
    /// bits (8 on the YM2610), or by what the memory-type registers select
    /// when it is 0, reset.
    pub fn new(addrshift: u32) -> Self {
        let mut engine = AdpcmBEngine {
            channel: AdpcmBChannel::new(addrshift),
            regs: AdpcmBRegisters::new(),
        };
        engine.reset();
        engine
    }

    /// Resets the engine state.
    pub fn reset(&mut self) {
        // reset registers
        self.regs.reset();

        // reset each channel
        self.channel.reset();
    }

    /// Clocks the engine, reading sample data through `intf`.
    pub fn clock<I: Interface>(&mut self, intf: &mut I) {
        self.channel.clock(&self.regs, intf);
    }

    /// Adds the output, shifted right by `rshift`, to `output`.
    pub fn output<const N: usize>(&self, output: &mut Output<N>, rshift: u32) {
        // compute the output of each channel
        self.channel.output(&self.regs, output, rshift);
    }

    /// Reads an ADPCM-B register; only register 8 reads anything, from
    /// external memory through `intf`.
    pub fn read<I: Interface>(&mut self, regnum: u32, intf: &mut I) -> u32 {
        u32::from(self.channel.read(&self.regs, regnum, intf))
    }

    /// Writes an ADPCM-B register, writing external memory through `intf`
    /// when recording.
    pub fn write<I: Interface>(&mut self, regnum: u32, data: u8, intf: &mut I) {
        // store the raw value to the register array; most writes are passive,
        // consumed only when needed
        self.regs.write(regnum, data);

        // let the channel handle any special writes
        self.channel.write(&self.regs, regnum, data, intf);
    }

    /// Returns the status: `STATUS_EOS`, `STATUS_BRDY` and `STATUS_PLAYING`.
    pub fn status(&self) -> u8 {
        self.channel.status()
    }

    /// Returns the register file as last written, for a diagnostic.
    pub fn registers(&self) -> &[u8] {
        self.regs.data()
    }
}
