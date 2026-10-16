// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The OPN chips that combine the FM engine with an SSG and ADPCM, held to
//! ymfm sample for sample: the YM2203, YM2608, YMF288, YM2610 and YM2610B.
//!
//! The cases in `tests/opn` named after these chips are synthetic register
//! logs with the renders ymfm made of them through `oracle/oracle.cpp`.
//! Together they reach every part of the chips: the FM channels each chip
//! has (and the ones it masks), the SSG at every prescale and fidelity
//! through each of the resampler's ratios, the prescale writes, the
//! fidelity changed as the chip runs, the status registers, timers, IRQ
//! line and its enables and flag controls, busy times, the SSG's I/O ports,
//! an SSG override, the rhythm ROM of the YM2608 and YMF288, ADPCM-A from
//! external memory on the YM2610, ADPCM-B from memory with its status, its
//! read path and recording, the YMF288's mode and readable registers, and
//! the ID codes.
//!
//! The memory images are synthetic, built by this test from the encoders in
//! `common::synth`; `images_are_the_synthetic_ones` checks the committed
//! files against them, and writes them again when `YMFM_WRITE_IMAGES` is
//! set. No ROM data from any real chip is involved: the YM2608's and
//! YMF288's rhythm ROM, which ymfm reads through the interface, is an image
//! of encoded waveforms of the same layout.
//!
//! With `YMFM_ORACLE` naming a built oracle, randomized comparisons run as
//! well, on every chip, with and without the override (see
//! `common::differential`).

#[macro_use]
mod common;

use std::fmt::Write as _;
use std::path::PathBuf;

use common::synth::{chirp, encode_adpcm_a, encode_adpcm_b, noise, ramp, sine, square};
use common::{Host, Random, Recorder};
use ymfm_oxide::opn::{OpnFidelity, Ym2203, Ym2608, Ym2610, Ym2610b, Ymf288};
use ymfm_oxide::{NullInterface, Output};

harness_chip!(Ym2203<Host>, 4, fidelity);
harness_chip!(Ym2608<Host>, 3, fidelity);
harness_chip!(Ymf288<Host>, 3, fidelity);
harness_chip!(Ym2610<Host>, 3, fidelity);
harness_chip!(Ym2610b<Host>, 3, fidelity);
harness_chip!(Ym2203<Host, Recorder>, 4, fidelity override);
harness_chip!(Ym2608<Host, Recorder>, 3, fidelity override);
harness_chip!(Ymf288<Host, Recorder>, 3, fidelity override);
harness_chip!(Ym2610<Host, Recorder>, 3, fidelity override);
harness_chip!(Ym2610b<Host, Recorder>, 3, fidelity override);

//*********************************************************
//  SYNTHETIC IMAGES
//*********************************************************

/// A stand-in for the rhythm ROM of the YM2608 and YMF288, 8 KB in the
/// layout ymfm gives it (bass drum, snare, top cymbal, high hat, tom, rim
/// shot), each sound encoded as ADPCM-A from its own synthetic waveform.
fn rhythm_image() -> Vec<u8> {
    let decaying = |samples: Vec<i32>| -> Vec<i32> {
        let count = samples.len() as i32;
        samples
            .iter()
            .enumerate()
            .map(|(n, value)| value * (count - n as i32) / count)
            .collect()
    };
    let mut image = Vec::new();
    // each region holds two nibbles per byte
    image.extend(encode_adpcm_a(&decaying(sine(0x1c0 * 2, 90.0, 1900.0))));
    image.extend(encode_adpcm_a(&decaying(square(0x280 * 2, 7, 1200))));
    image.extend(encode_adpcm_a(&decaying(chirp(0x1740 * 2, 1500.0))));
    image.extend(encode_adpcm_a(&decaying(square(0x180 * 2, 3, 900))));
    image.extend(encode_adpcm_a(&decaying(sine(0x280 * 2, 40.0, 1700.0))));
    image.extend(encode_adpcm_a(&decaying(ramp(0x80 * 2, 9, 1400))));
    assert_eq!(image.len(), 0x2000);
    image
}

/// The YM2610's ADPCM-A image, 16 KB in 256-byte units: six 2 KB sounds,
/// one per channel, then 4 KB of noise.
fn ym2610_a_image() -> Vec<u8> {
    let mut image = Vec::new();
    image.extend(encode_adpcm_a(&sine(4096, 50.0, 1800.0)));
    image.extend(encode_adpcm_a(&ramp(4096, 70, 1500)));
    image.extend(encode_adpcm_a(&square(4096, 90, 1100)));
    image.extend(encode_adpcm_a(&chirp(4096, 1900.0)));
    image.extend(encode_adpcm_a(&sine(4096, 17.0, 700.0)));
    image.extend(encode_adpcm_a(&sine(4096, 400.0, 2000.0)));
    image.extend(noise(4096, 2610));
    assert_eq!(image.len(), 0x4000);
    image
}

/// The ADPCM-B image, 16 KB: a sine, a ramp, a square loud enough to clamp
/// and a chirp, 4 KB each.
fn adpcm_b_image() -> Vec<u8> {
    let mut image = Vec::new();
    image.extend(encode_adpcm_b(&sine(8192, 80.0, 20000.0)));
    image.extend(encode_adpcm_b(&ramp(8192, 120, 22000)));
    image.extend(encode_adpcm_b(&square(8192, 200, 40000)));
    image.extend(encode_adpcm_b(&chirp(8192, 30000.0)));
    assert_eq!(image.len(), 0x4000);
    image
}

/// The images, by file name.
fn images() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("opna-rhythm.bin", rhythm_image()),
        ("ym2610-a.bin", ym2610_a_image()),
        ("opn-b.bin", adpcm_b_image()),
        ("opn-io.bin", vec![0x3c, 0xa5]),
    ]
}

#[test]
fn images_are_the_synthetic_ones() {
    let base = common::directory("opn");
    let write = std::env::var_os("YMFM_WRITE_IMAGES").is_some();
    for (name, image) in images() {
        let path = base.join(name);
        if write {
            std::fs::write(&path, &image).expect("the image writes");
        }
        let committed = std::fs::read(&path).expect("the image reads");
        assert!(committed == image, "{name} is not the synthetic image");
    }
}

//*********************************************************
//  GOLDEN RENDERS
//*********************************************************

/// Holds the log `name` to its golden, on the chip and with the override
/// the log names.
fn check(name: &str) {
    let log = common::read_log("opn", name);
    match (log.chip.as_str(), log.ssg_override) {
        ("ym2203", false) => common::check::<Ym2203<Host>>("opn", name),
        ("ym2608", false) => common::check::<Ym2608<Host>>("opn", name),
        ("ymf288", false) => common::check::<Ymf288<Host>>("opn", name),
        ("ym2610", false) => common::check::<Ym2610<Host>>("opn", name),
        ("ym2610b", false) => common::check::<Ym2610b<Host>>("opn", name),
        ("ym2203", true) => common::check::<Ym2203<Host, Recorder>>("opn", name),
        ("ym2608", true) => common::check::<Ym2608<Host, Recorder>>("opn", name),
        ("ymf288", true) => common::check::<Ymf288<Host, Recorder>>("opn", name),
        ("ym2610", true) => common::check::<Ym2610<Host, Recorder>>("opn", name),
        ("ym2610b", true) => common::check::<Ym2610b<Host, Recorder>>("opn", name),
        (other, _) => panic!("{name}: not a combination chip: {other}"),
    }
}

#[test]
fn ym2203_matches_ymfm_on_fm_through_its_dac() {
    check("ym2203-fm");
}

#[test]
fn ym2203_matches_ymfm_on_the_ssg_at_every_prescale_and_fidelity() {
    check("ym2203-ssg-max");
    check("ym2203-ssg-med");
    check("ym2203-ssg-min");
}

#[test]
fn ym2203_matches_ymfm_on_the_fidelity_changed_while_it_runs() {
    check("ym2203-fidelity");
}

#[test]
fn ym2203_matches_ymfm_on_timers_status_busy_csm_and_irq() {
    check("ym2203-timers");
}

#[test]
fn ym2203_matches_ymfm_on_the_io_ports_and_read_back() {
    check("ym2203-io");
}

#[test]
fn ym2203_matches_ymfm_with_an_ssg_override() {
    check("ym2203-override");
}

#[test]
fn ym2608_matches_ymfm_on_six_fm_channels_the_lfo_and_panning() {
    check("ym2608-fm");
}

#[test]
fn ym2608_matches_ymfm_on_the_rhythm_rom() {
    check("ym2608-rhythm");
}

#[test]
fn ym2608_matches_ymfm_on_adpcm_b_status_flags_and_irqs() {
    check("ym2608-adpcmb");
}

#[test]
fn ym2608_matches_ymfm_on_adpcm_b_reads_and_recording_to_memory() {
    check("ym2608-memory");
}

#[test]
fn ym2608_matches_ymfm_on_the_ssg_prescales_and_fidelities() {
    check("ym2608-ssg");
}

#[test]
fn ym2608_matches_ymfm_on_timers_and_irq_controls() {
    check("ym2608-timers");
}

#[test]
fn ym2608_matches_ymfm_with_an_ssg_override() {
    check("ym2608-override");
}

#[test]
fn ymf288_matches_ymfm_on_fm_ssg_and_rhythm() {
    check("ymf288-mix");
}

#[test]
fn ymf288_matches_ymfm_in_ymf288_mode() {
    check("ymf288-mode");
}

#[test]
fn ymf288_matches_ymfm_on_timers_and_irq_controls() {
    check("ymf288-timers");
}

#[test]
fn ymf288_matches_ymfm_with_an_ssg_override() {
    check("ymf288-override");
}

#[test]
fn ym2610_matches_ymfm_on_its_four_fm_channels() {
    check("ym2610-fm");
}

#[test]
fn ym2610_matches_ymfm_on_adpcm_a_from_memory_and_its_flags() {
    check("ym2610-adpcma");
}

#[test]
fn ym2610_matches_ymfm_on_adpcm_b_and_its_end_flag() {
    check("ym2610-adpcmb");
}

#[test]
fn ym2610_matches_ymfm_on_the_ssg_and_timers() {
    check("ym2610-ssg");
}

#[test]
fn ym2610_matches_ymfm_with_an_ssg_override() {
    check("ym2610-override");
}

#[test]
fn ym2610b_matches_ymfm_on_all_six_fm_channels_and_adpcm() {
    check("ym2610b-mix");
}

//*********************************************************
//  RATES, RESETS, OVERRIDES
//*********************************************************

#[test]
fn runs_at_the_rate_of_its_fidelity() {
    let mut chip = Ym2203::new(NullInterface);
    assert_eq!(chip.sample_rate(4_000_000), 1_000_000);
    chip.set_fidelity(OpnFidelity::Med);
    assert_eq!(chip.sample_rate(4_000_000), 333_333);
    chip.set_fidelity(OpnFidelity::Min);
    assert_eq!(chip.sample_rate(4_000_000), 166_666);
    assert_eq!(chip.ssg_effective_clock(4_000_000), 2_000_000);
    chip.write(0, 0x2f); // prescale 2
    assert_eq!(chip.ssg_effective_clock(4_000_000), 8_000_000);

    let mut chip = Ym2608::new(NullInterface);
    assert_eq!(chip.sample_rate(8_000_000), 1_000_000);
    assert_eq!(chip.ssg_effective_clock(8_000_000), 2_000_000);
    chip.set_fidelity(OpnFidelity::Min);
    assert_eq!(chip.sample_rate(8_000_000), 166_666);

    for rate in [
        Ymf288::new(NullInterface).sample_rate(8_000_000),
        Ym2610::new(NullInterface).sample_rate(8_000_000),
        Ym2610b::new(NullInterface).sample_rate(8_000_000),
    ] {
        assert_eq!(rate, 500_000);
    }
    let mut chip = Ym2610::new(NullInterface);
    chip.set_fidelity(OpnFidelity::Med);
    assert_eq!(chip.sample_rate(8_000_000), 55_555);
    assert_eq!(chip.fidelity(), OpnFidelity::Med);
    assert_eq!(OpnFidelity::default(), OpnFidelity::Max);
}

/// Every write of a log, in order.
fn writes(name: &str) -> Vec<(u32, u8)> {
    common::read_log("opn", name)
        .commands
        .iter()
        .flat_map(|command| match &command.action {
            common::Action::Write(pairs) => pairs.clone(),
            _ => Vec::new(),
        })
        .collect()
}

/// Plays a log's writes into a used chip, resets it, and checks that its
/// registers are a new chip's and that it is silent; `$make` makes a chip.
/// As in ymfm, a reset leaves the envelope counter, the SSG resampler's
/// sample index (which paces the FM samples) and the last FM sample alone,
/// so a reset chip is not sample for sample a new one.
macro_rules! resets_to_silence {
    ($name:expr, $outputs:expr, $make:expr) => {{
        let mut used = $make;
        let mut out = [Output::<$outputs>::default(); 4];
        for (offset, data) in &writes($name) {
            used.write(*offset, *data);
            used.generate(&mut out);
        }
        assert!(out
            .iter()
            .any(|sample| sample.data.iter().any(|value| *value != 0)));
        used.reset();
        assert_eq!(used.registers(), $make.registers(), "{}", $name);
        // the last FM sample plays until the next FM clock
        let mut after = [Output::<$outputs>::default(); 64];
        used.generate(&mut after);
        for _ in 0..100 {
            used.generate(&mut after);
            assert!(
                after
                    .iter()
                    .all(|sample| sample.data.iter().all(|value| *value == 0)),
                "{}",
                $name
            );
        }
    }};
}

#[test]
fn resets_to_a_new_chips_registers_and_silence() {
    resets_to_silence!("ym2203-fidelity", 4, Ym2203::new(NullInterface));
    resets_to_silence!("ym2608-fm", 3, Ym2608::new(NullInterface));
    resets_to_silence!("ymf288-mix", 3, Ymf288::new(NullInterface));
    resets_to_silence!("ym2610-fm", 3, Ym2610::new(NullInterface));
    resets_to_silence!("ym2610b-mix", 3, Ym2610b::new(NullInterface));
}

#[test]
fn generates_blocks_as_it_generates_single_samples() {
    let writes = writes("ym2203-ssg-min");
    let mut whole = Ym2203::new(NullInterface);
    let mut single = Ym2203::new(NullInterface);
    whole.set_fidelity(OpnFidelity::Min);
    single.set_fidelity(OpnFidelity::Min);
    for (offset, data) in &writes {
        whole.write(*offset, *data);
        single.write(*offset, *data);
    }
    let mut block = [Output::<4>::default(); 37];
    let mut one = [Output::<4>::default()];
    for _ in 0..200 {
        whole.generate(&mut block);
        for sample in &block {
            single.generate(&mut one);
            assert_eq!(one[0], *sample);
        }
    }
}

#[test]
fn an_override_leaves_the_ssg_output_alone() {
    // made with an override, a chip tells it of the prescale where ymfm's
    // update_prescale does (the YM2203 and YM2608), resets it, and never
    // writes the SSG output
    let mut host = Host::new();
    let recorder = Recorder::new(&mut host);
    let log = host.ssg_log.clone().unwrap();
    let mut chip = Ym2203::with_ssg_override(host, recorder);
    assert_eq!(log.borrow().as_slice(), [(3, 0, 0), (0, 0, 0)]);
    chip.write(0, 0x08);
    chip.write(1, 0x0f);
    chip.write(0, 0x2f);
    assert_eq!(log.borrow()[2..], [(2, 8, 0x0f), (3, 0, 0)]);
    let mut out = [Output::<4>::default(); 64];
    out.iter_mut().for_each(|sample| sample.data[1..].fill(77));
    chip.generate(&mut out);
    assert!(out.iter().all(|sample| sample.data[1..] == [77; 3]));

    let mut host = Host::new();
    let recorder = Recorder::new(&mut host);
    let log = host.ssg_log.clone().unwrap();
    let mut chip = Ym2610::with_ssg_override(host, recorder);
    assert_eq!(log.borrow().as_slice(), [(0, 0, 0)]);
    let mut out = [Output::<3>::default(); 64];
    out.iter_mut().for_each(|sample| sample.data[2] = 77);
    chip.generate(&mut out);
    assert!(out.iter().all(|sample| sample.data[2] == 77));
}

//*********************************************************
//  RANDOMIZED DIFFERENTIAL TESTS
//*********************************************************

/// A directory for a differential's memory images, removed when it ends
/// unless a log failed or `YMFM_KEEP` is set.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("ymfm-oxide-{label}-images-{}", std::process::id()));
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Scratch(path)
    }

    /// Writes an image and returns its absolute path.
    fn image(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).expect("the image writes");
        path.to_str().expect("a UTF-8 path").to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !std::thread::panicking() && std::env::var_os("YMFM_KEEP").is_none() {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
}

/// What a random log may reach on a chip.
#[derive(Clone, Copy)]
struct Spec {
    chip: &'static str,
    clock: u32,
    /// The OPNA register set, with its second bank.
    opna: bool,
    /// The YM2203's and YM2608's prescale writes.
    prescale: bool,
    /// The SSG's I/O ports.
    io: bool,
    /// ADPCM-A from memory, at the upper port (the YM2610).
    adpcm_a_memory: bool,
    /// ADPCM-A as the rhythm ROM, at the lower port (YM2608, YMF288).
    rhythm: bool,
    /// ADPCM-B at the upper port (YM2608).
    adpcm_b_upper: bool,
    /// ADPCM-B at the lower port (YM2610).
    adpcm_b_lower: bool,
}

const YM2203: Spec = Spec {
    chip: "ym2203",
    clock: 3_993_600,
    opna: false,
    prescale: true,
    io: true,
    adpcm_a_memory: false,
    rhythm: false,
    adpcm_b_upper: false,
    adpcm_b_lower: false,
};
const YM2608: Spec = Spec {
    chip: "ym2608",
    clock: 7_987_200,
    opna: true,
    prescale: true,
    io: true,
    adpcm_a_memory: false,
    rhythm: true,
    adpcm_b_upper: true,
    adpcm_b_lower: false,
};
const YMF288: Spec = Spec {
    chip: "ymf288",
    clock: 15_974_400,
    opna: true,
    prescale: false,
    io: false,
    adpcm_a_memory: false,
    rhythm: true,
    adpcm_b_upper: false,
    adpcm_b_lower: false,
};
const YM2610: Spec = Spec {
    chip: "ym2610",
    clock: 8_000_000,
    opna: true,
    prescale: false,
    io: false,
    adpcm_a_memory: true,
    rhythm: false,
    adpcm_b_upper: false,
    adpcm_b_lower: true,
};
const YM2610B: Spec = Spec {
    chip: "ym2610b",
    ..YM2610
};

/// Appends a register write: through the lower ports, or the upper ones
/// for an address with bit 8 set.
fn reg(text: &mut String, sample: u32, register: u32, value: u32) {
    let port = if register & 0x100 != 0 { 2 } else { 0 };
    let _ = writeln!(
        text,
        "{sample} w {port:x} {:02x} {:x} {:02x}",
        register & 0xff,
        port + 1,
        value & 0xff
    );
}

/// A random span of `size` bytes in units of `1 << shift`, as (start,
/// end), the end inclusive.
fn span(random: &mut Random, size: usize, shift: u32) -> (u32, u32) {
    let units = (size >> shift).max(2) as u32;
    let start = random.below(units - 1);
    let end = (start + 1 + random.below(units - start)).min(units + 8);
    (start, end)
}

/// Writes a random log for `spec`: bursts of writes biased towards sound,
/// over every register the chip decodes and some it does not, with the
/// prescale and fidelity changed, reads of every port, address writes
/// alone, timers, and the ADPCM engines started over random spans of random
/// images.
fn random_log(
    seed: u64,
    spec: Spec,
    scratch: &Scratch,
    ssg_override: bool,
    samples: u32,
) -> String {
    let mut random = Random::new(seed);
    let mut text = format!(
        "chip {}\nclock {}\nsamples {samples}\n",
        spec.chip, spec.clock
    );
    if ssg_override {
        text.push_str("ssg-override\n");
    }
    let fidelities = ["max", "min", "med"];
    // mostly the minimum fidelity, where an output sample spans the most
    let fidelity = [1, 1, 1, 2, 0][random.below(5) as usize];
    let _ = writeln!(text, "fidelity {}", fidelities[fidelity]);
    let image_size = 0x1000 * (1 + random.below(8) as usize);
    let image = |random: &mut Random| -> Vec<u8> {
        // encoded waveforms or noise
        let mut bytes = Vec::with_capacity(image_size);
        while bytes.len() < image_size {
            match random.below(3) {
                0 => bytes.extend(noise(512, u64::from(random.next()))),
                1 => bytes.extend(encode_adpcm_a(&sine(
                    1024,
                    8.0 + f64::from(random.below(200)),
                    f64::from(200 + random.below(1800)),
                ))),
                _ => bytes.extend(encode_adpcm_b(&sine(
                    1024,
                    8.0 + f64::from(random.below(200)),
                    f64::from(2000 + random.below(30000)),
                ))),
            }
        }
        bytes.truncate(image_size);
        bytes
    };
    if spec.rhythm {
        let rom: Vec<u8> = if random.below(2) == 0 {
            rhythm_image()
        } else {
            let mut bytes = image(&mut random);
            bytes.resize(0x2000, 0);
            bytes
        };
        let path = scratch.image(&format!("{}-{seed}-a.bin", spec.chip), &rom);
        let _ = writeln!(text, "mem adpcm-a {path}");
    }
    if spec.adpcm_a_memory {
        let bytes = image(&mut random);
        let path = scratch.image(&format!("{}-{seed}-a.bin", spec.chip), &bytes);
        let _ = writeln!(text, "mem adpcm-a {path}");
    }
    if spec.adpcm_b_upper || spec.adpcm_b_lower {
        let bytes = image(&mut random);
        let path = scratch.image(&format!("{}-{seed}-b.bin", spec.chip), &bytes);
        let _ = writeln!(text, "mem adpcm-b {path}");
    }
    if spec.io {
        let path = scratch.image(
            &format!("{}-{seed}-io.bin", spec.chip),
            &[random.byte(), random.byte()],
        );
        let _ = writeln!(text, "mem io {path}");
    }

    // most logs start with all six channels of an OPNA and the IRQs on
    if spec.opna && random.below(4) != 0 {
        reg(&mut text, 0, 0x29, 0x80 | random.below(0x20));
    }
    let banks = if spec.opna { 2 } else { 1 };
    let b_base = if spec.adpcm_b_upper { 0x100 } else { 0x10 };
    let mut sample = 0u32;
    while sample < samples {
        let burst = 1 + random.below(24);
        for _ in 0..burst {
            let bank = 0x100 * random.below(banks);
            let operator = random.below(16);
            let channel = random.below(3);
            match random.below(40) {
                // FM operators and channels, as the OPN2 random logs do
                0 => reg(
                    &mut text,
                    sample,
                    bank | (0x30 + operator),
                    random.next() & 0x7f,
                ),
                1 | 2 => reg(
                    &mut text,
                    sample,
                    bank | (0x40 + operator),
                    random.next() & 0x3f,
                ),
                3 => reg(
                    &mut text,
                    sample,
                    bank | (0x50 + operator),
                    random.next() & 0xdf | 0x10,
                ),
                4 => reg(
                    &mut text,
                    sample,
                    bank | (0x60 + operator),
                    random.next() & 0x9f,
                ),
                5 => reg(
                    &mut text,
                    sample,
                    bank | (0x70 + operator),
                    random.next() & 0x1f,
                ),
                6 => reg(&mut text, sample, bank | (0x80 + operator), random.next()),
                7 => reg(
                    &mut text,
                    sample,
                    bank | (0x90 + operator),
                    random.next() & 0x0f,
                ),
                8 => reg(
                    &mut text,
                    sample,
                    bank | (0xa4 + channel),
                    random.next() & 0x3f,
                ),
                9 => reg(&mut text, sample, bank | (0xa0 + channel), random.next()),
                10 => reg(
                    &mut text,
                    sample,
                    bank | (0xa8 + random.below(8)),
                    random.next(),
                ),
                11 => reg(
                    &mut text,
                    sample,
                    bank | (0xb0 + channel),
                    random.next() & 0x3f,
                ),
                12 => reg(&mut text, sample, bank | (0xb4 + channel), random.next()),
                13 | 14 => reg(&mut text, sample, 0x28, random.next() & 0xf7),
                15 => reg(&mut text, sample, 0x22, random.next() & 0x0f),
                // timers and the mode register
                16 => match random.below(3) {
                    0 => reg(&mut text, sample, 0x24 + random.below(3), random.next()),
                    _ => reg(&mut text, sample, 0x27, random.next()),
                },
                // the SSG
                17..=19 => {
                    let register = match random.below(8) {
                        0..=2 => random.below(6),
                        3 => 6,
                        4 => 7,
                        5 | 6 => 8 + random.below(3),
                        _ => 11 + random.below(5),
                    };
                    let value = match register {
                        8..=10 => random.next() & 0x1f,
                        1 | 3 | 5 => random.next() & 0x0f,
                        _ => random.next(),
                    };
                    reg(&mut text, sample, register, value);
                }
                // reads of every port, after an address write alone
                20 | 21 => {
                    let port = if banks == 2 {
                        random.below(4)
                    } else {
                        random.below(2)
                    };
                    if random.below(2) == 0 {
                        let address = match random.below(4) {
                            0 => random.below(0x10),
                            1 => 0xff,
                            _ => random.below(0x100),
                        };
                        let _ = writeln!(
                            text,
                            "{sample} w {:x} {address:02x}",
                            if port >= 2 && banks == 2 { 2 } else { 0 }
                        );
                    }
                    let _ = writeln!(text, "{sample} r {port:x}");
                }
                // the prescale, through address writes alone
                22 if spec.prescale => {
                    let _ = writeln!(text, "{sample} w 0 {:02x}", 0x2d + random.below(3));
                }
                // the fidelity, now and then
                23 if random.below(6) == 0 => {
                    let _ = writeln!(text, "{sample} f {}", fidelities[random.below(3) as usize]);
                }
                // IRQ enable and flag control (and the YMF288's mode)
                24 if spec.opna => match random.below(4) {
                    0 => reg(&mut text, sample, 0x29, random.next()),
                    1 if !spec.adpcm_b_lower => reg(&mut text, sample, 0x110, random.next()),
                    2 if spec.chip == "ymf288" => {
                        reg(&mut text, sample, 0x20, random.next() & 0x03)
                    }
                    _ if spec.adpcm_b_lower => reg(&mut text, sample, 0x1c, random.next()),
                    _ => reg(&mut text, sample, 0x29, random.next()),
                },
                // rhythm or ADPCM-A
                25..=28 if spec.rhythm => match random.below(5) {
                    0 | 1 => reg(&mut text, sample, 0x10, random.next() & 0xbf),
                    2 => reg(&mut text, sample, 0x10, 0x80 | random.next()),
                    3 => reg(&mut text, sample, 0x11, random.next() & 0x3f),
                    _ => reg(
                        &mut text,
                        sample,
                        0x18 + random.below(6),
                        random.next() | (0xc0 & random.next()),
                    ),
                },
                25..=28 if spec.adpcm_a_memory => {
                    let ch = random.below(6);
                    match random.below(6) {
                        0 => {
                            let (start, end) = span(&mut random, image_size, 8);
                            reg(&mut text, sample, 0x110 + ch, start);
                            reg(&mut text, sample, 0x118 + ch, start >> 8);
                            reg(&mut text, sample, 0x120 + ch, end);
                            reg(&mut text, sample, 0x128 + ch, end >> 8);
                            reg(&mut text, sample, 0x108 + ch, 0xc0 | random.next());
                            reg(&mut text, sample, 0x100, 1 << ch);
                        }
                        1 => reg(&mut text, sample, 0x100, random.next()),
                        2 => reg(&mut text, sample, 0x101, random.next() & 0x3f),
                        3 => reg(&mut text, sample, 0x108 + ch, random.next()),
                        4 => reg(&mut text, sample, 0x100 + random.below(0x30), random.next()),
                        _ => reg(&mut text, sample, 0x100, 0x80 | random.next()),
                    }
                }
                // ADPCM-B
                29..=32 if spec.adpcm_b_upper || spec.adpcm_b_lower => {
                    match random.below(6) {
                        0 | 1 => {
                            let shift = if spec.adpcm_b_lower {
                                8
                            } else {
                                [2, 5][random.below(2) as usize]
                            };
                            let (start, end) = span(&mut random, image_size, shift);
                            let control1 = if spec.adpcm_b_lower {
                                0xc0
                            } else {
                                0xc0 | if shift == 5 {
                                    2 * random.below(2) + 1
                                } else {
                                    0
                                }
                            };
                            reg(&mut text, sample, b_base + 1, control1);
                            reg(&mut text, sample, b_base + 2, start);
                            reg(&mut text, sample, b_base + 3, start >> 8);
                            reg(&mut text, sample, b_base + 4, end);
                            reg(&mut text, sample, b_base + 5, end >> 8);
                            reg(&mut text, sample, b_base + 9, random.next());
                            reg(&mut text, sample, b_base + 10, 0x20 + random.below(0xe0));
                            reg(&mut text, sample, b_base + 11, 0x40 + random.below(0xc0));
                            reg(
                                &mut text,
                                sample,
                                b_base,
                                [0xa0, 0xb0, 0x20, 0x60][random.below(4) as usize],
                            );
                        }
                        2 => reg(&mut text, sample, b_base, random.next()),
                        3 => reg(
                            &mut text,
                            sample,
                            b_base + random.below(0x10),
                            random.next(),
                        ),
                        4 => reg(&mut text, sample, b_base + 8, random.next()),
                        _ => {
                            // the memory read path, through the upper data port
                            reg(&mut text, sample, b_base, 0x20);
                            let _ = writeln!(
                                text,
                                "{sample} w {:x} 08",
                                if b_base == 0x100 { 2 } else { 0 }
                            );
                            let _ = writeln!(text, "{sample} r 3");
                        }
                    }
                }
                // an address on one port and data on the other
                33 if banks == 2 => {
                    let high = random.below(2);
                    let _ = writeln!(
                        text,
                        "{sample} w {:x} {:02x} {:x} {:02x}",
                        2 * high,
                        random.below(0x100),
                        3 - 2 * high,
                        random.byte()
                    );
                }
                // anything at all
                34 => reg(
                    &mut text,
                    sample,
                    (0x100 * random.below(banks)) | random.below(0x100),
                    random.next(),
                ),
                _ => reg(&mut text, sample, 0x28, 0xf0 | (random.next() & 0x07)),
            }
        }
        sample += random.below(1200);
    }
    text
}

/// Runs a chip's differential over logs for `spec`.
fn differential<C: common::Chip>(label: &str, spec: Spec, ssg_override: bool, samples: u32) {
    let scratch = Scratch::new(label);
    common::differential::<C>(label, |seed| {
        random_log(seed, spec, &scratch, ssg_override, samples)
    });
}

#[test]
fn ym2203_matches_ymfm_on_random_logs_when_an_oracle_is_named() {
    differential::<Ym2203<Host>>("ym2203", YM2203, false, 24_000);
}

#[test]
fn ym2608_matches_ymfm_on_random_logs_when_an_oracle_is_named() {
    differential::<Ym2608<Host>>("ym2608", YM2608, false, 24_000);
}

#[test]
fn ymf288_matches_ymfm_on_random_logs_when_an_oracle_is_named() {
    differential::<Ymf288<Host>>("ymf288", YMF288, false, 16_000);
}

#[test]
fn ym2610_matches_ymfm_on_random_logs_when_an_oracle_is_named() {
    differential::<Ym2610<Host>>("ym2610", YM2610, false, 16_000);
}

#[test]
fn ym2610b_matches_ymfm_on_random_logs_when_an_oracle_is_named() {
    differential::<Ym2610b<Host>>("ym2610b", YM2610B, false, 16_000);
}

#[test]
fn ym2203_matches_ymfm_on_random_logs_with_an_override_when_an_oracle_is_named() {
    differential::<Ym2203<Host, Recorder>>("ym2203-override", YM2203, true, 8_000);
}

#[test]
fn ym2608_matches_ymfm_on_random_logs_with_an_override_when_an_oracle_is_named() {
    differential::<Ym2608<Host, Recorder>>("ym2608-override", YM2608, true, 8_000);
}

#[test]
fn ymf288_matches_ymfm_on_random_logs_with_an_override_when_an_oracle_is_named() {
    differential::<Ymf288<Host, Recorder>>("ymf288-override", YMF288, true, 8_000);
}

#[test]
fn ym2610_matches_ymfm_on_random_logs_with_an_override_when_an_oracle_is_named() {
    differential::<Ym2610<Host, Recorder>>("ym2610-override", YM2610, true, 8_000);
}

#[test]
fn ym2610b_matches_ymfm_on_random_logs_with_an_override_when_an_oracle_is_named() {
    differential::<Ym2610b<Host, Recorder>>("ym2610b-override", YM2610B, true, 8_000);
}
