// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The SSG held to ymfm, sample for sample, through the YM2149.
//!
//! Each case in `tests/ssg` is a synthetic register log and the render ymfm
//! made of it through `oracle/oracle.cpp`: each channel's tone across the
//! periods and the fixed amplitudes, every noise period, every mixer
//! setting, all sixteen envelope shapes at short and long periods, and the
//! I/O ports in both directions with the read-back of every register and
//! the chip's bus states.
//!
//! With `YMFM_ORACLE` naming a built oracle, a randomized comparison runs as
//! well, over logs this test writes itself (see `common::differential`).

#[allow(unused_macros)]
#[macro_use]
mod common;

use std::fmt::Write as _;

use common::{Host, Random, Recorder};
use ymfm_oxide::misc::Ym2149;
use ymfm_oxide::ssg::{SsgEngine, SsgOverride};
use ymfm_oxide::{NullInterface, Output};

// The YM2149 has no timers, so it cannot take `harness_chip!`, whose chips
// all report timer expiries.
macro_rules! ym2149_harness {
    ($chip:ty, $override:expr, $create:expr) => {
        impl common::Chip for $chip {
            const OUTPUTS: usize = 3;
            const SSG_OVERRIDE: bool = $override;
            fn create(host: Host) -> Self {
                $create(host)
            }
            fn reset(&mut self) {
                <$chip>::reset(self)
            }
            fn write(&mut self, offset: u32, data: u8) {
                <$chip>::write(self, offset, data)
            }
            fn read(&mut self, offset: u32) -> u8 {
                <$chip>::read(self, offset)
            }
            fn generate(&mut self, output: &mut [i32]) {
                let mut out = [Output::<3>::default()];
                <$chip>::generate(self, &mut out);
                output.copy_from_slice(&out[0].data);
            }
            fn sample_rate(&self, clock: u32) -> u32 {
                <$chip>::sample_rate(self, clock)
            }
            fn timer_expired(&mut self, _tnum: u32) {
                unreachable!("the YM2149 sets no timers");
            }
            fn host(&mut self) -> &mut Host {
                self.intf_mut()
            }
            fn registers(&self) -> &[u8] {
                <$chip>::registers(self)
            }
        }
    };
}

ym2149_harness!(Ym2149<Host>, false, Ym2149::new);
ym2149_harness!(
    Ym2149<Host, Recorder>,
    true,
    |mut host: Host| {
        let recorder = Recorder::new(&mut host);
        Ym2149::with_ssg_override(host, recorder)
    }
);

fn check(name: &str) {
    common::check::<Ym2149<Host>>("ssg", name);
}

#[test]
fn matches_ymfm_with_an_override_in_place_of_the_ssg() {
    // the override takes every reset, read and write, and the engine,
    // never written, stays silent
    common::check_render::<Ym2149<Host, Recorder>>("ssg", "ssg-override", false);
}

#[test]
fn matches_ymfm_on_tones() {
    check("ssg-tones");
}

#[test]
fn matches_ymfm_on_noise() {
    check("ssg-noise");
}

#[test]
fn matches_ymfm_on_the_mixer() {
    check("ssg-mixer");
}

#[test]
fn matches_ymfm_on_the_envelope() {
    check("ssg-envelope");
}

#[test]
fn matches_ymfm_on_the_ports_and_read_back() {
    check("ssg-io");
}

#[test]
fn runs_at_a_sixty_fourth_of_its_clock() {
    let chip = Ym2149::new(NullInterface);
    assert_eq!(chip.sample_rate(2_000_000), 31_250);
    assert_eq!(Ym2149::<NullInterface>::OUTPUTS, 3);
}

/// An override that records what reaches it and answers reads itself.
#[derive(Default)]
struct Counter {
    resets: u32,
    prescales: u32,
    writes: Vec<(u32, u8)>,
    reads: Vec<u32>,
}

impl SsgOverride for Counter {
    fn ssg_reset(&mut self) {
        self.resets += 1;
    }
    fn ssg_read(&mut self, regnum: u32) -> u8 {
        self.reads.push(regnum);
        0xa0 | regnum as u8
    }
    fn ssg_write(&mut self, regnum: u32, data: u8) {
        self.writes.push((regnum, data));
    }
    fn ssg_prescale_changed(&mut self) {
        self.prescales += 1;
    }
}

#[test]
fn an_override_takes_resets_reads_and_writes() {
    let mut recorder = Counter::default();
    {
        let mut chip = Ym2149::with_ssg_override(NullInterface, &mut recorder);
        assert!(chip.ssg().overridden());
        chip.write(0, 0x18);
        chip.write(2, 0x0f);
        assert_eq!(chip.read(3), 0xa8);
        chip.reset();
        // the engine's own registers are untouched
        assert!(chip.registers().iter().all(|value| *value == 0));
    }
    assert_eq!(recorder.writes, [(8, 0x0f)]);
    assert_eq!(recorder.reads, [8]);
    // one reset as the chip is made, one asked for
    assert_eq!(recorder.resets, 2);

    let mut chip =
        Ym2149::<NullInterface, Counter>::with_ssg_override(NullInterface, Counter::default());
    chip.ssg_override(Counter::default());
    assert_eq!(chip.ssg().ssg_override().unwrap().resets, 0);

    let mut engine = SsgEngine::<Counter>::new();
    engine.prescale_changed();
    engine.set_override(Counter::default());
    engine.prescale_changed();
    engine.write(&mut NullInterface, 7, 0xff);
    assert_eq!(engine.regs().read(7), 0);
    let recorder = engine.ssg_override().unwrap();
    assert_eq!((recorder.prescales, recorder.writes.len()), (1, 1));
}

/// Writes a random YM2149 log: bursts of writes to every register, biased
/// towards audible settings, through every port offset and with high address
/// bits, mixed with reads of every register.
fn random_log(seed: u64, samples: u32) -> String {
    let mut random = Random::new(seed);
    let mut text = String::from("chip ym2149\nclock 2000000\n");
    let _ = writeln!(text, "samples {samples}");
    // A random input image for the ports, written into the scratch
    // directory `common::differential` keeps its logs in (and removes), and
    // which a log's memory paths are relative to.
    let scratch = std::env::temp_dir().join(format!("ymfm-oxide-ym2149-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("a scratch directory");
    let image = format!("ym2149-{seed}.io");
    std::fs::write(scratch.join(&image), [random.byte(), random.byte()]).expect("the image writes");
    let _ = writeln!(text, "mem io {image}");
    let mut sample = 0u32;
    while sample < samples {
        let burst = 1 + random.below(12);
        for _ in 0..burst {
            let high = if random.below(4) == 0 {
                random.byte() & 0xf0
            } else {
                0
            };
            let address_port = [0, 3][random.below(2) as usize] + 4 * random.below(2);
            let (register, value) = match random.below(16) {
                0..=2 => (2 * random.below(3), random.byte() >> random.below(8)),
                3 => (1 + 2 * random.below(3), random.byte() >> random.below(8)),
                4 => (6, random.byte() >> random.below(4)),
                5 | 6 => (7, random.byte()),
                7..=9 => (8 + random.below(3), random.byte()),
                10 => (11, random.byte() >> random.below(8)),
                11 => (12, random.byte() >> random.below(8)),
                12 => (13, random.byte()),
                13 => (14 + random.below(2), random.byte()),
                14 => {
                    let register = random.below(16) as u8 | high;
                    let _ = writeln!(text, "{sample} w {address_port:x} {register:02x}");
                    let read_port = 3 + 4 * random.below(2);
                    let port = if random.below(8) == 0 {
                        random.below(3)
                    } else {
                        read_port
                    };
                    let _ = writeln!(text, "{sample} r {port:x}");
                    continue;
                }
                _ => {
                    // an inactive write, at offset 1
                    let _ = writeln!(text, "{sample} w 1 {:02x}", random.byte());
                    continue;
                }
            };
            let register = register as u8 | high;
            let data_port = 2 + 4 * random.below(2);
            let _ = writeln!(
                text,
                "{sample} w {address_port:x} {register:02x} {data_port:x} {value:02x}"
            );
        }
        sample += random.below(1200);
    }
    text
}

#[test]
fn matches_ymfm_on_random_ym2149_logs_when_an_oracle_is_named() {
    common::differential::<Ym2149<Host>>("ym2149", |seed| random_log(seed, 20_000));
}

#[test]
fn matches_ymfm_on_random_ym2149_logs_with_an_override_when_an_oracle_is_named() {
    common::differential::<Ym2149<Host, Recorder>>("ym2149-override", |seed| {
        // the override takes the ports, so the log needs no I/O image
        random_log(seed, 5_000)
            .lines()
            .filter(|line| !line.starts_with("mem "))
            .collect::<Vec<_>>()
            .join("\n")
            .replacen("samples", "ssg-override\nsamples", 1)
    });
}
