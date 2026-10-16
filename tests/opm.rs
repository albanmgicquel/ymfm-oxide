// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The OPM held to ymfm, sample for sample.
//!
//! Each case in `tests/opm` is a synthetic register log and the render ymfm
//! made of it through `oracle/oracle.cpp`: all eight connections, feedback,
//! the envelope phases and rates, key scaling, both detunes and the
//! multiple across every key code, each LFO waveform at every sensitivity,
//! the noise generator, panning, a clipping full chip, per-operator key-on,
//! the timers with status, busy, IRQ and CSM, the CT pins, and the YM2164.
//!
//! With `YMFM_ORACLE` naming a built oracle, a randomized comparison runs as
//! well, over logs this test writes itself (see `common::differential`).

#[macro_use]
mod common;

use std::fmt::Write as _;

use common::{Host, Random};
use ymfm_oxide::opm::{Ym2151, Ym2164};

harness_chip!(Ym2151<Host>, 2);
harness_chip!(Ym2164<Host>, 2);

/// Holds the log `name` to its golden, on the chip the log names.
fn check(name: &str) {
    let log = common::read_log("opm", name);
    match log.chip.as_str() {
        "ym2151" => common::check::<Ym2151<Host>>("opm", name),
        "ym2164" => common::check::<Ym2164<Host>>("opm", name),
        other => panic!("{name}: not an OPM chip: {other}"),
    }
}

#[test]
fn matches_ymfm_on_all_eight_algorithms() {
    check("opm-algorithms");
}

#[test]
fn matches_ymfm_on_feedback() {
    check("opm-feedback");
}

#[test]
fn matches_ymfm_on_the_envelope() {
    check("opm-envelopes");
}

#[test]
fn matches_ymfm_on_key_scaling() {
    check("opm-keyscale");
}

#[test]
fn matches_ymfm_on_detune_multiple_and_every_key_code() {
    check("opm-detune");
}

#[test]
fn matches_ymfm_on_the_lfo() {
    check("opm-lfo");
}

#[test]
fn matches_ymfm_on_noise() {
    check("opm-noise");
}

#[test]
fn matches_ymfm_on_panning() {
    check("opm-pan");
}

#[test]
fn matches_ymfm_when_every_channel_is_loud() {
    check("opm-loud");
}

#[test]
fn matches_ymfm_on_per_operator_key_on() {
    check("opm-keyon");
}

#[test]
fn matches_ymfm_on_timers_status_busy_irq_and_csm() {
    check("opm-timers");
}

#[test]
fn matches_ymfm_on_the_ct_pins() {
    check("opm-ct");
}

#[test]
fn matches_ymfm_on_the_ym2164() {
    check("opp-ym2164");
}

/// Writes a random OPM log: bursts of writes to every register, biased
/// towards key-ons and audible levels, with the timers, CSM, the LFO, noise
/// and status reads mixed in.
fn random_log(seed: u64, chip: &str, samples: u32) -> String {
    let mut random = Random::new(seed);
    let mut text = format!("chip {chip}\nclock 3579545\nsamples {samples}\n");
    let mut sample = 0u32;
    while sample < samples {
        let burst = 1 + random.below(32);
        for _ in 0..burst {
            let operator = random.below(32);
            let channel = random.below(8);
            let (register, value) = match random.below(20) {
                0 => (0x40 + operator, random.next() & 0x7f),
                1 | 2 => (0x60 + operator, random.next() & 0x3f),
                3 => (0x80 + operator, random.next() & 0xdf | 0x10),
                4 => (0xa0 + operator, random.next() & 0x9f),
                5 => (0xc0 + operator, random.next() & 0xdf),
                6 => (0xe0 + operator, random.next() & 0xff),
                7 => (0x20 + channel, random.next() & 0xff | 0x40),
                8 => (0x28 + channel, random.next() & 0x7f),
                9 => (0x30 + channel, random.next() & 0xfc),
                10 => (0x38 + channel, random.next() & 0x73),
                11 => (0x18, random.next() & 0xff),
                12 => (0x19, random.next() & 0xff),
                13 => (0x1b, random.next() & 0xff),
                14 => match random.below(5) {
                    0 => (0x0f, random.next() & 0x9f),
                    1 => (0x01, random.next() & 0x02),
                    2 => (0x10 + random.below(3), random.next() & 0xff),
                    _ => (0x14, random.next() & 0xff),
                },
                15 => {
                    let _ = writeln!(text, "{sample} r {:x}", random.below(2));
                    continue;
                }
                _ => (0x08, random.next() & 0x7f),
            };
            let _ = writeln!(text, "{sample} w 0 {register:02x} 1 {value:02x}");
        }
        sample += random.below(1500);
    }
    text
}

#[test]
fn matches_ymfm_on_random_ym2151_logs_when_an_oracle_is_named() {
    common::differential::<Ym2151<Host>>("ym2151", |seed| random_log(seed, "ym2151", 30_000));
}

#[test]
fn matches_ymfm_on_random_ym2164_logs_when_an_oracle_is_named() {
    common::differential::<Ym2164<Host>>("ym2164", |seed| random_log(seed, "ym2164", 10_000));
}
