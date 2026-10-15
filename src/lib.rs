// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! A Yamaha OPL2 (YM3812) and OPL3 (YMF262) FM synthesizer, in pure Rust.
//!
//! The emulation is a translation of the OPL core of ymfm, Aaron Giles's
//! BSD-3-Clause emulator of the Yamaha FM family, at commit 81aec25. `fm.rs`
//! translates its FM engine and `registers.rs` its OPL register file and chip
//! front ends; each keeps the original copyright notice, conditions and
//! disclaimer in its header. This file is the crate's own glue: the one
//! surface a caller drives a chip through.
//!
//! The translation keeps every computation that reaches a sample as the
//! original writes it, and the tests in `tests/opl.rs` hold it to the original
//! sample for sample: golden renders of synthetic register logs that cover
//! every waveform, both 2-operator and all four 4-operator connections,
//! feedback, the envelope at several rates with key scaling, both LFOs at both
//! depths, rhythm mode and OPL3 stereo. What the original models and this
//! leaves out is everything that is read back rather than heard: status,
//! timers, IRQs and busy time.
//!
//! The crate is `#![no_std]`: it uses `core` alone, allocates nothing, and a
//! chip is a plain value of a few kilobytes, so creating, resetting and
//! rendering one are all safe on an audio callback.
//!
//! ```
//! use ymfm_oxide::{Chip, Kind};
//!
//! let mut chip = Chip::new(Kind::Opl2);
//! // A held sine on channel 0: modulator silent, carrier at full level,
//! // frequency number 0x241 in block 4, which is about 440 Hz, keyed on.
//! let note = [
//!     (0x20, 0x01),
//!     (0x23, 0x21),
//!     (0x40, 0x3f),
//!     (0x43, 0x00),
//!     (0x63, 0xf0),
//!     (0x83, 0x0f),
//!     (0xa0, 0x41),
//!     (0xb0, 0x32),
//! ];
//! for (register, value) in note {
//!     chip.write(register, value);
//! }
//! let samples: Vec<(i16, i16)> = (0..100).map(|_| chip.generate()).collect();
//! // The OPL2 is mono, so both sides carry the same sample.
//! assert!(samples.iter().all(|(left, right)| left == right));
//! assert!(samples.iter().any(|(left, _)| *left != 0));
//! ```

#![no_std]
#![warn(missing_docs)]

mod fm;
mod registers;

use registers::{Ym3812, Ymf262};

/// The rate a chip generates samples at, in hertz: its 14.31818 MHz master
/// clock over 288 on the OPL3, and 3.579545 MHz over 72 on the OPL2, which is
/// the same 49 715.9 Hz rounded.
pub const RATE: u32 = 49_716;

/// Selects which chip to emulate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The YM3812: nine channels of two operators, four waveforms, mono.
    Opl2,
    /// The YMF262: eighteen channels that pair into 4-operator voices, eight
    /// waveforms, and stereo from the pan bits of 0xC0 to 0xC8.
    Opl3,
}

impl Kind {
    /// Returns the highest register address the chip decodes: 0xFF on the
    /// OPL2, and 0x1FF on the OPL3, whose second bank is 0x100 up.
    pub const fn last_register(self) -> u16 {
        match self {
            Kind::Opl2 => 0xff,
            Kind::Opl3 => 0x1ff,
        }
    }
}

#[derive(Clone)]
enum Inner {
    Opl2(Ym3812),
    Opl3(Ymf262),
}

/// One emulated chip.
#[derive(Clone)]
pub struct Chip {
    inner: Inner,
}

impl Chip {
    /// Creates a chip in its power-on state.
    pub fn new(kind: Kind) -> Chip {
        let inner = match kind {
            Kind::Opl2 => Inner::Opl2(Ym3812::new()),
            Kind::Opl3 => Inner::Opl3(Ymf262::new()),
        };
        Chip { inner }
    }

    /// Reports which chip this is.
    pub fn kind(&self) -> Kind {
        match self.inner {
            Inner::Opl2(_) => Kind::Opl2,
            Inner::Opl3(_) => Kind::Opl3,
        }
    }

    /// Writes `value` to `register` the way a driver does, through the address
    /// port and then the data port.
    ///
    /// An OPL3 register from 0x100 up goes through the second bank's address
    /// port, and the chip itself masks it back to the first bank until the NEW
    /// bit in 0x105 is set, as the hardware does. Bits above
    /// `Kind::last_register` are ignored.
    pub fn write(&mut self, register: u16, value: u8) {
        match &mut self.inner {
            Inner::Opl2(chip) => {
                chip.write(0, register as u8);
                chip.write(1, value);
            }
            Inner::Opl3(chip) => {
                if register & 0x100 != 0 {
                    chip.write(2, register as u8);
                    chip.write(3, value);
                } else {
                    chip.write(0, register as u8);
                    chip.write(1, value);
                }
            }
        }
    }

    /// Generates the next sample as a left and right pair.
    ///
    /// The OPL2 is mono, so both halves carry its one output. The OPL3's are
    /// its outputs A and B, which the pan bits of each channel select.
    pub fn generate(&mut self) -> (i16, i16) {
        match &mut self.inner {
            Inner::Opl2(chip) => {
                let sample = chip.generate() as i16;
                (sample, sample)
            }
            Inner::Opl3(chip) => {
                let output = chip.generate();
                (output[0] as i16, output[1] as i16)
            }
        }
    }

    /// Returns the chip to its power-on state, as `new` makes it.
    pub fn reset(&mut self) {
        *self = Chip::new(self.kind());
    }

    /// Returns the register file as last written, 0x200 bytes of which the
    /// OPL2 uses the first 0x100. For a diagnostic: the chip reads its
    /// registers here, but holds envelope and phase state elsewhere.
    pub fn registers(&self) -> &[u8; 0x200] {
        match &self.inner {
            Inner::Opl2(chip) => chip.registers(),
            Inner::Opl3(chip) => chip.registers(),
        }
    }
}
