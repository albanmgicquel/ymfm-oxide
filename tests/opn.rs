// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The OPN family held to ymfm, sample for sample: so far the YM2612, the
//! YM3438 and the YMF276.
//!
//! Each case in `tests/opn` is a synthetic register log and the render ymfm
//! made of it through `oracle/oracle.cpp`: all eight connections in both
//! banks, feedback, the envelope phases and rates with key scaling, every
//! SSG-EG shape, channel 3's special mode with its frequency latches and
//! CSM, the LFO at every rate and sensitivity with the OPN PM adjustment,
//! detune and multiple across the key codes, the DAC on channel 6, panning,
//! per-operator key-on, a loud chip and one at full clip, past 16 bits, and
//! the timers with status, busy and IRQ. The DAC, loud and clip cases run on
//! all three chips, where their outputs differ: the YM2612's discontinuity ("ladder effect"), the YM3438's
//! 9-bit sum and the YMF276's 14-bit one.
//!
//! With `YMFM_ORACLE` naming a built oracle, a randomized comparison runs as
//! well, over logs this test writes itself (see `common::differential`).

#[macro_use]
mod common;

use std::fmt::Write as _;

use common::{Host, Random};
use ymfm_oxide::opn::{Ym2612, Ym3438, Ymf276};
use ymfm_oxide::Output;

harness_chip!(Ym2612<Host>, 2);
harness_chip!(Ym3438<Host>, 2);
harness_chip!(Ymf276<Host>, 2);

/// Holds the log `name` to its golden, on the chip the log names.
fn check(name: &str) {
    let log = common::read_log("opn", name);
    match log.chip.as_str() {
        "ym2612" => common::check::<Ym2612<Host>>("opn", name),
        "ym3438" => common::check::<Ym3438<Host>>("opn", name),
        "ymf276" => common::check::<Ymf276<Host>>("opn", name),
        other => panic!("{name}: not an OPN chip: {other}"),
    }
}

#[test]
fn matches_ymfm_on_all_eight_algorithms() {
    check("opn2-algorithms");
}

#[test]
fn matches_ymfm_on_feedback() {
    check("opn2-feedback");
}

#[test]
fn matches_ymfm_on_the_envelope() {
    check("opn2-envelopes");
}

#[test]
fn matches_ymfm_on_every_ssg_eg_shape() {
    check("opn2-ssgeg");
}

#[test]
fn matches_ymfm_on_channel_3_special_mode_the_latches_and_csm() {
    check("opn2-ch3");
}

#[test]
fn matches_ymfm_on_the_lfo() {
    check("opn2-lfo");
}

#[test]
fn matches_ymfm_on_detune_and_multiple() {
    check("opn2-detune");
}

#[test]
fn matches_ymfm_on_panning() {
    check("opn2-pan");
}

#[test]
fn matches_ymfm_on_per_operator_key_on() {
    check("opn2-keyon");
}

#[test]
fn matches_ymfm_on_timers_status_busy_and_irq() {
    check("opn2-timers");
}

#[test]
fn matches_ymfm_on_the_dac_on_each_chip() {
    check("opn2-dac");
    check("ym3438-dac");
    check("ymf276-dac");
}

#[test]
fn matches_ymfm_when_every_channel_is_loud_on_each_chip() {
    check("opn2-loud");
    check("ym3438-loud");
    check("ymf276-loud");
}

#[test]
fn matches_ymfm_at_full_clip_on_each_chip() {
    check("opn2-clip");
    check("ym3438-clip");
    check("ymf276-clip");
}

/// Returns the smallest and largest output of a golden.
fn extremes(name: &str) -> (i32, i32) {
    let log = common::read_log("opn", name);
    let golden = std::fs::read(common::directory("opn").join(format!("{name}.golden")))
        .expect("the golden reads");
    let render = common::decode(&golden, 2, log.samples as usize).expect("the golden decodes");
    let low = render.samples.iter().copied().min().unwrap();
    let high = render.samples.iter().copied().max().unwrap();
    (low, high)
}

#[test]
fn reaches_past_16_bits_at_full_clip_as_ymfm_does() {
    // what the clip logs hold, and so what the chips are held to: the
    // YM2612's six clipped channels sum to +32768, the YM3438's to -32896
    // and +32768, and the YMF276 clamps its 14-bit sum inside 16 bits
    for name in ["opn2-clip", "opn2-loud"] {
        assert_eq!(extremes(name), (-32768, 32768), "{name}");
    }
    for name in ["ym3438-clip", "ym3438-loud"] {
        assert_eq!(extremes(name), (-32896, 32768), "{name}");
    }
    let (low, high) = extremes("ymf276-clip");
    assert!(low >= -32768 && high <= 32767);
}

#[test]
fn resets_to_the_state_a_new_chip_starts_in() {
    let log = common::read_log("opn", "opn2-ch3");
    let writes: Vec<(u32, u8)> = log
        .commands
        .iter()
        .flat_map(|command| match &command.action {
            common::Action::Write(pairs) => pairs.clone(),
            _ => Vec::new(),
        })
        .collect();
    let mut used = Ym2612::new(Host::new());
    let mut out = [Output::<2>::default()];
    for (offset, data) in &writes {
        used.write(*offset, *data);
        used.generate(&mut out);
    }
    used.reset();
    let mut fresh = Ym2612::new(Host::new());
    assert_eq!(used.registers(), fresh.registers());
    for (offset, data) in &writes {
        used.write(*offset, *data);
        fresh.write(*offset, *data);
    }
    let mut a = [Output::<2>::default()];
    let mut b = [Output::<2>::default()];
    for _ in 0..20_000 {
        used.generate(&mut a);
        fresh.generate(&mut b);
        assert_eq!(a, b);
    }
}

#[test]
fn runs_at_the_clock_over_144() {
    let chip = Ym2612::new(Host::new());
    assert_eq!(chip.sample_rate(7_670_453), 53_267);
    assert_eq!(Ym3438::new(Host::new()).sample_rate(7_670_453), 53_267);
    assert_eq!(Ymf276::new(Host::new()).sample_rate(7_670_453), 53_267);
}

/// Writes a random OPN2 log: bursts of writes to every register in both
/// banks, biased towards key-ons and audible levels, with the frequency
/// latches, channel 3's modes, the timers and CSM, the LFO, the DAC,
/// mismatched address and data ports, and status reads mixed in. Every
/// channel can reach both outputs, so the logs clip as far as the chips do.
fn random_log(seed: u64, chip: &str, samples: u32) -> String {
    let mut random = Random::new(seed);
    let mut text = format!("chip {chip}\nclock 7670453\nsamples {samples}\n");
    let mut sample = 0u32;
    while sample < samples {
        let burst = 1 + random.below(32);
        for _ in 0..burst {
            let bank = 0x100 * random.below(2);
            let operator = random.below(16);
            let channel = random.below(3);
            let (register, value): (u32, u32) = match random.below(24) {
                0 => (bank | (0x30 + operator), random.next() & 0x7f),
                1 | 2 => (bank | (0x40 + operator), random.next() & 0x3f),
                3 => (bank | (0x50 + operator), random.next() & 0xdf | 0x10),
                4 => (bank | (0x60 + operator), random.next() & 0x9f),
                5 => (bank | (0x70 + operator), random.next() & 0x1f),
                6 => (bank | (0x80 + operator), random.next() & 0xff),
                7 => (bank | (0x90 + operator), random.next() & 0x0f),
                8 => (bank | (0xa4 + channel), random.next() & 0x3f),
                9 => (bank | (0xa0 + channel), random.next() & 0xff),
                10 => (bank | (0xa8 + random.below(8)), random.next() & 0xff),
                11 => (bank | (0xb0 + channel), random.next() & 0x3f),
                12 => (bank | (0xb4 + channel), random.next() & 0xff),
                13 => (0x22, random.next() & 0x0f),
                14 => match random.below(5) {
                    0 => (0x24 + random.below(3), random.next() & 0xff),
                    1 => (0x27, random.next() & 0xff),
                    2 => (0x2b, random.next() & 0x80),
                    3 => (0x2c, random.next() & 0x08),
                    _ => (0x21 + random.below(0xdf), random.next() & 0xff),
                },
                15 | 16 => (0x2a, random.next() & 0xff),
                17 => {
                    let _ = writeln!(text, "{sample} r {:x}", random.below(4));
                    continue;
                }
                18 => {
                    // an address on one port and data on the other
                    let high = random.below(2);
                    let _ = writeln!(
                        text,
                        "{sample} w {:x} {:02x} {:x} {:02x}",
                        2 * high,
                        0x30 + random.below(0x90),
                        3 - 2 * high,
                        random.next() & 0xff
                    );
                    continue;
                }
                _ => (0x28, random.next() & 0xf7),
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
fn matches_ymfm_on_random_ym2612_logs_when_an_oracle_is_named() {
    common::differential::<Ym2612<Host>>("ym2612", |seed| random_log(seed, "ym2612", 30_000));
}

#[test]
fn matches_ymfm_on_random_ym3438_logs_when_an_oracle_is_named() {
    common::differential::<Ym3438<Host>>("ym3438", |seed| random_log(seed, "ym3438", 30_000));
}

#[test]
fn matches_ymfm_on_random_ymf276_logs_when_an_oracle_is_named() {
    common::differential::<Ymf276<Host>>("ymf276", |seed| random_log(seed, "ymf276", 30_000));
}
