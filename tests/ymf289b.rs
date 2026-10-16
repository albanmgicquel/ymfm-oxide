// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The YMF289B (OPL3L) held to ymfm, sample for sample.
//!
//! The case in `tests/ymf289b` is a synthetic register log and the render
//! ymfm made of it through `oracle/oracle.cpp`, written for the purpose:
//! OPL3 notes panned to all four channel outputs, of which the chip exposes
//! two, 4-operator channels, status and data reads in and out of YMF289B
//! mode with the busy flag, register read-back from both banks, the bulk
//! register clear, and the timers with their IRQ.
//!
//! With `YMFM_ORACLE` naming a built oracle, a randomized comparison runs as
//! well, over logs this test writes itself (see `common::differential`).

#[macro_use]
mod common;

use std::fmt::Write as _;

use common::{Host, Random};
use ymfm_oxide::opl::Ymf289b;

harness_chip!(Ymf289b<Host>, 2);

#[test]
fn matches_ymfm_on_the_ymf289b() {
    common::check::<Ymf289b<Host>>("ymf289b", "ymf289b-basic");
}

/// Writes a random YMF289B log: the OPL3 register classes in both banks,
/// biased towards key-ons, with YMF289B mode, the bulk clear, the timers,
/// and status and data reads mixed in.
fn random_log(seed: u64, samples: u32) -> String {
    let mut random = Random::new(seed);
    let mut text = format!("chip ymf289b\nclock 14318180\nsamples {samples}\n");
    let mut sample = 0u32;
    let _ = writeln!(
        text,
        "0 w 2 05 3 {:02x}",
        [0x01, 0x05, 0x04, 0x00][random.below(4) as usize]
    );
    while sample < samples {
        let burst = 1 + random.below(24);
        for _ in 0..burst {
            let bank = 0x100 * random.below(2);
            let (register, value): (u32, u32) = match random.below(16) {
                0 => (0x20 + random.below(0x16), random.next() & 0xff),
                1 => (0x40 + random.below(0x16), random.next() & 0x7f),
                2 => (0x60 + random.below(0x16), random.next() & 0xff | 0x80),
                3 => (0x80 + random.below(0x16), random.next() & 0xff),
                4 => (0xa0 + random.below(9), random.next() & 0xff),
                5 | 6 => (0xb0 + random.below(9), random.next() & 0x3f),
                7 => (0xc0 + random.below(9), random.next() & 0xff),
                8 => (0xe0 + random.below(0x16), random.next() & 0x07),
                9 => (0xbd, random.next() & 0xff),
                10 => match random.below(5) {
                    0 => (0x105, random.next() & 0x05),
                    1 => (0x104, random.next() & 0x3f),
                    2 if random.below(4) == 0 => (0x108, random.next() & 0x07),
                    3 => (0x08, random.next() & 0xc0),
                    _ => (0x04, random.next() & 0xe3),
                },
                11 => (0x02 + random.below(2), random.next() & 0xff),
                12 | 13 => {
                    let _ = writeln!(text, "{sample} r {:x}", random.below(4));
                    continue;
                }
                _ => (0xb0 + random.below(9), 0x20 | (random.next() & 0x1f)),
            };
            let register = if register >= 0x100 {
                register
            } else {
                register | bank
            };
            let port = if register & 0x100 != 0 { 2 } else { 0 };
            let _ = writeln!(
                text,
                "{sample} w {port:x} {:02x} {:x} {value:02x}",
                register & 0xff,
                port + 1
            );
            // an address write alone, then a read of what it selects
            if random.below(8) == 0 {
                let _ = writeln!(text, "{sample} w {port:x} {:02x}", random.next() & 0xff);
                let _ = writeln!(text, "{sample} r 1");
            }
        }
        sample += random.below(1500);
    }
    text
}

#[test]
fn matches_ymfm_on_random_ymf289b_logs_when_an_oracle_is_named() {
    common::differential::<Ymf289b<Host>>("ymf289b", |seed| random_log(seed, 20_000));
}
