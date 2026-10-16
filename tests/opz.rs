// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The OPZ held to ymfm, sample for sample.
//!
//! Each case in `tests/opz` is a synthetic register log and the render ymfm
//! made of it through `oracle/oracle.cpp`: all eight connections, feedback,
//! the envelope phases and rates, the reverb stage at every rate, the
//! envelope shift (and its absence on operator 1), the eight operator
//! waveforms, fixed-frequency operators with their substep, detune, detune
//! 2, the multiple and the fine field across every key code, both LFOs at
//! every waveform and sensitivity, alone and added, with their sync bits,
//! the noise generator, panning, the preset store and restore, key-on
//! through the channel select, a clipping full chip, the timers with
//! status, busy, IRQ and CSM, and the CT pins.
//!
//! With `YMFM_ORACLE` naming a built oracle, a randomized comparison runs as
//! well, over logs this test writes itself (see `common::differential`).

#[macro_use]
mod common;

use std::fmt::Write as _;

use common::{Host, Random};
use ymfm_oxide::opz::Ym2414;

harness_chip!(Ym2414<Host>, 2);

/// Holds the log `name` to its golden.
fn check(name: &str) {
    let log = common::read_log("opz", name);
    assert_eq!(log.chip, "ym2414", "{name}: not an OPZ log");
    common::check::<Ym2414<Host>>("opz", name);
}

#[test]
fn matches_ymfm_on_all_eight_algorithms() {
    check("opz-algorithms");
}

#[test]
fn matches_ymfm_on_feedback() {
    check("opz-feedback");
}

#[test]
fn matches_ymfm_on_the_envelope() {
    check("opz-envelopes");
}

#[test]
fn matches_ymfm_on_the_reverb_stage() {
    check("opz-reverb");
}

#[test]
fn matches_ymfm_on_the_envelope_shift() {
    check("opz-egshift");
}

#[test]
fn matches_ymfm_on_the_eight_waveforms() {
    check("opz-waveforms");
}

#[test]
fn matches_ymfm_on_fixed_frequency_operators() {
    check("opz-fixed");
}

#[test]
fn matches_ymfm_on_detune_multiple_fine_and_every_key_code() {
    check("opz-detune");
}

#[test]
fn matches_ymfm_on_both_lfos() {
    check("opz-lfo");
}

#[test]
fn matches_ymfm_on_noise() {
    check("opz-noise");
}

#[test]
fn matches_ymfm_on_panning() {
    check("opz-pan");
}

#[test]
fn matches_ymfm_on_key_on() {
    check("opz-keyon");
}

#[test]
fn matches_ymfm_on_presets() {
    check("opz-presets");
}

#[test]
fn matches_ymfm_when_every_channel_is_loud() {
    check("opz-loud");
}

#[test]
fn matches_ymfm_on_timers_status_busy_irq_and_csm() {
    check("opz-timers");
}

#[test]
fn matches_ymfm_on_the_ct_pins() {
    check("opz-ct");
}

/// Writes a random OPZ log: bursts of writes to every register, normal and
/// alternate, biased towards key-ons and audible levels, with the channel
/// select, presets, both LFOs, fixed frequency, the timers, CSM, noise and
/// status reads mixed in.
fn random_log(seed: u64, samples: u32) -> String {
    let mut random = Random::new(seed);
    let mut text = format!("chip ym2414\nclock 3579545\nsamples {samples}\n");
    let mut sample = 0u32;
    while sample < samples {
        let burst = 1 + random.below(32);
        for _ in 0..burst {
            let operator = random.below(32);
            let channel = random.below(8);
            let (register, value) = match random.below(26) {
                0 => (0x40 + operator, random.next() & 0x7f),
                1 => (0x40 + operator, random.next() | 0x80),
                2 | 3 => (0x60 + operator, random.next() & 0x3f),
                4 => (0x80 + operator, random.next() & 0xff | 0x10),
                5 => (0xa0 + operator, random.next() & 0x9f),
                6 => (0xc0 + operator, random.next() & 0xdf),
                7 => (0xc0 + operator, random.next() | 0x20),
                8 => (0xe0 + operator, random.next() & 0xff),
                9 => (0x28 + channel, random.next() & 0x7f),
                10 => (0x30 + channel, random.next() & 0xff),
                11 => (0x38 + channel, random.next() & 0xff),
                12 => (0x16 + 2 * random.below(2), random.next() & 0xff),
                13 => (0x17 + 2 * random.below(2), random.next() & 0xff),
                14 => (0x1b, random.next() & 0xff),
                15 => match random.below(6) {
                    0 => (0x0f, random.next() & 0x9f),
                    1 => (channel, random.next() & 0xff),
                    2 => (0x10 + random.below(3), random.next() & 0xff),
                    3 => (random.below(0x100), random.next() & 0xff),
                    _ => (0x14, random.next() & 0xff),
                },
                16 => {
                    let _ = writeln!(text, "{sample} r {:x}", random.below(2));
                    continue;
                }
                17..=19 => (0x08, channel),
                20 => (0x20 + channel, random.next() & 0xff),
                _ => (0x20 + channel, random.next() & 0xbf | 0x40 | 0x80),
            };
            let _ = writeln!(text, "{sample} w 0 {register:02x} 1 {value:02x}");
        }
        sample += random.below(1500);
    }
    text
}

#[test]
fn matches_ymfm_on_random_ym2414_logs_when_an_oracle_is_named() {
    common::differential::<Ym2414<Host>>("ym2414", |seed| random_log(seed, 20_000));
}
