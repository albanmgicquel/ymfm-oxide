// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The YMF278B (OPL4) held to ymfm, sample for sample.
//!
//! Each case in `tests/pcm` is a synthetic register log and the render ymfm
//! made of it through `oracle/oracle.cpp`. The wavetable memory the logs
//! play, `tests/pcm/wavetable.bin`, is no ROM: `wavetable` below builds it,
//! headers and samples, from integer arithmetic, and a test holds the
//! committed file to it. The logs cover the 8-bit, 12-bit and 16-bit sample
//! formats and the fourth format code, loops (short, past the end, from
//! the start, with no end), the wave table headers (the registers they set,
//! the ninth wave table bit, both header banks from 384 up, a header
//! rewritten during a note), pitch over every octave and the F-number,
//! the envelope at many rates with rate correction, key timing and
//! pseudo-reverb, damping, the LFO at every speed with vibrato and AM and
//! the LFO reset, every panpot value, both output pairs, level
//! interpolation and level direct, the mix control, memory reads and
//! writes through the PCM data port with the address moving on and
//! wrapping, the status register with the chip ID, BUSY, LD and the
//! timers, and the FM side mixed in.
//!
//! With `YMFM_ORACLE` naming a built oracle, a randomized comparison runs as
//! well, over logs and memory images this test writes itself (see
//! `common::differential`).

#[macro_use]
mod common;

use std::fmt::Write as _;
use std::path::PathBuf;

use common::{Host, Random};
use ymfm_oxide::opl::Ymf278b;
use ymfm_oxide::{AccessClass, Output};

harness_chip!(Ymf278b<Host>, 6);

/// The size of the wavetable image: enough for the second header bank, at
/// 512 KiB, to hold a few headers.
const IMAGE_SIZE: usize = 0x80400;

/// One wave table header: the sample format, the start address, the loop
/// and end positions in samples, and the five bytes that set registers 80,
/// 98, B0, C8 and E0.
struct Header {
    format: u8,
    start: u32,
    looppos: u16,
    end: u16,
    params: [u8; 5],
}

/// Writes a header at `at`, the end stored negated, as the chip reads it.
fn put_header(image: &mut [u8], at: usize, header: &Header) {
    let end = 0u16.wrapping_sub(header.end);
    let bytes = [
        (header.format << 6) | ((header.start >> 16) & 0x3f) as u8,
        (header.start >> 8) as u8,
        header.start as u8,
        (header.looppos >> 8) as u8,
        header.looppos as u8,
        (end >> 8) as u8,
        end as u8,
    ];
    image[at..at + 7].copy_from_slice(&bytes);
    image[at + 7..at + 12].copy_from_slice(&header.params);
}

/// A sine-like wave in integers: two parabolas, one per half period, with a
/// peak of `peak`.
fn parabola(index: u32, period: u32, peak: i32) -> i32 {
    let half = period / 2;
    let x = (index % half) as i64;
    let h = half as i64;
    let value = (4 * x * (h - x) * peak as i64 / (h * h)) as i32;
    if index % period < half {
        value
    } else {
        -value
    }
}

/// Builds the synthetic wavetable image the golden logs play.
///
/// Samples:
/// - 0x2000: 256 8-bit samples, a triangle;
/// - 0x2100: 512 12-bit samples (768 bytes), a sawtooth;
/// - 0x2400: 1024 16-bit samples, two parabolas;
/// - 0x2c00: 4096 8-bit samples, a decaying noise burst ending in silence;
/// - 0x7f000: 1024 16-bit samples, a square wave of two levels.
///
/// Headers, at 12 times the wave table number (and from 0x80000 for the
/// second bank):
/// - 0: the triangle, looped whole;
/// - 1: the sawtooth, looped from 128;
/// - 2: the parabolas, looped from 512;
/// - 3: the noise burst, a one-shot holding its last, silent, sample;
/// - 4: the sawtooth again, under the fourth format code;
/// - 5: the triangle with its loop past its end;
/// - 6: the triangle with an end of zero;
/// - 7: the square wave, high in memory, with rate correction;
/// - 8: the parabolas with every register byte set;
/// - 384, 385, 511: bank-0 headers past 384;
/// - bank 1, 384 and 385: other waves at 0x80000.
fn wavetable() -> Vec<u8> {
    let mut image = vec![0u8; IMAGE_SIZE];

    // the triangle, 8-bit
    for i in 0..256u32 {
        let value = if i < 128 { i * 2 } else { 511 - i * 2 } as i32 - 128;
        image[0x2000 + i as usize] = value as u8;
    }
    // the sawtooth, 12-bit: two samples in three bytes, the first's high
    // byte, both low nibbles (the second's in the high half), the second's
    // high byte
    for pair in 0..256u32 {
        let first = (pair * 2 * 8) as i32 - 2048;
        let second = ((pair * 2 + 1) * 8) as i32 - 2048;
        let (first, second) = ((first & 0xfff) as u32, (second & 0xfff) as u32);
        let at = 0x2100 + 3 * pair as usize;
        image[at] = (first >> 4) as u8;
        image[at + 1] = (((second & 0xf) << 4) | (first & 0xf)) as u8;
        image[at + 2] = (second >> 4) as u8;
    }
    // the parabolas, 16-bit big-endian, two periods
    for i in 0..1024u32 {
        let value = parabola(i, 512, 30000) as u16;
        let at = 0x2400 + 2 * i as usize;
        image[at] = (value >> 8) as u8;
        image[at + 1] = value as u8;
    }
    // the noise burst, 8-bit, decaying to silence by its last sample
    let mut lfsr = 0x1234u32;
    for i in 0..4096u32 {
        lfsr = (lfsr >> 1) ^ (0xb400 & 0u32.wrapping_sub(lfsr & 1));
        let level = 4095 - i;
        let value = ((lfsr & 0xff) as i32 - 128) * level as i32 / 4096;
        image[0x2c00 + i as usize] = value as u8;
    }
    // the square wave, 16-bit, high in memory
    for i in 0..1024u32 {
        let value: i16 = if (i / 64) % 2 == 0 { 12000 } else { -9000 };
        let at = 0x7f000 + 2 * i as usize;
        image[at] = (value as u16 >> 8) as u8;
        image[at + 1] = value as u8;
    }

    let headers: [(usize, Header); 14] = [
        (
            0,
            Header {
                format: 0,
                start: 0x2000,
                looppos: 0,
                end: 256,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
        (
            1,
            Header {
                format: 1,
                start: 0x2100,
                looppos: 128,
                end: 512,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
        (
            2,
            Header {
                format: 2,
                start: 0x2400,
                looppos: 512,
                end: 1024,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
        (
            3,
            Header {
                format: 0,
                start: 0x2c00,
                looppos: 4095,
                end: 4096,
                params: [0x00, 0xf2, 0x46, 0xfb, 0x00],
            },
        ),
        (
            4,
            Header {
                format: 3,
                start: 0x2100,
                looppos: 0,
                end: 512,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
        (
            5,
            Header {
                format: 0,
                start: 0x2000,
                looppos: 200,
                end: 100,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
        (
            6,
            Header {
                format: 0,
                start: 0x2000,
                looppos: 3,
                end: 0,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
        (
            7,
            Header {
                format: 2,
                start: 0x7f000,
                looppos: 0,
                end: 1024,
                params: [0x00, 0x9b, 0x5a, 0x3d, 0x00],
            },
        ),
        (
            8,
            Header {
                format: 2,
                start: 0x2400,
                looppos: 512,
                end: 1024,
                params: [0x2b, 0xe7, 0x35, 0x7c, 0x05],
            },
        ),
        (
            384,
            Header {
                format: 0,
                start: 0x2000,
                looppos: 64,
                end: 128,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
        (
            385,
            Header {
                format: 1,
                start: 0x2100,
                looppos: 0,
                end: 256,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
        (
            511,
            Header {
                format: 2,
                start: 0x2400,
                looppos: 0,
                end: 256,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
        (
            0x80000,
            Header {
                format: 2,
                start: 0x7f000,
                looppos: 256,
                end: 1024,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
        (
            0x8000c,
            Header {
                format: 0,
                start: 0x2c00,
                looppos: 0,
                end: 2048,
                params: [0x00, 0xf0, 0x00, 0xfc, 0x00],
            },
        ),
    ];
    for (at, header) in &headers {
        let at = if *at >= 0x80000 { *at } else { 12 * *at };
        put_header(&mut image, at, header);
    }
    image
}

#[test]
fn the_committed_wavetable_is_the_one_the_tests_build() {
    let path = common::directory("pcm").join("wavetable.bin");
    let image = wavetable();
    if std::env::var_os("YMFM_PCM_WRITE_IMAGE").is_some() {
        std::fs::write(&path, &image).expect("the image writes");
    }
    let committed = std::fs::read(&path).expect("the image reads");
    assert!(committed == image, "tests/pcm/wavetable.bin is stale");
}

/// Holds the log `name` to its golden.
fn check(name: &str) {
    common::check::<Ymf278b<Host>>("pcm", name);
}

#[test]
fn matches_ymfm_on_8_12_and_16_bit_samples() {
    check("pcm-formats");
}

#[test]
fn matches_ymfm_on_loops() {
    check("pcm-loops");
}

#[test]
fn matches_ymfm_on_wave_table_headers_and_banks() {
    check("pcm-headers");
}

#[test]
fn matches_ymfm_on_octaves_and_f_numbers() {
    check("pcm-pitch");
}

#[test]
fn matches_ymfm_on_envelopes_rate_correction_and_reverb() {
    check("pcm-envelope");
}

#[test]
fn matches_ymfm_on_damping() {
    check("pcm-damp");
}

#[test]
fn matches_ymfm_on_the_lfo_vibrato_and_am() {
    check("pcm-lfo");
}

#[test]
fn matches_ymfm_on_panning_levels_and_the_mix() {
    check("pcm-pan-level");
}

#[test]
fn matches_ymfm_on_memory_reads_and_writes() {
    check("pcm-memory");
}

#[test]
fn matches_ymfm_on_status_busy_load_and_timers() {
    check("pcm-status");
}

#[test]
fn matches_ymfm_on_the_fm_side_and_its_mix() {
    check("pcm-fm-mix");
}

#[test]
fn matches_ymfm_when_every_channel_is_loud() {
    check("pcm-loud");
}

#[test]
fn reads_and_writes_memory_through_the_interface() {
    let mut chip = Ymf278b::new(Host::new());
    // NEW and NEW2, then memory access mode at address 0x123456
    for (offset, data) in [(2, 0x05), (3, 0x03), (4, 0x02), (5, 0x01)] {
        chip.write(offset, data);
    }
    for (register, value) in [(0x03, 0x12), (0x04, 0x34), (0x05, 0x56)] {
        chip.write(4, register);
        chip.write(5, value);
    }
    chip.write(4, 0x06);
    chip.write(5, 0xab);
    chip.write(5, 0xcd);
    let memory = &chip.intf().memory[AccessClass::Pcm as usize];
    assert_eq!(memory[0x123456], 0xab);
    assert_eq!(memory[0x123457], 0xcd);
    chip.write(4, 0x05);
    assert_eq!(chip.read(5), 0x58);
    chip.write(5, 0x56);
    chip.write(4, 0x06);
    assert_eq!(chip.read(5), 0xab);
    assert_eq!(chip.read(5), 0xcd);
}

#[test]
fn resets_to_the_state_a_new_chip_starts_in() {
    let log = common::read_log("pcm", "pcm-formats");
    let writes: Vec<(u32, u8)> = log
        .commands
        .iter()
        .flat_map(|command| match &command.action {
            common::Action::Write(pairs) => pairs.clone(),
            common::Action::Read(_) => Vec::new(),
        })
        .collect();
    let mut host = Host::new();
    host.memory[AccessClass::Pcm as usize] = wavetable();
    let mut used = Ymf278b::new(host.clone());
    let mut out = [Output::<6>::default()];
    for (offset, data) in &writes {
        used.write(*offset, *data);
        used.generate(&mut out);
    }
    used.reset();
    let fresh = Ymf278b::new(host);
    assert_eq!(used.registers(), fresh.registers());
    assert_eq!(used.pcm_registers(), fresh.pcm_registers());
    // ymfm's reset leaves the envelope counters and the FM resampling
    // position as they were, so the two chips need not render alike from
    // here; a reset chip is silent until it is written, though.
    for _ in 0..1000 {
        used.generate(&mut out);
        assert_eq!(out[0], Output::<6>::default());
    }
}

/// Where the randomized test writes its memory images.
fn image_directory() -> PathBuf {
    std::env::temp_dir().join(format!("ymfm-oxide-pcm-images-{}", std::process::id()))
}

/// Builds a random memory image: random samples, and random headers for
/// every wave table number of bank 0 (and, in a large image, bank 1),
/// pointing mostly inside the image with loops and ends mostly short
/// enough to wrap.
fn random_image(random: &mut Random) -> Vec<u8> {
    let large = random.below(4) == 0;
    let size = if large {
        IMAGE_SIZE
    } else {
        0x4000 + 0x1000 * random.below(12) as usize
    };
    let mut image: Vec<u8> = (0..size).map(|_| random.byte()).collect();
    let header = |image: &mut Vec<u8>, at: usize, random: &mut Random| {
        let start = if random.below(16) == 0 {
            random.next() & 0x3f_ffff
        } else {
            0x1800 + random.below(size as u32 - 0x1800)
        };
        let end = match random.below(8) {
            0 => random.next() as u16,
            1 => 0,
            _ => 1 + random.below(2048) as u16,
        };
        let looppos = match random.below(4) {
            0 => random.next() as u16,
            _ => random.below(u32::from(end).max(1)) as u16,
        };
        let params = [
            random.byte() & 0x3f,
            random.byte(),
            random.byte(),
            random.byte(),
            random.byte() & 0x07,
        ];
        let header = Header {
            format: random.below(4) as u8,
            start,
            looppos,
            end,
            params,
        };
        put_header(image, at, &header);
    };
    for wavnum in 0..512 {
        header(&mut image, 12 * wavnum, random);
    }
    if large {
        for wavnum in 0..32 {
            header(&mut image, 0x80000 + 12 * wavnum, random);
        }
    }
    image
}

/// Writes a random YMF278B log and its memory image: bursts of writes to
/// every PCM register class, biased towards wave table loads and key-ons
/// so that something sounds most of the time, with memory access, status
/// and data-port reads, the mix control, and FM voices and timers mixed
/// in.
fn random_log(seed: u64, samples: u32) -> String {
    let mut random = Random::new(seed);
    let directory = image_directory();
    std::fs::create_dir_all(&directory).expect("an image directory");
    let image_path = directory.join(format!("pcm-{seed}.bin"));
    std::fs::write(&image_path, random_image(&mut random)).expect("the image writes");

    let mut text = format!(
        "chip ymf278b\nclock 33868800\nsamples {samples}\nmem pcm {}\n",
        image_path.display()
    );
    let mut sample = 0u32;
    // NEW and NEW2 most of the time, so the PCM side listens
    match random.below(8) {
        0 => {}
        1 => {
            let _ = writeln!(text, "0 w 2 05 3 01");
        }
        _ => {
            let _ = writeln!(text, "0 w 2 05 3 03");
        }
    }
    let pcm = |text: &mut String, sample: u32, register: u32, value: u32| {
        let _ = writeln!(text, "{sample} w 4 {register:02x} 5 {value:02x}");
    };
    while sample < samples {
        let burst = 1 + random.below(24);
        for _ in 0..burst {
            let channel = random.below(24);
            match random.below(24) {
                // wave table number, which loads a header
                0..=2 => {
                    if random.below(2) == 0 {
                        pcm(&mut text, sample, 0x20 + channel, random.next() & 0xff);
                    }
                    pcm(&mut text, sample, 0x08 + channel, random.next() & 0xff);
                }
                // F-number and octave
                3 => pcm(&mut text, sample, 0x20 + channel, random.next() & 0xff),
                4 => pcm(&mut text, sample, 0x38 + channel, random.next() & 0xff),
                // level
                5 => pcm(&mut text, sample, 0x50 + channel, random.next() & 0xff),
                // key on, damp, LFO reset, output channel, panpot
                6..=8 => pcm(&mut text, sample, 0x68 + channel, random.next() & 0xff),
                9 => pcm(
                    &mut text,
                    sample,
                    0x68 + channel,
                    0x80 | (random.next() & 0x3f),
                ),
                // LFO and vibrato, envelope, AM
                10 => pcm(&mut text, sample, 0x80 + channel, random.next() & 0xff),
                11 => pcm(&mut text, sample, 0x98 + channel, random.next() & 0xff),
                12 => pcm(&mut text, sample, 0xb0 + channel, random.next() & 0xff),
                13 => pcm(&mut text, sample, 0xc8 + channel, random.next() & 0xff),
                14 => pcm(&mut text, sample, 0xe0 + channel, random.next() & 0xff),
                // the mix control
                15 => pcm(
                    &mut text,
                    sample,
                    0xf8 + random.below(2),
                    random.next() & 0x3f,
                ),
                // memory access: the mode and bank, an address, data
                16 => match random.below(5) {
                    0 => pcm(&mut text, sample, 0x02, random.next() & 0x1f),
                    1 => pcm(&mut text, sample, 0x02, 0x01 | (random.next() & 0x1e)),
                    2 => pcm(
                        &mut text,
                        sample,
                        0x03 + random.below(3),
                        random.next() & 0xff,
                    ),
                    3 => {
                        let _ = writeln!(text, "{sample} w 4 06");
                        for _ in 0..1 + random.below(6) {
                            let _ = writeln!(text, "{sample} r 5");
                        }
                    }
                    _ => {
                        let _ = write!(text, "{sample} w 4 06");
                        for _ in 0..1 + random.below(6) {
                            let _ = write!(text, " 5 {:02x}", random.byte());
                        }
                        let _ = writeln!(text);
                    }
                },
                // a register read back through the data port
                17 => {
                    let _ = writeln!(text, "{sample} w 4 {:02x}", random.byte());
                    let _ = writeln!(text, "{sample} r 5");
                }
                // the status register
                18 => {
                    let _ = writeln!(text, "{sample} r 0");
                }
                // an FM voice
                19 => {
                    let bank = random.below(2);
                    let port = 2 * bank;
                    let ch = random.below(9);
                    let op = [0, 1, 2, 8, 9, 10, 16, 17, 18][ch as usize];
                    let _ = writeln!(
                        text,
                        "{sample} w {port:x} {:02x} {:x} {:02x} {port:x} {:02x} {:x} 00 {port:x} {:02x} {:x} {:02x} {port:x} {:02x} {:x} {:02x} {port:x} {:02x} {:x} {:02x} {port:x} {:02x} {:x} {:02x}",
                        0x20 + op + 3, port + 1, random.next() & 0xff,
                        0x40 + op + 3, port + 1,
                        0x60 + op + 3, port + 1, random.next() & 0xff | 0x80,
                        0xc0 + ch, port + 1, random.next() & 0xff,
                        0xa0 + ch, port + 1, random.next() & 0xff,
                        0xb0 + ch, port + 1, random.next() & 0x3f,
                    );
                }
                // FM system registers: the timers, NEW and NEW2, 4-op mode
                20 => {
                    let (port, register, value) = match random.below(6) {
                        0 => (0, 0x02 + random.below(2), random.next() & 0xff),
                        1 | 2 => (0, 0x04, random.next() & 0xe3),
                        3 => (2, 0x05, random.next() & 0x03),
                        4 => (2, 0x04, random.next() & 0x3f),
                        _ => (0, 0xbd, random.next() & 0xff),
                    };
                    let _ = writeln!(
                        text,
                        "{sample} w {port:x} {register:02x} {:x} {value:02x}",
                        port + 1
                    );
                }
                // any PCM register at all
                21 => pcm(&mut text, sample, random.below(0x100), random.next() & 0xff),
                // a key on that should sound: a full level and a direct jump
                _ => {
                    pcm(&mut text, sample, 0x50 + channel, random.next() & 0x1f);
                    pcm(
                        &mut text,
                        sample,
                        0x68 + channel,
                        0x80 | (random.next() & 0x1f),
                    );
                }
            }
        }
        sample += random.below(1500);
    }
    text
}

#[test]
fn matches_ymfm_on_random_ymf278b_logs_when_an_oracle_is_named() {
    common::differential::<Ymf278b<Host>>("ymf278b", |seed| random_log(seed, 12_000));
    // a failure panics above and keeps the images with the logs
    if std::env::var_os("YMFM_KEEP").is_none() {
        std::fs::remove_dir_all(image_directory()).ok();
    }
}
