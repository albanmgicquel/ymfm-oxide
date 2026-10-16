// This file is a Rust translation of ymfm's `ymfm_misc.h` and `ymfm_misc.cpp`
// at commit 81aec25 (https://github.com/aaronsgiles/ymfm), with changes: the
// chip owns its interface, and its SSG override as a type parameter; save
// states are gone (the chip is `Clone`); a new chip is reset, where ymfm
// leaves its register file undefined until `reset()`; a chip can be made
// with its override in place; and the chip gains accessors for its
// interface, its engine and its registers. Every computation
// that reaches a sample or an I/O access is kept as ymfm writes it.
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

//! The YM2149 (SSG), a stand-alone SSG: three tone channels, noise, an
//! envelope and two I/O ports, with no FM part.

use crate::ssg::{NoSsgOverride, SsgEngine, SsgOverride};
use crate::ymfm::{Interface, NullInterface, Output};

//*********************************************************
//  SSG IMPLEMENTATION CLASSES
//*********************************************************

/// The Yamaha YM2149 (SSG): three square-wave tone channels with a shared
/// noise generator and envelope, and two 8-bit I/O ports, its three outputs
/// kept apart.
///
/// The ports are the bus control lines BC2 and BC1, in bits 1 and 0 of the
/// offset: a write at 0 or 3 sets the address, a write at 2 writes the data,
/// and a read at 3 reads the data; every other access is inactive (a read
/// gives 0xff). Only the low four bits of the address select a register.
///
/// ymfm models the chip at one sample per 64 input clocks, the SSG's
/// divider of 8 times 8, without the SEL pin that halves the clock on the
/// real part; its envelope has the 32 steps of the YM2149 (the AY-3-8910's
/// 16 are not modelled), and a fixed amplitude `n` plays as envelope step
/// `2n + 1`, so 15 is the envelope's loudest.
#[derive(Clone, Debug)]
pub struct Ym2149<I: Interface = NullInterface, O: SsgOverride = NoSsgOverride> {
    /// The address register.
    address: u8,
    /// The SSG engine.
    ssg: SsgEngine<O>,
    /// The interface to the system.
    intf: I,
}

impl<I: Interface> Ym2149<I> {
    /// Creates a chip that talks to the system through `intf`, reset.
    pub fn new(intf: I) -> Self {
        Self::with_ssg_override_option(intf, None)
    }
}

impl<I: Interface, O: SsgOverride> Ym2149<I, O> {
    /// Creates a chip that talks to the system through `intf`, with
    /// `ssg_override` in place of its SSG, reset (which resets the
    /// override).
    pub fn with_ssg_override(intf: I, ssg_override: O) -> Self {
        Self::with_ssg_override_option(intf, Some(ssg_override))
    }

    fn with_ssg_override_option(intf: I, ssg_override: Option<O>) -> Self {
        let mut chip = Ym2149 {
            address: 0,
            ssg: SsgEngine::new(),
            intf,
        };
        if let Some(ssg_override) = ssg_override {
            chip.ssg.set_override(ssg_override);
        }
        chip.reset();
        chip
    }

    /// The number of outputs: channels A, B and C.
    pub const OUTPUTS: usize = SsgEngine::<O>::OUTPUTS;
    /// The number of SSG outputs.
    pub const SSG_OUTPUTS: usize = SsgEngine::<O>::OUTPUTS;

    /// Installs an override, which from now on takes the SSG's resets,
    /// reads and writes.
    pub fn ssg_override(&mut self, ssg_override: O) {
        self.ssg.set_override(ssg_override);
    }

    /// Resets the chip. As in ymfm, the address register keeps its value.
    pub fn reset(&mut self) {
        // reset the engines
        self.ssg.reset();
    }

    /// Returns the output sample rate for an input clock.
    pub fn sample_rate(&self, input_clock: u32) -> u32 {
        input_clock / SsgEngine::<O>::CLOCK_DIVIDER / 8
    }

    /// Reads the data register: the register the address selects.
    pub fn read_data(&mut self) -> u8 {
        self.ssg
            .read(&mut self.intf, u32::from(self.address & 0x0f))
    }

    /// Reads from the chip: the data at offset 3 (BC2 and BC1 high), 0xff
    /// at the others.
    pub fn read(&mut self, offset: u32) -> u8 {
        match offset & 3 {
            // BC2,BC1
            3 => self.read_data(), // read
            _ => 0xff,             // inactive, address, inactive
        }
    }

    /// Writes the address register.
    pub fn write_address(&mut self, data: u8) {
        // just set the address
        self.address = data;
    }

    /// Writes the data register: the register the address selects.
    pub fn write_data(&mut self, data: u8) {
        self.ssg
            .write(&mut self.intf, u32::from(self.address & 0x0f), data);
    }

    /// Writes to the chip: the address at offsets 0 and 3, the data at 2,
    /// nothing at 1.
    pub fn write(&mut self, offset: u32, data: u8) {
        match offset & 3 {
            // BC2,BC1
            0 | 3 => self.write_address(data), // address
            1 => {}                            // inactive
            _ => self.write_data(data),        // write
        }
    }

    /// Generates one sample into each element of `output`.
    pub fn generate(&mut self, output: &mut [Output<3>]) {
        for out in output.iter_mut() {
            // clock the SSG
            self.ssg.clock();

            // YM2149 keeps the three SSG outputs independent
            self.ssg.output(out);
        }
    }

    /// Returns the interface.
    pub fn intf(&self) -> &I {
        &self.intf
    }

    /// Returns the interface, mutably.
    pub fn intf_mut(&mut self) -> &mut I {
        &mut self.intf
    }

    /// Returns the SSG engine.
    pub fn ssg(&self) -> &SsgEngine<O> {
        &self.ssg
    }

    /// Returns the register file as last written, for a diagnostic.
    pub fn registers(&self) -> &[u8] {
        self.ssg.regs().data()
    }
}
