// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The ADPCM engines and the Y8950 held to ymfm, sample for sample.
//!
//! The memory images in `tests/adpcm` are synthetic: this test writes them
//! (`images_are_the_synthetic_ones` checks the committed ones, and writes
//! them again when `YMFM_WRITE_IMAGES` is set) by encoding sines, ramps,
//! squares and noise with an ADPCM encoder of its own, so no sample data
//! comes from anywhere else.
//!
//! The Y8950 cases are register logs rendered by `oracle/oracle.cpp`: ADPCM-B
//! playback from RAM and ROM at both address granularities, repeat, start,
//! end and the wrap at the limit address, delta-N rates from 0 to 0xffff,
//! the level, the reset bit, CPU-fed playback with its BRDY handshake,
//! recording to external memory through the interface (the record path's
//! external writes are events, compared too), the status register with its
//! live ADPCM bits and their masks, the timers and IRQ line, the keyboard and
//! I/O ports, and the OPL side the Y8950 shares with the YM3526.
//!
//! The Y8950 exposes neither the right pan, nor a fixed address shift, nor
//! the limit register, nor ADPCM-B's register-8 read path, and no chip the
//! main oracle drives reaches ADPCM-A until the OPN family is ported. So
//! `tests/adpcm/engines.cpp`, a second oracle, drives upstream's two engines
//! on their own, and the `engine-*` cases hold the translated engines to it:
//! ADPCM-A at both address shifts with every level, pan, key-on and dump,
//! the 20-bit end compare and the channel masks; ADPCM-B with the YM2610's
//! fixed shift, stereo pans, the limit register, the read path with its
//! dummy reads, recording and CPU feeding.
//!
//! With `YMFM_ORACLE` naming a built oracle, a randomized comparison of the
//! Y8950 runs as well (see `common::differential`); with
//! `YMFM_ENGINE_ORACLE` naming a built `engines.cpp`, so does one of each
//! engine.

#[macro_use]
mod common;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use common::synth::{chirp, encode_adpcm_a, encode_adpcm_b, noise, ramp, sine, square};
use common::{Host, Random};
use ymfm_oxide::adpcm::{AdpcmAEngine, AdpcmBEngine};
use ymfm_oxide::opl::Y8950;
use ymfm_oxide::{AccessClass, Output};

harness_chip!(Y8950<Host>, 1);

//*********************************************************
//  SYNTHETIC IMAGES
//*********************************************************

/// The ADPCM-B image the Y8950 cases play, 8 KB: four 1 KB segments (a
/// sine, a ramp, noise, a square loud enough to clamp) then a 4 KB chirp,
/// each encoded from the decoder's reset state.
fn y8950_image() -> Vec<u8> {
    let mut image = Vec::new();
    image.extend(encode_adpcm_b(&sine(2048, 64.0, 20000.0)));
    image.extend(encode_adpcm_b(&ramp(2048, 100, 24000)));
    image.extend(noise(1024, 8950));
    image.extend(encode_adpcm_b(&square(2048, 150, 40000)));
    image.extend(encode_adpcm_b(&chirp(8192, 30000.0)));
    assert_eq!(image.len(), 0x2000);
    image
}

/// The I/O image: the I/O lines at 0, the keyboard at 1.
fn y8950_io_image() -> Vec<u8> {
    vec![0x5a, 0xc3]
}

/// The ADPCM-A image, 8 KB: six 1 KB segments, one per channel, each with
/// its own waveform, then 2 KB of noise.
fn adpcm_a_image() -> Vec<u8> {
    let mut image = Vec::new();
    image.extend(encode_adpcm_a(&sine(2048, 40.0, 1800.0)));
    image.extend(encode_adpcm_a(&ramp(2048, 60, 1500)));
    image.extend(encode_adpcm_a(&square(2048, 80, 1000)));
    image.extend(encode_adpcm_a(&chirp(2048, 1900.0)));
    image.extend(encode_adpcm_a(&sine(2048, 13.0, 600.0)));
    image.extend(encode_adpcm_a(&sine(2048, 300.0, 2000.0)));
    image.extend(noise(2048, 2610));
    assert_eq!(image.len(), 0x2000);
    image
}

/// The images, by file name.
fn images() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("y8950.bin", y8950_image()),
        ("y8950-io.bin", y8950_io_image()),
        ("adpcm-a.bin", adpcm_a_image()),
    ]
}

#[test]
fn images_are_the_synthetic_ones() {
    let base = common::directory("adpcm");
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

#[test]
fn the_encoders_follow_their_targets() {
    // Not a property of ymfm, but of the images: a sine encoded and
    // decoded by the engine stays close to the sine, so the cases play
    // something shaped rather than noise.
    let target = sine(512, 64.0, 20000.0);
    let image = encode_adpcm_b(&target);
    let mut host = Host::new();
    host.memory[AccessClass::AdpcmB as usize] = image;
    let mut engine = AdpcmBEngine::new(0);
    for (register, value) in [(0x01, 0x80), (0x02, 0), (0x03, 0), (0x04, 0x3f), (0x05, 0)] {
        engine.write(register, value, &mut host);
    }
    for (register, value) in [(0x09, 0xff), (0x0a, 0xff), (0x0b, 0xff), (0x00, 0xa0)] {
        engine.write(register, value, &mut host);
    }
    let mut worst = 0;
    for (n, want) in target.iter().enumerate().take(500) {
        // At delta-N 0xffff a nibble lands every sample but one in 65536.
        engine.clock(&mut host);
        let mut out = Output::<1>::default();
        engine.output(&mut out, 0);
        // The output is the interpolated accumulator scaled by 255/256.
        if n > 20 {
            worst = worst.max((out.data[0] - want * 255 / 256).abs());
        }
    }
    assert!(worst < 2500, "the sine decodes {worst} away from itself");
}

//*********************************************************
//  Y8950 GOLDENS
//*********************************************************

/// Holds the Y8950 log `name` to its golden.
fn check(name: &str) {
    let log = common::read_log("adpcm", name);
    assert_eq!(log.chip, "y8950", "{name}: not a Y8950 log");
    common::check::<Y8950<Host>>("adpcm", name);
}

#[test]
fn matches_ymfm_on_adpcm_b_playback_from_ram() {
    check("y8950-play");
}

#[test]
fn matches_ymfm_on_repeat_rom_and_8_bit_dram() {
    check("y8950-repeat");
}

#[test]
fn matches_ymfm_on_the_wrap_at_the_limit_address() {
    check("y8950-limit");
}

#[test]
fn matches_ymfm_on_delta_n_rates_and_levels() {
    check("y8950-rates");
}

#[test]
fn matches_ymfm_on_cpu_fed_playback() {
    check("y8950-cpu");
}

#[test]
fn matches_ymfm_on_recording_to_external_memory() {
    check("y8950-record");
}

#[test]
fn matches_ymfm_on_the_keyboard_and_io_ports() {
    check("y8950-io");
}

#[test]
fn matches_ymfm_on_the_fm_side_timers_status_and_irq() {
    check("y8950-fm");
}

#[test]
fn resets_to_the_state_a_new_chip_starts_in() {
    let log = common::read_log("adpcm", "y8950-play");
    let writes: Vec<(u32, u8)> = log
        .commands
        .iter()
        .flat_map(|command| match &command.action {
            common::Action::Write(pairs) => pairs.clone(),
            _ => Vec::new(),
        })
        .collect();
    let mut host = Host::new();
    host.memory[AccessClass::AdpcmB as usize] = y8950_image();
    let mut used = Y8950::new(host.clone());
    let mut out = [Output::<1>::default()];
    for (offset, data) in &writes {
        used.write(*offset, *data);
        used.generate(&mut out);
    }
    used.reset();
    let mut fresh = Y8950::new(host);
    assert_eq!(used.registers(), fresh.registers());
    assert_eq!(used.adpcm_b_registers(), fresh.adpcm_b_registers());
    assert_eq!(used.read_status(), fresh.read_status());
    for (offset, data) in &writes {
        used.write(*offset, *data);
        fresh.write(*offset, *data);
    }
    let mut a = [Output::<1>::default()];
    let mut b = [Output::<1>::default()];
    for _ in 0..20_000 {
        used.generate(&mut a);
        fresh.generate(&mut b);
        assert_eq!(a, b);
    }
}

//*********************************************************
//  ENGINE GOLDENS
//*********************************************************

/// A log for the engine oracle, `tests/adpcm/engines.cpp`.
#[derive(Clone, Debug)]
struct EngineLog {
    engine: char,
    shift: u32,
    rshift: u32,
    samples: u32,
    memory: Vec<u8>,
    commands: Vec<(u32, char, Vec<u32>)>,
}

/// Reads an engine log; `directory` resolves the memory image's path.
fn parse_engine_log(text: &str, directory: &Path) -> EngineLog {
    let mut log = EngineLog {
        engine: ' ',
        shift: 0,
        rshift: 0,
        samples: 0,
        memory: Vec::new(),
        commands: Vec::new(),
    };
    for line in text.lines() {
        let body = line.split('#').next().unwrap_or("");
        let words: Vec<&str> = body.split_whitespace().collect();
        let Some(first) = words.first() else {
            continue;
        };
        match (*first, words.len()) {
            ("engine", 2) => log.engine = words[1].chars().next().unwrap(),
            ("shift", 2) => log.shift = words[1].parse().expect("a shift"),
            ("rshift", 2) => log.rshift = words[1].parse().expect("a shift"),
            ("samples", 2) => log.samples = words[1].parse().expect("a sample count"),
            ("mem", 2) => {
                let path = directory.join(words[1]);
                log.memory = std::fs::read(&path)
                    .unwrap_or_else(|_| panic!("cannot read {}", path.display()));
            }
            _ if first.starts_with(|c: char| c.is_ascii_digit()) && words.len() >= 3 => {
                let sample = first.parse().expect("a sample");
                let kind = words[1].chars().next().unwrap();
                let values = words[2..]
                    .iter()
                    .map(|word| u32::from_str_radix(word, 16).expect("a hex value"))
                    .collect();
                log.commands.push((sample, kind, values));
            }
            _ => panic!("cannot read the line: {line}"),
        }
    }
    assert!(matches!(log.engine, 'a' | 'b'), "the log needs an engine");
    log
}

/// What an engine produced: per sample both outputs and the flags byte,
/// then the events as (sample, kind, address, value).
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngineRender {
    samples: Vec<(i32, i32, u8)>,
    events: Vec<(u32, u8, u32, u8)>,
}

/// Reads a render in the engine oracle's format.
fn decode_engine(bytes: &[u8], samples: usize) -> Result<EngineRender, String> {
    let body = samples * 9;
    if bytes.len() < body + 4 {
        return Err(format!(
            "the render is {} bytes, short of {samples} samples",
            bytes.len()
        ));
    }
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let samples = (0..samples)
        .map(|n| {
            (
                u32_at(9 * n) as i32,
                u32_at(9 * n + 4) as i32,
                bytes[9 * n + 8],
            )
        })
        .collect();
    let count = u32_at(body) as usize;
    if bytes.len() != body + 4 + count * 10 {
        return Err(format!("the render's {count} events do not fill it"));
    }
    let events = (0..count)
        .map(|n| {
            let at = body + 4 + 10 * n;
            (u32_at(at), bytes[at + 4], u32_at(at + 5), bytes[at + 9])
        })
        .collect();
    Ok(EngineRender { samples, events })
}

/// Renders an engine log through the translated engine.
fn render_engine(log: &EngineLog) -> EngineRender {
    let mut host = Host::new();
    let class = if log.engine == 'a' {
        AccessClass::AdpcmA
    } else {
        AccessClass::AdpcmB
    };
    host.memory[class as usize] = log.memory.clone();
    let mut a = AdpcmAEngine::new(log.shift);
    let mut b = AdpcmBEngine::new(log.shift);
    a.reset();
    b.reset();
    let mut mask = 0x3f;
    let mut next = 0;
    let mut samples = Vec::with_capacity(log.samples as usize);
    let mut reads = Vec::new();
    for sample in 0..log.samples {
        host.sample = sample;
        while next < log.commands.len() && log.commands[next].0 <= sample {
            let (_, kind, values) = &log.commands[next];
            match (kind, log.engine) {
                ('w', engine) => {
                    for pair in values.as_chunks::<2>().0 {
                        if engine == 'a' {
                            a.write(pair[0], pair[1] as u8);
                        } else {
                            b.write(pair[0], pair[1] as u8, &mut host);
                        }
                    }
                }
                ('r', 'b') => {
                    let value = b.read(values[0], &mut host) as u8;
                    // keep reads and writes in the order they happened
                    reads.push((host.events.len(), (sample, b'r', values[0], value)));
                }
                ('m', 'a') => mask = values[0],
                _ => panic!("cannot apply the command at {sample}"),
            }
            next += 1;
        }
        let mut output = Output::<2>::default();
        let flags = if log.engine == 'a' {
            let flags = a.clock(mask, &mut host) as u8;
            a.output(&mut output, mask);
            flags
        } else {
            b.clock(&mut host);
            b.output(&mut output, log.rshift);
            b.status()
        };
        samples.push((output.data[0], output.data[1], flags));
    }
    // merge the reads into the host's external writes
    let writes: Vec<(u32, u8, u32, u8)> = host
        .events
        .iter()
        .map(|event| (event.sample, event.kind, event.address, event.value))
        .collect();
    let mut events = Vec::new();
    let mut taken = 0;
    for (before, read) in reads {
        events.extend_from_slice(&writes[taken..before]);
        taken = before;
        events.push(read);
    }
    events.extend_from_slice(&writes[taken..]);
    EngineRender { samples, events }
}

/// Compares an engine render with the expected one, naming the first
/// difference.
fn compare_engine(log: &EngineLog, expected: &EngineRender) -> Result<(), String> {
    let got = render_engine(log);
    for (n, (want, have)) in expected.samples.iter().zip(&got.samples).enumerate() {
        if want != have {
            return Err(format!(
                "sample {n}: ymfm gives {want:?} and this gives {have:?} (left, right, flags)"
            ));
        }
    }
    if expected.samples.len() != got.samples.len() {
        return Err("the sample counts differ".into());
    }
    for (n, (want, have)) in expected.events.iter().zip(&got.events).enumerate() {
        if want != have {
            return Err(format!(
                "event {n}: ymfm gives {want:?} and this gives {have:?} (sample, kind, address, value)"
            ));
        }
    }
    if expected.events.len() != got.events.len() {
        return Err(format!(
            "ymfm records {} events and this {}",
            expected.events.len(),
            got.events.len()
        ));
    }
    Ok(())
}

/// Holds the engine log `name` to its golden.
fn check_engine(name: &str) {
    let base = common::directory("adpcm");
    let text = std::fs::read_to_string(base.join(format!("{name}.log"))).expect("the log reads");
    let log = parse_engine_log(&text, &base);
    let golden = std::fs::read(base.join(format!("{name}.golden"))).expect("the golden reads");
    let expected = decode_engine(&golden, log.samples as usize)
        .unwrap_or_else(|reason| panic!("{name}: {reason}"));
    assert!(
        expected.samples.iter().any(|s| s.0 != 0 || s.1 != 0),
        "{name}: the golden is silent"
    );
    if let Err(reason) = compare_engine(&log, &expected) {
        panic!("{name}: {reason}");
    }
}

#[test]
fn matches_ymfm_on_adpcm_a_as_the_ym2610_addresses_it() {
    check_engine("engine-a-ym2610");
}

#[test]
fn matches_ymfm_on_adpcm_a_as_the_ym2608_addresses_it() {
    check_engine("engine-a-ym2608");
}

#[test]
fn matches_ymfm_on_adpcm_b_as_the_ym2610_addresses_it() {
    check_engine("engine-b-ym2610");
}

#[test]
fn matches_ymfm_on_adpcm_b_memory_reads_writes_and_cpu_feeding() {
    check_engine("engine-b-memory");
}

//*********************************************************
//  RANDOMIZED DIFFERENTIAL TESTS
//*********************************************************

/// A scratch directory for the random images, removed when dropped unless
/// a test failed or `YMFM_KEEP` is set.
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
        // kept on a failure, or with YMFM_KEEP, as the logs are
        if !std::thread::panicking() && std::env::var_os("YMFM_KEEP").is_none() {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
}

/// A random ADPCM-B image: segments of encoded waveforms and of noise.
fn random_b_image(random: &mut Random, size: usize) -> Vec<u8> {
    let mut image = Vec::with_capacity(size);
    while image.len() < size {
        let count = 64 * (1 + random.below(32) as usize);
        let amplitude = 1000 + random.below(40000) as i32;
        match random.below(4) {
            0 => image.extend(encode_adpcm_b(&sine(
                count,
                8.0 + f64::from(random.below(200)),
                f64::from(amplitude),
            ))),
            1 => image.extend(encode_adpcm_b(&square(
                count,
                1 + random.below(100) as usize,
                amplitude,
            ))),
            2 => image.extend(encode_adpcm_b(&ramp(
                count,
                2 + random.below(100) as usize,
                amplitude,
            ))),
            _ => image.extend(noise(count / 2, u64::from(random.next()))),
        }
    }
    image.truncate(size);
    image
}

/// A start and end register pair inside an image of `size` bytes at an
/// address shift, mostly, and sometimes anywhere.
fn random_span(random: &mut Random, size: usize, shift: u32) -> (u32, u32) {
    let units = (size >> shift).max(2) as u32;
    if random.below(8) == 0 {
        return (random.below(0x10000), random.below(0x10000));
    }
    let start = random.below(units - 1);
    let longest = if random.below(2) == 0 { 4 } else { 64 };
    let end = start + random.below((units - start).min(longest));
    (start, end)
}

/// One coherent ADPCM-B scenario, as ADPCM-B register writes and, where
/// `value` is `None`, reads of the data register: playback, recording then
/// playing back, reading memory back (with `reads`), or CPU feeding. With a
/// `shift` of 0 the memory type picks it, as on the Y8950.
fn b_scenario(
    random: &mut Random,
    size: usize,
    shift: u32,
    reads: bool,
) -> Vec<(u32, Option<u32>)> {
    let mut steps = Vec::new();
    let kind = match random.below(if reads { 4 } else { 3 }) {
        2 if !reads => 3,
        kind => kind,
    };
    let memory = random.below(4);
    let unit = match (shift, memory) {
        (0, 0) => 2,
        (0, _) => 5,
        (shift, _) => shift,
    };
    let (start, end) = random_span(random, size, unit);
    let mut set = |register: u32, value: u32| steps.push((register, Some(value)));
    set(0x01, (random.next() & 0xc0) | memory);
    for (register, value) in [(2, start), (3, start >> 8), (4, end), (5, end >> 8)] {
        set(register, value & 0xff);
    }
    let delta = 0x1000 + random.below(0xf000);
    set(0x09, delta & 0xff);
    set(0x0a, delta >> 8);
    set(0x0b, 0x40 + random.below(0xc0));
    match kind {
        0 => set(0x00, [0xa0, 0xb0, 0xa8, 0xb0][random.below(4) as usize]),
        1 => {
            set(0x00, 0x60);
            for _ in 0..1 + random.below(80) {
                set(0x08, random.next() & 0xff);
            }
            set(0x00, 0x20);
            set(0x00, [0xa0, 0xb0][random.below(2) as usize]);
        }
        2 => {
            set(0x00, 0x20);
            for _ in 0..1 + random.below(80) {
                steps.push((0x08, None));
            }
        }
        _ => {
            set(0x00, [0x80, 0x90][random.below(2) as usize]);
            for _ in 0..1 + random.below(8) {
                set(0x08, random.next() & 0xff);
            }
        }
    }
    steps
}

/// Writes a random Y8950 log: OPL writes as the YM3526 takes them, the
/// ADPCM-B registers with playable spans and rates, CPU data, recording,
/// the keyboard and I/O ports, status and data-port reads, and the timers.
fn random_y8950_log(seed: u64, scratch: &Scratch, samples: u32) -> String {
    let mut random = Random::new(seed);
    let size = 1024 * (1 + random.below(16) as usize);
    let image = random_b_image(&mut random, size);
    let image = scratch.image(&format!("b-{seed}.bin"), &image);
    let io = scratch.image(&format!("io-{seed}.bin"), &[random.byte(), random.byte()]);
    let mut text =
        format!("chip y8950\nclock 3579545\nsamples {samples}\nmem adpcm-b {image}\nmem io {io}\n");
    let mut sample = 0u32;
    let write = |text: &mut String, sample: u32, register: u32, value: u32| {
        let _ = writeln!(text, "{sample} w 0 {register:02x} 1 {value:02x}");
    };
    // most logs start with something playing
    if random.below(4) != 0 {
        let shift = if random.below(2) == 0 { 2 } else { 5 };
        let (start, end) = random_span(&mut random, size, shift);
        write(
            &mut text,
            0,
            0x08,
            if shift == 2 { 0 } else { 1 + random.below(2) },
        );
        write(&mut text, 0, 0x09, start & 0xff);
        write(&mut text, 0, 0x0a, start >> 8);
        write(&mut text, 0, 0x0b, end & 0xff);
        write(&mut text, 0, 0x0c, end >> 8);
        write(&mut text, 0, 0x10, random.byte().into());
        write(&mut text, 0, 0x11, 0x20 + random.below(0xe0));
        write(&mut text, 0, 0x12, 0x40 + random.below(0xc0));
        write(&mut text, 0, 0x07, [0xa0, 0xb0][random.below(2) as usize]);
    }
    while sample < samples {
        let burst = 1 + random.below(16);
        for _ in 0..burst {
            if random.below(8) == 0 {
                // ADPCM-B registers sit at 0x07 on, but for the split 0x08
                for (register, value) in b_scenario(&mut random, size, 0, false) {
                    let value = value.expect("no reads on the Y8950");
                    let (register, value) = if register == 1 {
                        (0x08, value & 0x0f)
                    } else {
                        (register + 7, value)
                    };
                    write(&mut text, sample, register, value);
                }
                continue;
            }
            let (register, value): (u32, u32) = match random.below(24) {
                0 => (0x20 + random.below(0x16), random.next() & 0xff),
                1 => (0x40 + random.below(0x16), random.next() & 0x7f),
                2 => (0x60 + random.below(0x16), random.next() & 0xff | 0x80),
                3 => (0x80 + random.below(0x16), random.next() & 0xff),
                4 => (0xa0 + random.below(9), random.next() & 0xff),
                5 => (0xb0 + random.below(9), 0x20 | (random.next() & 0x1f)),
                6 => (0xb0 + random.below(9), random.next() & 0x3f),
                7 => (0xc0 + random.below(9), random.next() & 0xff),
                8 => (0xbd, random.next() & 0xff),
                9 => match random.below(4) {
                    0 => (0x01, random.next() & 0xff),
                    1 => (0x02 + random.below(2), random.next() & 0xff),
                    _ => (0x04, random.next() & 0xff),
                },
                10 => {
                    // the control register: start, repeat, record, CPU, reset
                    const CONTROLS: [u32; 10] =
                        [0xa0, 0xb0, 0x80, 0x00, 0x20, 0x60, 0x01, 0xa1, 0x40, 0xe0];
                    let control = if random.below(6) == 0 {
                        random.next() & 0xff
                    } else {
                        CONTROLS[random.below(CONTROLS.len() as u32) as usize]
                    };
                    (0x07, control)
                }
                11 => (0x08, random.next() & 0xff),
                12 => {
                    let shift = 2 + 3 * random.below(2);
                    let (start, end) = random_span(&mut random, size, shift);
                    let pick = random.below(4);
                    let register = 0x09 + pick;
                    let value = match pick {
                        0 => start & 0xff,
                        1 => start >> 8,
                        2 => end & 0xff,
                        _ => end >> 8,
                    };
                    (register, value)
                }
                13 => (0x0d + random.below(2), random.next() & 0xff),
                14 | 15 => (0x0f, random.next() & 0xff),
                16 => (0x10 + random.below(2), random.next() & 0xff),
                17 => (0x12, random.next() & 0xff),
                18 => (0x13 + random.below(5), random.next() & 0xff),
                19 => (0x18 + random.below(2), random.next() & 0xff),
                20 => (0x06, random.next() & 0xff),
                21 => {
                    let _ = writeln!(text, "{sample} r 0");
                    continue;
                }
                22 => {
                    const DATA: [u32; 6] = [0x05, 0x09, 0x19, 0x1a, 0x0f, 0x04];
                    let register = DATA[random.below(DATA.len() as u32) as usize];
                    let _ = writeln!(text, "{sample} w 0 {register:02x}");
                    let _ = writeln!(text, "{sample} r 1");
                    continue;
                }
                _ => (0x08, random.next() & 0xc0),
            };
            write(&mut text, sample, register, value);
        }
        sample += random.below(1200);
    }
    text
}

#[test]
fn matches_ymfm_on_random_y8950_logs_when_an_oracle_is_named() {
    if std::env::var_os("YMFM_ORACLE").is_none() {
        return;
    }
    let scratch = Scratch::new("y8950");
    common::differential::<Y8950<Host>>("y8950", |seed| random_y8950_log(seed, &scratch, 20_000));
}

/// Writes a random engine log for `engine`, at a random address shift.
fn random_engine_log(seed: u64, engine: char, scratch: &Scratch, samples: u32) -> String {
    let mut random = Random::new(seed);
    let size = 1024 * (1 + random.below(16) as usize);
    let image = if engine == 'a' {
        let mut image = Vec::new();
        while image.len() < size {
            let count = 64 * (1 + random.below(32) as usize);
            let amplitude = 100 + random.below(2000) as i32;
            if random.below(4) == 0 {
                image.extend(noise(count / 2, u64::from(random.next())));
            } else {
                image.extend(encode_adpcm_a(&sine(
                    count,
                    6.0 + f64::from(random.below(200)),
                    f64::from(amplitude),
                )));
            }
        }
        image.truncate(size);
        image
    } else {
        random_b_image(&mut random, size)
    };
    let image = scratch.image(&format!("{engine}-{seed}.bin"), &image);
    let shift = if engine == 'a' {
        [0, 8, 1, 4][random.below(4) as usize]
    } else {
        [0, 8, 0, 3][random.below(4) as usize]
    };
    let rshift = random.below(4);
    let mut text = format!(
        "engine {engine}\nshift {shift}\nrshift {rshift}\nsamples {samples}\nmem {image}\n"
    );
    let mut sample = 0u32;
    while sample < samples {
        let burst = 1 + random.below(12);
        for _ in 0..burst {
            if engine == 'b' && random.below(6) == 0 {
                for (register, value) in b_scenario(&mut random, size, shift, true) {
                    match value {
                        Some(value) => {
                            let _ = writeln!(text, "{sample} w {register:x} {value:02x}");
                        }
                        None => {
                            let _ = writeln!(text, "{sample} r {register:x}");
                        }
                    }
                }
                continue;
            }
            if engine == 'a' {
                let channel = random.below(6);
                let span = (size >> shift).max(2) as u32;
                let (register, value) = match random.below(12) {
                    0 | 1 => (0x00, random.next() & 0x3f),
                    2 => (0x00, 0x80 | (random.next() & 0x3f)),
                    3 => (0x01, random.next() & 0xff),
                    4 => (0x02, random.next() & 0xff),
                    5 => (0x08 + channel, random.next() & 0xff),
                    6 | 7 => {
                        let start = random.below(span - 1);
                        let end = start + random.below((span - start).min(48));
                        let value = if random.below(8) == 0 {
                            random.below(0x10000)
                        } else if random.below(2) == 0 {
                            start
                        } else {
                            end
                        };
                        let base = [0x10, 0x18, 0x20, 0x28][random.below(4) as usize];
                        let high = base == 0x18 || base == 0x28;
                        (base + channel, if high { value >> 8 } else { value & 0xff })
                    }
                    8 => {
                        let _ = writeln!(text, "{sample} m {:x}", random.next() & 0x3f);
                        continue;
                    }
                    _ => (random.below(0x30), random.next() & 0xff),
                };
                let _ = writeln!(text, "{sample} w {register:x} {value:02x}");
            } else {
                let unit = if shift != 0 { shift } else { 2 };
                let (start, end) = random_span(&mut random, size, unit);
                let (register, value) = match random.below(16) {
                    0 | 1 => {
                        const CONTROLS: [u32; 10] =
                            [0xa0, 0xb0, 0x80, 0x00, 0x20, 0x60, 0x01, 0xa1, 0x40, 0xe0];
                        let control = if random.below(6) == 0 {
                            random.next() & 0xff
                        } else {
                            CONTROLS[random.below(CONTROLS.len() as u32) as usize]
                        };
                        (0x00, control)
                    }
                    2 => (0x01, random.next() & 0xff),
                    3 => (0x02, start & 0xff),
                    4 => (0x03, start >> 8),
                    5 => (0x04, end & 0xff),
                    6 => (0x05, end >> 8),
                    7 => (0x08, random.next() & 0xff),
                    8 => (0x09 + random.below(2), random.next() & 0xff),
                    9 => (0x0b, random.next() & 0xff),
                    10 => (
                        0x0c + random.below(2),
                        [start, end, 0xff][random.below(3) as usize] & 0xff,
                    ),
                    11 | 12 => {
                        let _ = writeln!(text, "{sample} r 8");
                        continue;
                    }
                    13 => {
                        let _ = writeln!(text, "{sample} r {:x}", random.below(0x10));
                        continue;
                    }
                    _ => (random.below(0x11), random.next() & 0xff),
                };
                let _ = writeln!(text, "{sample} w {register:x} {value:02x}");
            }
        }
        sample += random.below(800);
    }
    text
}

/// The engines' randomized differential test, over the engine oracle named
/// by `YMFM_ENGINE_ORACLE`, with the counts and seeds of
/// `common::differential`.
fn engine_differential(engine: char) {
    let Some(oracle) = std::env::var_os("YMFM_ENGINE_ORACLE") else {
        return;
    };
    let count: u64 = std::env::var("YMFM_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let first: u64 = std::env::var("YMFM_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let scratch = Scratch::new(&format!("engine-{engine}"));
    let mut failures = Vec::new();
    for seed in first..first + count {
        let text = random_engine_log(seed, engine, &scratch, 20_000);
        let path = scratch.0.join(format!("engine-{engine}-{seed}.log"));
        std::fs::write(&path, &text).expect("the log writes");
        let output = std::process::Command::new(&oracle)
            .arg(&path)
            .output()
            .expect("the engine oracle runs");
        assert!(
            output.status.success(),
            "the engine oracle failed on seed {seed}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let log = parse_engine_log(&text, &scratch.0);
        let result = decode_engine(&output.stdout, log.samples as usize)
            .and_then(|expected| compare_engine(&log, &expected));
        if let Err(reason) = result {
            failures.push(format!("seed {seed}: {reason}"));
        }
    }
    // on a failure the scratch directory, logs and images, is kept
    assert!(
        failures.is_empty(),
        "{} of {count} engine {engine} logs differ, kept in {}:\n{}",
        failures.len(),
        scratch.0.display(),
        failures.join("\n")
    );
}

#[test]
fn matches_ymfm_on_random_adpcm_a_logs_when_an_engine_oracle_is_named() {
    engine_differential('a');
}

#[test]
fn matches_ymfm_on_random_adpcm_b_logs_when_an_engine_oracle_is_named() {
    engine_differential('b');
}
