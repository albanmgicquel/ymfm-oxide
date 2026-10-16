// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The OPQ held to ymfm, sample for sample.
//!
//! Each case in `tests/opq` is a synthetic register log and the render ymfm
//! made of it through `oracle/oracle.cpp`: all eight connections, feedback,
//! the envelope phases and rates, key scaling, the 6-bit detune and the
//! multiples through both of their addresses, the two frequencies of a
//! channel, the two waveforms, the reverb stage, the LFO at every rate and
//! sensitivity with its disable bit, panning, a clipping full chip,
//! per-operator key-on, the timer with status and IRQ, and the YM3533.
//!
//! With `YMFM_ORACLE` naming a built oracle, a randomized comparison runs as
//! well, over logs this test writes itself (see `common::differential`).

#[macro_use]
mod common;

use std::fmt::Write as _;

use common::{Host, Random};
use ymfm_oxide::opq::{Ym3533, Ym3806};

harness_chip!(Ym3806<Host>, 2);
harness_chip!(Ym3533<Host>, 2);

/// Holds the log `name` to its golden, on the chip the log names.
fn check(name: &str) {
    let log = common::read_log("opq", name);
    match log.chip.as_str() {
        "ym3806" => common::check::<Ym3806<Host>>("opq", name),
        "ym3533" => common::check::<Ym3533<Host>>("opq", name),
        other => panic!("{name}: not an OPQ chip: {other}"),
    }
}

#[test]
fn matches_ymfm_on_all_eight_algorithms() {
    check("opq-algorithms");
}

#[test]
fn matches_ymfm_on_feedback() {
    check("opq-feedback");
}

#[test]
fn matches_ymfm_on_the_envelope() {
    check("opq-envelopes");
}

#[test]
fn matches_ymfm_on_key_scaling() {
    check("opq-keyscale");
}

#[test]
fn matches_ymfm_on_detune_and_multiple() {
    check("opq-detune");
}

#[test]
fn matches_ymfm_on_the_two_frequencies_of_a_channel() {
    check("opq-frequencies");
}

#[test]
fn matches_ymfm_on_the_waveforms() {
    check("opq-waveforms");
}

#[test]
fn matches_ymfm_on_the_reverb_stage() {
    check("opq-reverb");
}

#[test]
fn matches_ymfm_on_the_lfo() {
    check("opq-lfo");
}

#[test]
fn matches_ymfm_on_panning() {
    check("opq-pan");
}

#[test]
fn matches_ymfm_when_every_channel_is_loud() {
    check("opq-loud");
}

#[test]
fn matches_ymfm_on_per_operator_key_on() {
    check("opq-keyon");
}

#[test]
fn matches_ymfm_on_the_timer() {
    check("opq-timers");
}

#[test]
fn matches_ymfm_on_the_ym3533() {
    check("opq-ym3533");
}

/// Writes a random OPQ log: bursts of writes to every register the chip has
/// (the internal multiples at 0x100-0x11F included), biased towards key-ons
/// and audible levels, with the timer, the LFO and status reads mixed in.
fn random_log(seed: u64, chip: &str, samples: u32) -> String {
    let mut random = Random::new(seed);
    let mut text = format!("chip {chip}\nclock 3579545\nsamples {samples}\n");
    let mut sample = 0u32;
    while sample < samples {
        let burst = 1 + random.below(32);
        for _ in 0..burst {
            let operator = random.below(32);
            let channel = random.below(8);
            let (register, value) = match random.below(22) {
                0 | 1 => (0x40 + operator, random.next() & 0xff),
                2 | 3 => (0x60 + operator, random.next() & 0x3f),
                4 => (0x80 + operator, random.next() & 0xdf | 0x10),
                5 => (0xa0 + operator, random.next() & 0xff),
                6 => (0xc0 + operator, random.next() & 0x1f),
                7 => (0xe0 + operator, random.next() & 0xff),
                8 => (0x10 + channel, random.next() & 0xff | 0x40),
                9 => (0x18 + channel, random.next() & 0xff),
                10 => (0x20 + channel + 8 * random.below(2), random.next() & 0xff),
                11 => (0x30 + channel + 8 * random.below(2), random.next() & 0xff),
                12 => (0x04, random.next() & 0xff),
                13 => match random.below(4) {
                    0 => (0x100 + operator, random.next() & 0xff),
                    1 => (random.below(0x120), random.next() & 0xff),
                    _ => (0x03, random.next() & 0xff),
                },
                14 => {
                    let offset = if random.below(4) == 0 { 1 } else { 0 };
                    let _ = writeln!(text, "{sample} r {offset:x}");
                    continue;
                }
                _ => (0x05, random.next() & 0xff),
            };
            let _ = writeln!(text, "{sample} w {register:x} {value:02x}");
        }
        sample += random.below(1500);
    }
    text
}

#[test]
fn matches_ymfm_on_random_ym3806_logs_when_an_oracle_is_named() {
    common::differential::<Ym3806<Host>>("ym3806", |seed| random_log(seed, "ym3806", 30_000));
}

#[test]
fn matches_ymfm_on_random_ym3533_logs_when_an_oracle_is_named() {
    common::differential::<Ym3533<Host>>("ym3533", |seed| random_log(seed, "ym3533", 10_000));
}
