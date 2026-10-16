// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! A pure-Rust port of ymfm, Aaron Giles's BSD-3-Clause emulator of the
//! Yamaha FM sound chips, at upstream commit
//! `81aec25ccbb98f4873a255f7551ac4dadac59b4a`. This project is not
//! affiliated with or endorsed by the upstream project or its author.
//!
//! The modules follow the upstream files: [`ymfm`] the interface and the
//! shared helpers, `fm` the generic FM engine (crate-private), and one module
//! per chip family. Each translated file keeps the original copyright
//! notice, conditions and disclaimer, with a note of what the translation
//! changed.
//!
//! Every chip upstream implements is ported:
//!
//! | Module    | Chips |
//! |-----------|-------|
//! | [`misc`]  | YM2149 (SSG) |
//! | [`opl`]   | YM3526 (OPL), Y8950 (MSX-Audio), YM3812 (OPL2), YMF262 (OPL3), YMF289B (OPL3L), YMF278B (OPL4), YM2413 (OPLL), YM2423 (OPLL-X), YMF281 (OPLLP), DS1001 (Konami VRC7) |
//! | [`opm`]   | YM2151 (OPM), YM2164 (OPP) |
//! | [`opn`]   | YM2203 (OPN), YM2608 (OPNA), YMF288 (OPN3L), YM2610 (OPNB), YM2610B (OPNB2), YM2612 (OPN2), YM3438 (OPN2C), YMF276 (OPN2L) |
//! | [`opq`]   | YM3806 (OPQ), YM3533 |
//! | [`opz`]   | YM2414 (OPZ) |
//!
//! [`ssg`], [`adpcm`] and [`pcm`] hold the engines the chips share beside
//! their FM engine: the SSG, ADPCM-A and ADPCM-B, and the OPL4's wavetable.
//! The YMF271 (OPX), of which upstream has only a header and no
//! implementation, is out of scope.
//!
//! A chip is a plain value: it is created with an [`Interface`], written
//! through its ports as the hardware is, and pulled one sample at a time at
//! its own rate, `sample_rate(clock)`. Status reads, busy time, timers and
//! the IRQ line go through the interface; [`NullInterface`] leaves them all
//! inert. A host that wants timers, when `set_timer` asks, calls the chip's
//! `timer_expired` once that many input clocks have passed.
//!
//! The output is held to the original sample for sample: golden renders of
//! synthetic register logs, made by the original through an oracle program,
//! and a randomized differential test against it.
//!
//! The crate is `#![no_std]`: it uses `core` alone and allocates nothing, so
//! creating, resetting and rendering a chip are all safe on an audio
//! callback.
//!
//! ```
//! use ymfm_oxide::opm::Ym2151;
//! use ymfm_oxide::{NullInterface, Output};
//!
//! let mut chip = Ym2151::new(NullInterface);
//! // Channel 0 on algorithm 7 with only its fourth operator open: a sine
//! // at key code 0x4a (A4), keyed on.
//! let writes = [
//!     (0x20, 0xc7), // both outputs, no feedback, algorithm 7
//!     (0x60, 0x7f), // operators 1 to 3 silent
//!     (0x68, 0x7f),
//!     (0x70, 0x7f),
//!     (0x78, 0x00), // operator 4 at full level
//!     (0x58, 0x01), // multiple 1
//!     (0x98, 0x1f), // the fastest attack
//!     (0xf8, 0x0f), // the fastest release
//!     (0x28, 0x4a), // key code
//!     (0x08, 0x40), // key on operator 4 of channel 0
//! ];
//! for (register, value) in writes {
//!     chip.write(0, register); // address port
//!     chip.write(1, value); // data port
//! }
//! let mut samples = [Output::<2>::default(); 256];
//! chip.generate(&mut samples);
//! assert!(samples.iter().any(|sample| sample.data[0] != 0));
//! assert_eq!(chip.sample_rate(3_579_545), 55_930);
//! ```

#![no_std]
#![warn(missing_docs)]

pub mod adpcm;
mod fm;
pub mod misc;
pub mod opl;
pub mod opm;
pub mod opn;
pub mod opq;
pub mod opz;
pub mod pcm;
pub mod ssg;
pub mod ymfm;

pub use ymfm::{AccessClass, Interface, NullInterface, Output};
