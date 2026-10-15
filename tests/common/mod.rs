// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! The harness every chip's tests share: the register log reader, a host
//! interface with the oracle's time model, the golden reader, the replay
//! that compares a chip with a render sample by sample, and the randomized
//! differential driver.
//!
//! The formats and the time model are the ones `oracle/oracle.cpp`
//! documents; this is their Rust side, and the two must stay in step.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ymfm_oxide::{AccessClass, Interface};

/// A time that never comes.
const NEVER: u64 = u64::MAX;

/// The largest external memory an external write grows an image to.
const MEMORY_LIMIT: u32 = 1 << 24;

/// One recorded event: a read's result or an external write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub sample: u32,
    /// `b'r'` for a read, `b'x'` for an external write.
    pub kind: u8,
    /// The access class, or 0xff for a read.
    pub access: u8,
    /// The address, or the offset for a read.
    pub address: u32,
    pub value: u8,
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.kind == b'r' {
            write!(
                f,
                "read of offset {:x} at sample {} gave {:02x}",
                self.address, self.sample, self.value
            )
        } else {
            write!(
                f,
                "external write of {:02x} to class {} address {:x} at sample {}",
                self.value, self.access, self.address, self.sample
            )
        }
    }
}

/// The host side of the interface, with the oracle's time model.
#[derive(Clone, Debug)]
pub struct Host {
    pub now: u64,
    pub deadline: [u64; 2],
    pub busy_end: u64,
    pub irq: bool,
    pub sample: u32,
    pub memory: [Vec<u8>; 4],
    pub events: Vec<Event>,
}

impl Host {
    pub fn new() -> Host {
        Host {
            now: 0,
            deadline: [NEVER, NEVER],
            busy_end: 0,
            irq: false,
            sample: 0,
            memory: Default::default(),
            events: Vec::new(),
        }
    }

    /// Returns the timer due next, if one is due now: the earliest, timer 0
    /// first on a tie.
    fn due(&self) -> Option<usize> {
        let which = if self.deadline[1] < self.deadline[0] {
            1
        } else {
            0
        };
        (self.deadline[which] != NEVER && self.deadline[which] <= self.now).then_some(which)
    }
}

impl Default for Host {
    fn default() -> Self {
        Host::new()
    }
}

impl Interface for Host {
    fn set_timer(&mut self, tnum: u32, duration_in_clocks: i32) {
        self.deadline[tnum as usize] = if duration_in_clocks < 0 {
            NEVER
        } else {
            self.now + duration_in_clocks as u64
        };
    }

    fn set_busy_end(&mut self, clocks: u32) {
        self.busy_end = self.now + u64::from(clocks);
    }

    fn is_busy(&mut self) -> bool {
        self.now < self.busy_end
    }

    fn update_irq(&mut self, asserted: bool) {
        self.irq = asserted;
    }

    fn external_read(&mut self, access: AccessClass, address: u32) -> u8 {
        self.memory[access as usize]
            .get(address as usize)
            .copied()
            .unwrap_or(0)
    }

    fn external_write(&mut self, access: AccessClass, address: u32, data: u8) {
        let image = &mut self.memory[access as usize];
        if address < MEMORY_LIMIT {
            if address as usize >= image.len() {
                image.resize(address as usize + 1, 0);
            }
            image[address as usize] = data;
        }
        self.events.push(Event {
            sample: self.sample,
            kind: b'x',
            access: access as u8,
            address,
            value: data,
        });
    }
}

/// A chip the harness can drive, over a `Host`.
pub trait Chip {
    /// The number of outputs.
    const OUTPUTS: usize;
    fn create(host: Host) -> Self;
    fn reset(&mut self);
    fn write(&mut self, offset: u32, data: u8);
    fn read(&mut self, offset: u32) -> u8;
    /// Generates one sample into `output`, which holds `OUTPUTS` values.
    fn generate(&mut self, output: &mut [i32]);
    fn sample_rate(&self, clock: u32) -> u32;
    fn timer_expired(&mut self, tnum: u32);
    fn host(&mut self) -> &mut Host;
    fn registers(&self) -> &[u8];
}

/// Implements `Chip` for one of the crate's chips over `Host`.
macro_rules! harness_chip {
    ($chip:ty, $outputs:expr) => {
        impl crate::common::Chip for $chip {
            const OUTPUTS: usize = $outputs;
            fn create(host: crate::common::Host) -> Self {
                <$chip>::new(host)
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
                let mut out = [ymfm_oxide::Output::<$outputs>::default()];
                <$chip>::generate(self, &mut out);
                output.copy_from_slice(&out[0].data);
            }
            fn sample_rate(&self, clock: u32) -> u32 {
                <$chip>::sample_rate(self, clock)
            }
            fn timer_expired(&mut self, tnum: u32) {
                <$chip>::timer_expired(self, tnum)
            }
            fn host(&mut self) -> &mut crate::common::Host {
                <$chip>::intf_mut(self)
            }
            fn registers(&self) -> &[u8] {
                <$chip>::registers(self)
            }
        }
    };
}

/// One command of a log.
#[derive(Clone, Debug)]
pub enum Action {
    /// write(offset, data) for each pair, in order.
    Write(Vec<(u32, u8)>),
    /// read(offset), recorded.
    Read(u32),
}

#[derive(Clone, Debug)]
pub struct Command {
    pub sample: u32,
    pub action: Action,
}

impl std::fmt::Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:>7}", self.sample)?;
        match &self.action {
            Action::Write(pairs) => {
                write!(f, " w")?;
                for (offset, data) in pairs {
                    write!(f, " {offset:x} {data:02x}")?;
                }
                Ok(())
            }
            Action::Read(offset) => write!(f, " r {offset:x}"),
        }
    }
}

/// A chip-tagged register log.
#[derive(Clone, Debug)]
pub struct Log {
    pub chip: String,
    pub clock: u32,
    pub samples: u32,
    pub memory: Vec<(AccessClass, Vec<u8>)>,
    pub commands: Vec<Command>,
}

fn access_class(name: &str) -> AccessClass {
    match name {
        "io" => AccessClass::Io,
        "adpcm-a" => AccessClass::AdpcmA,
        "adpcm-b" => AccessClass::AdpcmB,
        "pcm" => AccessClass::Pcm,
        other => panic!("unknown access class {other}"),
    }
}

/// Reads a log; `directory` resolves the paths of memory images.
pub fn parse(text: &str, directory: &Path) -> Log {
    let mut log = Log {
        chip: String::new(),
        clock: 0,
        samples: 0,
        memory: Vec::new(),
        commands: Vec::new(),
    };
    let mut last = 0;
    for line in text.lines() {
        let body = line.split('#').next().unwrap_or("");
        let words: Vec<&str> = body.split_whitespace().collect();
        let Some(first) = words.first() else {
            continue;
        };
        match (*first, words.len()) {
            ("chip", 2) => log.chip = words[1].to_string(),
            ("clock", 2) => log.clock = words[1].parse().expect("a clock"),
            ("samples", 2) => log.samples = words[1].parse().expect("a sample count"),
            ("mem", 3) => {
                let path = directory.join(words[2]);
                let image = std::fs::read(&path)
                    .unwrap_or_else(|_| panic!("cannot read {}", path.display()));
                log.memory.push((access_class(words[1]), image));
            }
            _ if first.starts_with(|c: char| c.is_ascii_digit()) && words.len() >= 3 => {
                let sample: u32 = first.parse().expect("a sample");
                assert!(sample >= last, "commands go back in time at {sample}");
                last = sample;
                let hex = |word: &str| u32::from_str_radix(word, 16).expect("a hex value");
                let action = match (words[1], words.len()) {
                    ("w", n) if n % 2 == 0 => Action::Write(
                        words[2..]
                            .chunks(2)
                            .map(|pair| (hex(pair[0]), hex(pair[1]) as u8))
                            .collect(),
                    ),
                    ("r", 3) => Action::Read(hex(words[2])),
                    _ => panic!("cannot read the command: {line}"),
                };
                log.commands.push(Command { sample, action });
            }
            _ => panic!("cannot read the line: {line}"),
        }
    }
    assert!(
        !log.chip.is_empty() && log.clock != 0,
        "the log needs a chip and a clock"
    );
    log
}

/// What a chip produced from a log: every output of every sample, the IRQ
/// line after each sample, and the events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Render {
    pub outputs: usize,
    pub samples: Vec<i16>,
    pub irq: Vec<u8>,
    pub events: Vec<Event>,
}

/// Reads a render in the oracle's binary format.
pub fn decode(bytes: &[u8], outputs: usize, samples: usize) -> Result<Render, String> {
    let stride = outputs * 2 + 1;
    let body = samples * stride;
    if bytes.len() < body + 4 {
        return Err(format!(
            "the render holds {} bytes, short of {} samples of {outputs} outputs",
            bytes.len(),
            samples
        ));
    }
    let mut render = Render {
        outputs,
        samples: Vec::with_capacity(samples * outputs),
        irq: Vec::with_capacity(samples),
        events: Vec::new(),
    };
    for record in bytes[..body].chunks_exact(stride) {
        for pair in record[..outputs * 2].as_chunks::<2>().0 {
            render.samples.push(i16::from_le_bytes([pair[0], pair[1]]));
        }
        render.irq.push(record[outputs * 2]);
    }
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let count = u32_at(body) as usize;
    if bytes.len() != body + 4 + count * 11 {
        return Err(format!(
            "the render's {count} events do not fill its {} bytes",
            bytes.len()
        ));
    }
    for at in (body + 4..bytes.len()).step_by(11) {
        render.events.push(Event {
            sample: u32_at(at),
            kind: bytes[at + 4],
            access: bytes[at + 5],
            address: u32_at(at + 6),
            value: bytes[at + 10],
        });
    }
    Ok(render)
}

/// Describes the chip at a mismatch: the last commands and every register
/// that is not zero.
fn describe<C: Chip>(chip: &C, log: &Log, applied: usize) -> String {
    let mut text = String::new();
    let from = applied.saturating_sub(12);
    let _ = writeln!(text, "the last commands applied, of {applied}:");
    for command in &log.commands[from..applied] {
        let _ = writeln!(text, "    {command}");
    }
    let _ = write!(text, "nonzero registers:");
    let mut column = 0;
    for (register, value) in chip.registers().iter().enumerate() {
        if *value != 0 {
            if column % 16 == 0 {
                let _ = write!(text, "\n   ");
            }
            let _ = write!(text, " {register:03x}={value:02x}");
            column += 1;
        }
    }
    text
}

/// Replays a log through `C` and compares everything it produces with
/// `expected`, returning the first mismatch described.
pub fn compare<C: Chip>(log: &Log, expected: &Render) -> Result<(), String> {
    let outputs = C::OUTPUTS;
    if expected.outputs != outputs || expected.irq.len() != log.samples as usize {
        return Err(format!(
            "the render holds {} samples of {} outputs; the log asks for {} of {outputs}",
            expected.irq.len(),
            expected.outputs,
            log.samples
        ));
    }
    let mut host = Host::new();
    for (access, image) in &log.memory {
        host.memory[*access as usize] = image.clone();
    }
    let mut chip = C::create(host);
    chip.reset();
    let mut next = 0;
    let mut checked_events = 0;
    let mut output = vec![0i32; outputs];
    for sample in 0..log.samples {
        chip.host().sample = sample;
        while let Some(which) = chip.host().due() {
            let host = chip.host();
            let saved = host.now;
            host.now = host.deadline[which];
            host.deadline[which] = NEVER;
            chip.timer_expired(which as u32);
            chip.host().now = saved;
        }
        while next < log.commands.len() && log.commands[next].sample <= sample {
            match &log.commands[next].action {
                Action::Write(pairs) => {
                    for (offset, data) in pairs {
                        chip.write(*offset, *data);
                    }
                }
                Action::Read(offset) => {
                    let value = chip.read(*offset);
                    chip.host().events.push(Event {
                        sample,
                        kind: b'r',
                        access: 0xff,
                        address: *offset,
                        value,
                    });
                }
            }
            next += 1;
        }
        chip.generate(&mut output);

        let base = sample as usize * outputs;
        for (index, value) in output.iter().enumerate() {
            let want = expected.samples[base + index];
            if *value != i32::from(want) {
                return Err(format!(
                    "sample {sample}, output {index}: ymfm gives {want} and this gives {value}\n{}",
                    describe(&chip, log, next)
                ));
            }
        }
        let irq = u8::from(chip.host().irq);
        if irq != expected.irq[sample as usize] {
            return Err(format!(
                "sample {sample}: ymfm's IRQ line is {} and this one is {irq}\n{}",
                expected.irq[sample as usize],
                describe(&chip, log, next)
            ));
        }
        let events = chip.host().events.clone();
        for event in &events[checked_events..] {
            match expected.events.get(checked_events) {
                Some(want) if want == event => {}
                Some(want) => {
                    return Err(format!(
                        "event {checked_events}: ymfm gives the {want} and this the {event}\n{}",
                        describe(&chip, log, next)
                    ))
                }
                None => {
                    return Err(format!(
                        "event {checked_events}: ymfm stops and this gives the {event}\n{}",
                        describe(&chip, log, next)
                    ))
                }
            }
            checked_events += 1;
        }

        let rate = chip.sample_rate(log.clock);
        assert!(rate != 0, "the chip reports a sample rate of zero");
        chip.host().now += (u64::from(log.clock) + u64::from(rate / 2)) / u64::from(rate);
    }
    if checked_events != expected.events.len() {
        return Err(format!(
            "ymfm records {} events and this {checked_events}; the next is the {}",
            expected.events.len(),
            expected.events[checked_events]
        ));
    }
    Ok(())
}

/// The directory of a chip family's test data.
pub fn directory(family: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(family)
}

/// Reads the log `name` of a family.
pub fn read_log(family: &str, name: &str) -> Log {
    let base = directory(family);
    let text = std::fs::read_to_string(base.join(format!("{name}.log"))).expect("the log reads");
    parse(&text, &base)
}

/// Replays the log `name` of a family through `C` and holds it to its
/// golden render.
pub fn check<C: Chip>(family: &str, name: &str) {
    let log = read_log(family, name);
    let golden =
        std::fs::read(directory(family).join(format!("{name}.golden"))).expect("the golden reads");
    let expected = decode(&golden, C::OUTPUTS, log.samples as usize)
        .unwrap_or_else(|reason| panic!("{name}: {reason}"));
    // A golden of silence would pass a chip that renders nothing.
    assert!(
        expected.samples.iter().any(|sample| *sample != 0),
        "{name}: the golden is silent"
    );
    if let Err(reason) = compare::<C>(&log, &expected) {
        panic!("{name}: {reason}");
    }
}

/// A small deterministic generator, so a failing seed can be replayed.
pub struct Random(u64);

impl Random {
    pub fn new(seed: u64) -> Random {
        Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }

    pub fn below(&mut self, bound: u32) -> u32 {
        self.next() % bound
    }

    pub fn byte(&mut self) -> u8 {
        self.next() as u8
    }
}

/// The randomized differential test: when `YMFM_ORACLE` names a built
/// oracle, writes `YMFM_CASES` random logs (200 by default, seeds from
/// `YMFM_SEED`, 1 by default) with `generate`, renders each through the
/// oracle, and holds `C` to it. Without `YMFM_ORACLE` it returns at once.
/// Failing logs are kept in a temporary directory, so a seed can be
/// replayed; `generate` must start its log with the `chip` and `clock`
/// lines the oracle needs.
pub fn differential<C: Chip>(label: &str, generate: impl Fn(u64) -> String) {
    let Some(oracle) = std::env::var_os("YMFM_ORACLE") else {
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
    let scratch = std::env::temp_dir().join(format!("ymfm-oxide-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("a scratch directory");
    let mut failures = Vec::new();
    for seed in first..first + count {
        let text = generate(seed);
        let path = scratch.join(format!("{label}-{seed}.log"));
        std::fs::write(&path, &text).expect("the log writes");
        let output = std::process::Command::new(&oracle)
            .arg(&path)
            .output()
            .expect("the oracle runs");
        assert!(
            output.status.success(),
            "the oracle failed on seed {seed}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let log = parse(&text, &scratch);
        let result = decode(&output.stdout, C::OUTPUTS, log.samples as usize)
            .and_then(|expected| compare::<C>(&log, &expected));
        match result {
            Ok(()) => {
                std::fs::remove_file(&path).ok();
            }
            Err(reason) => failures.push(format!("seed {seed}: {reason}")),
        }
    }
    if failures.is_empty() {
        std::fs::remove_dir_all(&scratch).ok();
    }
    assert!(
        failures.is_empty(),
        "{} of {count} {label} logs differ, kept in {}:\n{}",
        failures.len(),
        scratch.display(),
        failures.join("\n\n")
    );
}
