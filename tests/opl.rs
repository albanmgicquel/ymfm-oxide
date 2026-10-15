// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The OPL family held to ymfm, sample for sample.
//!
//! Each case in `tests/opl` is a synthetic register log and the render ymfm
//! made of it through `oracle/oracle.cpp`. The logs are written for the
//! purpose, so nothing here is game data: every waveform, the 2-operator and
//! 4-operator connections, feedback, the envelope at several rates with key
//! scaling, both LFOs at both depths, rhythm mode on both chips, OPL3 panning
//! and the NEW bit, key timing, a full chip loud enough to clip, and the
//! timers with their status bits, masks, IRQ line and CSM key-ons.
//!
//! With `YMFM_ORACLE` naming a built oracle, a randomized comparison runs as
//! well, over logs this test writes itself (see `common::differential`).

#[macro_use]
mod common;

use std::fmt::Write as _;

use common::{Host, Random};
use ymfm_oxide::opl::{Ym3526, Ym3812, Ymf262};
use ymfm_oxide::Output;

harness_chip!(Ym3526<Host>, 1);
harness_chip!(Ym3812<Host>, 1);
harness_chip!(Ymf262<Host>, 4);

/// Holds the log `name` to its golden, on the chip the log names.
fn check(name: &str) {
    let log = common::read_log("opl", name);
    match log.chip.as_str() {
        "ym3526" => common::check::<Ym3526<Host>>("opl", name),
        "ym3812" => common::check::<Ym3812<Host>>("opl", name),
        "ymf262" => common::check::<Ymf262<Host>>("opl", name),
        other => panic!("{name}: not an OPL chip: {other}"),
    }
}

#[test]
fn matches_ymfm_on_every_opl2_waveform() {
    check("opl2-waveforms");
}

#[test]
fn matches_ymfm_on_every_opl3_waveform() {
    check("opl3-waveforms");
}

#[test]
fn matches_ymfm_on_two_operator_connections_and_feedback() {
    check("opl2-feedback");
}

#[test]
fn matches_ymfm_on_the_four_operator_connections() {
    check("opl3-fourop");
}

#[test]
fn matches_ymfm_on_attack_decay_and_sustain() {
    check("opl2-envelope");
}

#[test]
fn matches_ymfm_on_release_rates() {
    check("opl2-release");
}

#[test]
fn matches_ymfm_on_key_scale_rate_and_level() {
    check("opl2-keyscale");
}

#[test]
fn matches_ymfm_on_tremolo_and_vibrato_at_both_depths() {
    check("opl2-lfo");
}

#[test]
fn matches_ymfm_on_opl2_rhythm_mode() {
    check("opl2-rhythm");
}

#[test]
fn matches_ymfm_on_opl3_rhythm_mode_beside_the_second_bank() {
    check("opl3-rhythm");
}

#[test]
fn matches_ymfm_on_opl3_panning_and_the_new_bit() {
    check("opl3-pan");
}

#[test]
fn matches_ymfm_on_key_timing() {
    check("opl2-keytiming");
}

#[test]
fn matches_ymfm_when_every_channel_is_loud() {
    check("opl2-loud");
    check("opl3-loud");
}

#[test]
fn matches_ymfm_on_the_ym3526() {
    check("opl-ym3526");
}

#[test]
fn matches_ymfm_on_opl2_timers_status_and_csm() {
    check("opl2-timers");
}

#[test]
fn matches_ymfm_on_opl3_timers_and_status() {
    check("opl3-timers");
}

#[test]
fn resets_to_the_state_a_new_chip_starts_in() {
    let log = common::read_log("opl", "opl2-envelope");
    let writes: Vec<(u32, u8)> = log
        .commands
        .iter()
        .flat_map(|command| match &command.action {
            common::Action::Write(pairs) => pairs.clone(),
            common::Action::Read(_) => Vec::new(),
        })
        .collect();
    let mut used = Ymf262::new(Host::new());
    let mut out = [Output::<4>::default()];
    for (offset, data) in &writes {
        used.write(*offset, *data);
        used.generate(&mut out);
    }
    used.reset();
    let mut fresh = Ymf262::new(Host::new());
    assert_eq!(used.registers(), fresh.registers());
    for (offset, data) in &writes {
        used.write(*offset, *data);
        fresh.write(*offset, *data);
    }
    let mut a = [Output::<4>::default()];
    let mut b = [Output::<4>::default()];
    for _ in 0..20_000 {
        used.generate(&mut a);
        fresh.generate(&mut b);
        assert_eq!(a, b);
    }
}

/// Writes a random OPL log: bursts of writes to every register class, biased
/// towards key-ons so that something sounds most of the time, with the
/// timers and status reads mixed in.
fn random_log(seed: u64, chip: &str, samples: u32) -> String {
    let mut random = Random::new(seed);
    let opl3 = chip == "ymf262";
    let clock = if opl3 { 14_318_180 } else { 3_579_545 };
    let mut text = format!("chip {chip}\nclock {clock}\nsamples {samples}\n");
    let banks = if opl3 { 2 } else { 1 };
    let mut sample = 0u32;
    if opl3 && random.below(4) != 0 {
        let _ = writeln!(text, "0 w 2 05 3 01");
    }
    while sample < samples {
        let burst = 1 + random.below(24);
        for _ in 0..burst {
            let bank = 0x100 * random.below(banks);
            let (register, value): (u32, u32) = match random.below(14) {
                0 => (0x20 + random.below(0x16), random.next() & 0xff),
                1 => (0x40 + random.below(0x16), random.next() & 0x7f),
                2 => (0x60 + random.below(0x16), random.next() & 0xff | 0x80),
                3 => (0x80 + random.below(0x16), random.next() & 0xff),
                4 => (0xa0 + random.below(9), random.next() & 0xff),
                5 | 6 => (0xb0 + random.below(9), random.next() & 0x3f),
                7 => (0xc0 + random.below(9), random.next() & 0xff),
                8 => (0xe0 + random.below(0x16), random.next() & 0x07),
                9 => (0xbd, random.next() & 0xff),
                10 => match random.below(4) {
                    0 => (0x01, random.next() & 0x20),
                    1 => (0x08, random.next() & 0xc0),
                    2 if opl3 => (0x104 - bank, random.next() & 0x3f),
                    _ => (0x04, random.next() & 0xe3),
                },
                11 => (0x02 + random.below(2), random.next() & 0xff),
                12 => {
                    let _ = writeln!(text, "{sample} r 0");
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
        }
        sample += random.below(1500);
    }
    text
}

#[test]
fn matches_ymfm_on_random_ym3526_logs_when_an_oracle_is_named() {
    common::differential::<Ym3526<Host>>("ym3526", |seed| random_log(seed, "ym3526", 20_000));
}

#[test]
fn matches_ymfm_on_random_ym3812_logs_when_an_oracle_is_named() {
    common::differential::<Ym3812<Host>>("ym3812", |seed| random_log(seed, "ym3812", 30_000));
}

#[test]
fn matches_ymfm_on_random_ymf262_logs_when_an_oracle_is_named() {
    common::differential::<Ymf262<Host>>("ymf262", |seed| random_log(seed, "ymf262", 30_000));
}
