// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The OPL emulator held to ymfm, sample for sample.
//!
//! Each case in `tests/opl` is a synthetic register log and the render ymfm
//! made of it, through `tests/opl/oracle.cpp`. The logs are written for the
//! purpose, so nothing here is game data: every waveform, the 2-operator and
//! 4-operator connections, feedback, the envelope at several rates with key
//! scaling, both LFOs at both depths, rhythm mode on both chips, OPL3 panning
//! and the NEW bit, key timing, and a full chip loud enough to clip.
//!
//! A mismatch reports the first sample that differs, the writes that led up
//! to it, and the register file at that point, which is usually enough to see
//! which part of the chip it is in.
//!
//! `TECS_OPL_ORACLE`, naming a built oracle, adds a randomized comparison over
//! logs this test writes itself. It is a development aid rather than a gate:
//! the oracle is never built by Cargo, so the test returns at once without it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use tecs_opl::{Chip, Kind};

/// One register log: the chip, the samples to render and the writes.
struct Log {
    kind: Kind,
    samples: usize,
    writes: Vec<(u64, u16, u8)>,
}

fn directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("opl")
}

fn parse(text: &str) -> Log {
    let mut lines = text.lines();
    let header = lines.next().expect("a log has a header");
    let fields: Vec<&str> = header.trim_start_matches('#').split_whitespace().collect();
    let kind = match fields.first().copied() {
        Some("opl2") => Kind::Opl2,
        Some("opl3") => Kind::Opl3,
        other => panic!("unknown chip {other:?}"),
    };
    let samples = fields[1].parse().expect("a sample count");
    let mut writes = Vec::new();
    for line in lines {
        let body = line.split('#').next().unwrap_or("").trim();
        if body.is_empty() {
            continue;
        }
        let mut parts = body.split_whitespace();
        let sample = parts.next().and_then(|v| v.parse().ok()).expect("a sample");
        let register = parts
            .next()
            .and_then(|v| u16::from_str_radix(v, 16).ok())
            .expect("a register");
        let value = parts
            .next()
            .and_then(|v| u8::from_str_radix(v, 16).ok())
            .expect("a value");
        writes.push((sample, register, value));
    }
    Log {
        kind,
        samples,
        writes,
    }
}

fn channels(kind: Kind) -> usize {
    match kind {
        Kind::Opl2 => 1,
        Kind::Opl3 => 2,
    }
}

/// Describes the chip at a mismatch: the last writes and every register that
/// is not zero.
fn describe(chip: &Chip, writes: &[(u64, u16, u8)], applied: usize) -> String {
    let mut text = String::new();
    let from = applied.saturating_sub(12);
    let _ = writeln!(text, "the last writes applied, of {applied}:");
    for (sample, register, value) in &writes[from..applied] {
        let _ = writeln!(text, "    {sample:>7} {register:03x} {value:02x}");
    }
    let _ = write!(text, "nonzero registers:");
    for (register, value) in chip.registers().iter().enumerate() {
        if *value != 0 {
            if register % 0x100 == 0 || text.ends_with(':') {
                let _ = write!(text, "\n   ");
            }
            let _ = write!(text, " {register:03x}={value:02x}");
        }
    }
    text
}

/// Replays a log and compares every sample with `expected`, returning the
/// first mismatch described rather than panicking, so the randomized test can
/// add its seed.
fn compare(log: &Log, expected: &[i16]) -> Result<(), String> {
    let width = channels(log.kind);
    if expected.len() != log.samples * width {
        return Err(format!(
            "the golden holds {} samples and the log asks for {}",
            expected.len() / width,
            log.samples
        ));
    }
    let mut chip = Chip::new(log.kind);
    let mut next = 0;
    for sample in 0..log.samples {
        while next < log.writes.len() && log.writes[next].0 <= sample as u64 {
            chip.write(log.writes[next].1, log.writes[next].2);
            next += 1;
        }
        let (left, right) = chip.generate();
        let got = [left, right];
        for channel in 0..width {
            let want = expected[sample * width + channel];
            if got[channel] != want {
                return Err(format!(
                    "sample {sample}, {} channel: ymfm gives {want} and this gives {}\n{}",
                    ["left", "right"][channel],
                    got[channel],
                    describe(&chip, &log.writes, next)
                ));
            }
        }
    }
    Ok(())
}

fn decode(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect()
}

fn check(name: &str) {
    let base = directory();
    let text = std::fs::read_to_string(base.join(format!("{name}.log"))).expect("the log reads");
    let golden = std::fs::read(base.join(format!("{name}.golden"))).expect("the golden reads");
    let log = parse(&text);
    let expected = decode(&golden);
    // A golden of silence would pass a chip that renders nothing.
    assert!(
        expected.iter().any(|sample| *sample != 0),
        "{name}: the golden is silent"
    );
    if let Err(reason) = compare(&log, &expected) {
        panic!("{name}: {reason}");
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
fn resets_to_the_state_a_new_chip_starts_in() {
    for kind in [Kind::Opl2, Kind::Opl3] {
        let text =
            std::fs::read_to_string(directory().join("opl2-envelope.log")).expect("the log reads");
        let log = parse(&text);
        let mut used = Chip::new(kind);
        for (_, register, value) in &log.writes {
            used.write(*register, *value);
            used.generate();
        }
        used.reset();
        let mut fresh = Chip::new(kind);
        assert_eq!(used.registers(), fresh.registers());
        // The LFOs and noise generator restart too, so a reset chip and a new
        // one render the same notes identically.
        for (_, register, value) in &log.writes {
            used.write(*register, *value);
            fresh.write(*register, *value);
        }
        for _ in 0..20_000 {
            assert_eq!(used.generate(), fresh.generate());
        }
    }
}

/// A small deterministic generator, so a failing seed can be replayed.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }

    fn below(&mut self, bound: u32) -> u32 {
        self.next() % bound
    }
}

/// Writes a random log: bursts of writes to every register class, biased
/// towards key-ons so that something sounds most of the time.
fn random_log(seed: u64, kind: Kind, samples: usize) -> String {
    let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut text = format!(
        "# {} {samples}\n",
        if kind == Kind::Opl2 { "opl2" } else { "opl3" }
    );
    let banks = if kind == Kind::Opl3 { 2 } else { 1 };
    let mut sample = 0u64;
    if kind == Kind::Opl3 && random.below(4) != 0 {
        let _ = writeln!(text, "0 105 01");
    }
    while (sample as usize) < samples {
        let burst = 1 + random.below(24);
        for _ in 0..burst {
            let bank = 0x100 * random.below(banks) as u16;
            let (register, value): (u16, u32) = match random.below(12) {
                0 => (0x20 + random.below(0x16) as u16, random.next() & 0xff),
                1 => (0x40 + random.below(0x16) as u16, random.next() & 0x7f),
                2 => (
                    0x60 + random.below(0x16) as u16,
                    random.next() & 0xff | 0x80,
                ),
                3 => (0x80 + random.below(0x16) as u16, random.next() & 0xff),
                4 => (0xa0 + random.below(9) as u16, random.next() & 0xff),
                5 | 6 => (0xb0 + random.below(9) as u16, random.next() & 0x3f),
                7 => (0xc0 + random.below(9) as u16, random.next() & 0xff),
                8 => (0xe0 + random.below(0x16) as u16, random.next() & 0x07),
                9 => (0xbd, random.next() & 0xff),
                10 => match random.below(4) {
                    0 => (0x01, random.next() & 0x20),
                    1 => (0x08, random.next() & 0x40),
                    2 if kind == Kind::Opl3 => (0x104 - bank, random.next() & 0x3f),
                    _ => (0x04, random.next() & 0xe0),
                },
                _ => (0xb0 + random.below(9) as u16, 0x20 | (random.next() & 0x1f)),
            };
            let register = if register >= 0x100 {
                register
            } else {
                register | bank
            };
            let _ = writeln!(text, "{sample} {register:03x} {value:02x}");
        }
        sample += u64::from(random.below(1500));
    }
    text
}

#[test]
fn matches_ymfm_on_random_logs_when_an_oracle_is_named() {
    let Some(oracle) = std::env::var_os("TECS_OPL_ORACLE") else {
        return;
    };
    let count: u64 = std::env::var("TECS_OPL_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let scratch = std::env::temp_dir().join(format!("tecs-opl-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("a scratch directory");
    let mut failures = Vec::new();
    for seed in 1..=count {
        let kind = if seed % 2 == 0 {
            Kind::Opl3
        } else {
            Kind::Opl2
        };
        let text = random_log(seed, kind, 30_000);
        let path = scratch.join(format!("{seed}.log"));
        std::fs::write(&path, &text).expect("the log writes");
        let output = std::process::Command::new(&oracle)
            .arg(&path)
            .output()
            .expect("the oracle runs");
        assert!(output.status.success(), "the oracle failed on seed {seed}");
        if let Err(reason) = compare(&parse(&text), &decode(&output.stdout)) {
            failures.push(format!("seed {seed}: {reason}"));
        } else {
            std::fs::remove_file(&path).ok();
        }
    }
    if failures.is_empty() {
        std::fs::remove_dir_all(&scratch).ok();
    }
    assert!(
        failures.is_empty(),
        "{} of {count} logs differ, kept in {}:\n{}",
        failures.len(),
        scratch.display(),
        failures.join("\n\n")
    );
}
