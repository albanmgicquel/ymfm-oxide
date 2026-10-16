// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The OPLL family held to ymfm, sample for sample.
//!
//! Each case in `tests/opll` is a synthetic register log and the render ymfm
//! made of it through `oracle/oracle.cpp`. The logs are written for the
//! purpose, so nothing here is game data: the built-in instrument set of
//! each of the four variants with its rhythm sounds, the user instrument
//! field by field, the DP ("depress") stage on retriggered notes, rhythm
//! mode at every volume and over melodic notes, the sustain bit against
//! both EG types, key scale rate and level, both LFOs, a full chip on both
//! outputs, and the rest of the register interface.
//!
//! With `YMFM_ORACLE` naming a built oracle, a randomized comparison runs as
//! well, over logs this test writes itself (see `common::differential`).

#[macro_use]
mod common;

use std::fmt::Write as _;

use common::{Host, Random};
use ymfm_oxide::opl::{Ds1001, OpllBase, Ym2413, Ym2423, Ymf281};
use ymfm_oxide::Output;

harness_chip!(Ym2413<Host>, 2);
harness_chip!(Ym2423<Host>, 2);
harness_chip!(Ymf281<Host>, 2);
harness_chip!(Ds1001<Host>, 2);

/// Holds the log `name` to its golden, on the chip the log names.
fn check(name: &str) {
    let log = common::read_log("opll", name);
    match log.chip.as_str() {
        "ym2413" => common::check::<Ym2413<Host>>("opll", name),
        "ym2423" => common::check::<Ym2423<Host>>("opll", name),
        "ymf281" => common::check::<Ymf281<Host>>("opll", name),
        "ds1001" => common::check::<Ds1001<Host>>("opll", name),
        other => panic!("{name}: not an OPLL chip: {other}"),
    }
}

#[test]
fn matches_ymfm_on_the_ym2413_instruments() {
    check("ym2413-instruments");
}

#[test]
fn matches_ymfm_on_the_ym2423_instruments() {
    check("ym2423-instruments");
}

#[test]
fn matches_ymfm_on_the_ymf281_instruments() {
    check("ymf281-instruments");
}

#[test]
fn matches_ymfm_on_the_ds1001_instruments() {
    check("ds1001-instruments");
}

#[test]
fn matches_ymfm_on_the_user_instrument() {
    check("opll-custom");
}

#[test]
fn matches_ymfm_on_the_depress_stage() {
    check("opll-depress");
}

#[test]
fn matches_ymfm_on_rhythm_mode() {
    check("opll-rhythm");
}

#[test]
fn matches_ymfm_on_sustain() {
    check("opll-sustain");
}

#[test]
fn matches_ymfm_on_key_scale_rate_and_level() {
    check("opll-keyscale");
}

#[test]
fn matches_ymfm_on_tremolo_and_vibrato() {
    check("opll-lfo");
}

#[test]
fn matches_ymfm_when_every_channel_is_loud() {
    check("opll-loud");
}

#[test]
fn matches_ymfm_on_the_rest_of_the_register_interface() {
    check("opll-misc");
}

/// The writes of the log `name`, in order.
fn writes_of(name: &str) -> Vec<(u32, u8)> {
    common::read_log("opll", name)
        .commands
        .iter()
        .flat_map(|command| match &command.action {
            common::Action::Write(pairs) => pairs.clone(),
            _ => Vec::new(),
        })
        .collect()
}

/// Plays `writes` through a chip, one write a sample, then 4000 samples
/// more, and returns every output.
fn play<C: common::Chip>(chip: &mut C, writes: &[(u32, u8)]) -> Vec<i32> {
    let mut out = Vec::new();
    let mut sample = [0i32; 2];
    for index in 0..writes.len() + 4000 {
        if let Some((offset, data)) = writes.get(index) {
            chip.write(*offset, *data);
        }
        chip.generate(&mut sample);
        out.extend_from_slice(&sample);
    }
    out
}

#[test]
fn plays_a_variant_given_its_instrument_set() {
    // A YM2413 given the DS1001's set plays as a DS1001, and so does one
    // switched to it once its caches are refreshed, and the bare base.
    let writes = writes_of("ds1001-instruments");
    let set = Ds1001::<Host>::DEFAULT_INSTRUMENTS;
    let want = play(&mut Ds1001::new(Host::new()), &writes);
    assert!(want.iter().any(|value| *value != 0));
    assert_eq!(
        play(&mut Ym2413::with_instruments(Host::new(), set), &writes),
        want
    );
    let mut switched = Ym2413::new(Host::new());
    switched.set_instrument_data(set);
    switched.invalidate_caches();
    assert_eq!(play(&mut switched, &writes), want);

    let mut base = OpllBase::new(Host::new(), set);
    let mut got = Vec::new();
    for index in 0..writes.len() + 4000 {
        if let Some((offset, data)) = writes.get(index) {
            base.write(*offset, *data);
        }
        let mut out = [Output::<2>::default()];
        base.generate(&mut out);
        got.extend_from_slice(&out[0].data);
    }
    assert_eq!(got, want);
}

#[test]
fn has_the_variants_own_instrument_sets() {
    let sets = [
        Ym2413::<Host>::DEFAULT_INSTRUMENTS,
        Ym2423::<Host>::DEFAULT_INSTRUMENTS,
        Ymf281::<Host>::DEFAULT_INSTRUMENTS,
        Ds1001::<Host>::DEFAULT_INSTRUMENTS,
    ];
    for (index, set) in sets.iter().enumerate() {
        for other in &sets[index + 1..] {
            // the melodic instruments differ, the rhythm ones are shared
            assert_ne!(set[..0x78], other[..0x78]);
            assert_eq!(set[0x78..], other[0x78..]);
        }
    }
    assert_eq!(Ym2413::new(Host::new()).sample_rate(3_579_545), 49_715);
}

#[test]
fn resets_to_the_state_a_new_chip_starts_in() {
    let writes = writes_of("opll-rhythm");
    let mut used = Ym2413::new(Host::new());
    let mut out = [Output::<2>::default()];
    for (offset, data) in &writes {
        used.write(*offset, *data);
        used.generate(&mut out);
    }
    used.reset();
    let mut fresh = Ym2413::new(Host::new());
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

/// Writes a random OPLL log: bursts of writes to every register class,
/// biased towards key-ons so that something sounds most of the time, with
/// rhythm mode, the sustain bit, stray registers and reads mixed in.
fn random_log(seed: u64, chip: &str, samples: u32) -> String {
    let mut random = Random::new(seed);
    let mut text = format!("chip {chip}\nclock 3579545\nsamples {samples}\n");
    let mut sample = 0u32;
    while sample < samples {
        let burst = 1 + random.below(24);
        for _ in 0..burst {
            let (register, value): (u32, u32) = match random.below(14) {
                0..=2 => (random.below(8), random.next() & 0xff),
                3 => (0x10 + random.below(9), random.next() & 0xff),
                4 | 5 => (0x20 + random.below(9), random.next() & 0x3f),
                6 => (0x20 + random.below(9), 0x10 | (random.next() & 0x2f)),
                7 | 8 => (0x30 + random.below(9), random.next() & 0xff),
                9 => (
                    0x0e,
                    if random.below(3) == 0 {
                        random.next() & 0x1f
                    } else {
                        0x20 | (random.next() & 0x1f)
                    },
                ),
                10 => match random.below(5) {
                    0 => (0x0f, 0),
                    1 => (0x3f, random.next() & 0xff),
                    2 => (0x40 + random.below(0xc0), random.next() & 0xff),
                    3 => (
                        0x19 + random.below(7) + 0x10 * random.below(3),
                        random.next() & 0xff,
                    ),
                    _ => (0x08 + random.below(6), random.next() & 0xff),
                },
                11 => {
                    let _ = writeln!(text, "{sample} r {:x}", random.below(2));
                    continue;
                }
                _ => (0x20 + random.below(9), 0x10 | (random.next() & 0x2f)),
            };
            let _ = writeln!(text, "{sample} w 0 {register:02x} 1 {value:02x}");
        }
        sample += random.below(1500);
    }
    text
}

#[test]
fn matches_ymfm_on_random_ym2413_logs_when_an_oracle_is_named() {
    common::differential::<Ym2413<Host>>("ym2413", |seed| random_log(seed, "ym2413", 20_000));
}

#[test]
fn matches_ymfm_on_random_ym2423_logs_when_an_oracle_is_named() {
    common::differential::<Ym2423<Host>>("ym2423", |seed| random_log(seed, "ym2423", 20_000));
}

#[test]
fn matches_ymfm_on_random_ymf281_logs_when_an_oracle_is_named() {
    common::differential::<Ymf281<Host>>("ymf281", |seed| random_log(seed, "ymf281", 20_000));
}

#[test]
fn matches_ymfm_on_random_ds1001_logs_when_an_oracle_is_named() {
    common::differential::<Ds1001<Host>>("ds1001", |seed| random_log(seed, "ds1001", 20_000));
}
