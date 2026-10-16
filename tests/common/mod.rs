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

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ymfm_oxide::ssg::SsgOverride;

pub mod synth;
use ymfm_oxide::{AccessClass, Interface};

/// A time that never comes.
const NEVER: u64 = u64::MAX;

/// The largest external memory an external write grows an image to.
const MEMORY_LIMIT: u32 = 1 << 24;

/// One recorded event: a read's result or an external write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub sample: u32,
    /// `b'r'` for a read, `b'x'` for an external write, `b'o'` for a call
    /// of the SSG override.
    pub kind: u8,
    /// The access class, 0xff for a read, or the override call (0 reset,
    /// 1 read, 2 write, 3 prescale changed).
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
        } else if self.kind == b'o' {
            let call = ["reset", "read", "write", "prescale change"]
                .get(self.access as usize)
                .unwrap_or(&"unknown call");
            write!(
                f,
                "override {call} of register {:x} with {:02x} at sample {}",
                self.address, self.value, self.sample
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
    /// The calls a `Recorder` override has seen and not yet moved to
    /// `events`, as (call, register, value).
    pub ssg_log: Option<OverrideLog>,
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
            ssg_log: None,
        }
    }

    /// Moves the override's calls since the last drain into the events, at
    /// the current sample. The harness drains after every call into the
    /// chip, so the calls keep their order with the chip's other events.
    pub fn drain(&mut self) {
        if let Some(log) = &self.ssg_log {
            for (call, regnum, value) in log.borrow_mut().drain(..) {
                self.events.push(Event {
                    sample: self.sample,
                    kind: b'o',
                    access: call,
                    address: regnum,
                    value,
                });
            }
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

/// The calls an override records, as (call, register, value), shared
/// between the override and its host.
pub type OverrideLog = Rc<RefCell<Vec<(u8, u32, u8)>>>;

/// The oracle's recording SSG override: it keeps its own 16 registers,
/// clears them on a reset, stores writes and answers a read with the
/// register XOR 0xA5, and records every call in its host's `ssg_log`.
#[derive(Clone, Debug)]
pub struct Recorder {
    regs: [u8; 16],
    log: OverrideLog,
}

impl Recorder {
    /// Creates a recorder that records into `host`.
    pub fn new(host: &mut Host) -> Recorder {
        let log = Rc::new(RefCell::new(Vec::new()));
        host.ssg_log = Some(log.clone());
        Recorder { regs: [0; 16], log }
    }
}

impl SsgOverride for Recorder {
    fn ssg_reset(&mut self) {
        self.regs = [0; 16];
        self.log.borrow_mut().push((0, 0, 0));
    }
    fn ssg_read(&mut self, regnum: u32) -> u8 {
        let value = self.regs[(regnum & 0x0f) as usize] ^ 0xa5;
        self.log.borrow_mut().push((1, regnum, value));
        value
    }
    fn ssg_write(&mut self, regnum: u32, data: u8) {
        self.regs[(regnum & 0x0f) as usize] = data;
        self.log.borrow_mut().push((2, regnum, data));
    }
    fn ssg_prescale_changed(&mut self) {
        self.log.borrow_mut().push((3, 0, 0));
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
    /// Whether `create` installs a `Recorder` as the SSG override.
    const SSG_OVERRIDE: bool = false;
    /// Sets the fidelity: 0 the maximum, 1 the minimum, 2 the medium.
    fn set_fidelity(&mut self, _fidelity: u8) {
        panic!("the chip has no fidelity setting");
    }
}

/// Implements `Chip` for one of the crate's chips over `Host`: plain, or
/// with `fidelity` for a chip with an `OpnFidelity`, and with `override`
/// for one whose SSG override is a `Recorder` (`fidelity override` for
/// both).
macro_rules! harness_chip {
    ($chip:ty, $outputs:expr) => {
        harness_chip!(@impl $chip, $outputs, false, {
            fn create(host: crate::common::Host) -> Self {
                <$chip>::new(host)
            }
        });
    };
    ($chip:ty, $outputs:expr, fidelity) => {
        harness_chip!(@impl $chip, $outputs, false, {
            fn create(host: crate::common::Host) -> Self {
                <$chip>::new(host)
            }
            harness_chip!(@fidelity $chip);
        });
    };
    ($chip:ty, $outputs:expr, override) => {
        harness_chip!(@impl $chip, $outputs, true, {
            harness_chip!(@override $chip);
        });
    };
    ($chip:ty, $outputs:expr, fidelity override) => {
        harness_chip!(@impl $chip, $outputs, true, {
            harness_chip!(@override $chip);
            harness_chip!(@fidelity $chip);
        });
    };
    (@override $chip:ty) => {
        fn create(mut host: crate::common::Host) -> Self {
            let recorder = crate::common::Recorder::new(&mut host);
            <$chip>::with_ssg_override(host, recorder)
        }
    };
    (@fidelity $chip:ty) => {
        fn set_fidelity(&mut self, fidelity: u8) {
            <$chip>::set_fidelity(
                self,
                match fidelity {
                    0 => ymfm_oxide::opn::OpnFidelity::Max,
                    1 => ymfm_oxide::opn::OpnFidelity::Min,
                    _ => ymfm_oxide::opn::OpnFidelity::Med,
                },
            )
        }
    };
    (@impl $chip:ty, $outputs:expr, $override:expr, { $($extra:tt)* }) => {
        impl crate::common::Chip for $chip {
            const OUTPUTS: usize = $outputs;
            const SSG_OVERRIDE: bool = $override;
            $($extra)*
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
    /// set_fidelity: 0 the maximum, 1 the minimum, 2 the medium.
    Fidelity(u8),
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
            Action::Fidelity(fidelity) => write!(f, " f {}", FIDELITIES[*fidelity as usize]),
        }
    }
}

/// The fidelities' names in a log, in the order of their numbers.
const FIDELITIES: [&str; 3] = ["max", "min", "med"];

fn fidelity(name: &str) -> u8 {
    FIDELITIES
        .iter()
        .position(|known| *known == name)
        .unwrap_or_else(|| panic!("unknown fidelity {name}")) as u8
}

/// A chip-tagged register log.
#[derive(Clone, Debug)]
pub struct Log {
    pub chip: String,
    pub clock: u32,
    pub samples: u32,
    pub memory: Vec<(AccessClass, Vec<u8>)>,
    /// The fidelity to set before the reset, if any.
    pub fidelity: Option<u8>,
    /// Whether the SSG override is installed.
    pub ssg_override: bool,
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
        fidelity: None,
        ssg_override: false,
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
            ("fidelity", 2) => log.fidelity = Some(fidelity(words[1])),
            ("ssg-override", 1) => log.ssg_override = true,
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
                    ("f", 3) => Action::Fidelity(fidelity(words[2])),
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
    pub samples: Vec<i32>,
    pub irq: Vec<u8>,
    pub events: Vec<Event>,
}

/// The render format's magic and the version this reads.
const RENDER_MAGIC: &[u8; 4] = b"YMFR";
const RENDER_VERSION: u32 = 2;

/// Reads a render in the oracle's binary format, version 2.
pub fn decode(bytes: &[u8], outputs: usize, samples: usize) -> Result<Render, String> {
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    if bytes.len() < 16 || &bytes[..4] != RENDER_MAGIC {
        return Err("the render does not start with the YMFR magic".to_string());
    }
    if u32_at(4) != RENDER_VERSION {
        return Err(format!(
            "the render is in version {} of the format, not {RENDER_VERSION}",
            u32_at(4)
        ));
    }
    if (u32_at(8) as usize, u32_at(12) as usize) != (outputs, samples) {
        return Err(format!(
            "the render holds {} samples of {} outputs; the log asks for {samples} of {outputs}",
            u32_at(12),
            u32_at(8)
        ));
    }
    let stride = outputs * 4 + 1;
    let body = 16 + samples * stride;
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
    for record in bytes[16..body].chunks_exact(stride) {
        for word in record[..outputs * 4].as_chunks::<4>().0 {
            render.samples.push(i32::from_le_bytes(*word));
        }
        render.irq.push(record[outputs * 4]);
    }
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
    assert_eq!(
        log.ssg_override,
        C::SSG_OVERRIDE,
        "the log and the harness disagree on the SSG override"
    );
    // as the oracle: the chip as made (an override installed with the
    // default fidelity, then a reset), the log's fidelity, and a reset
    let mut chip = C::create(host);
    chip.host().drain();
    if let Some(fidelity) = log.fidelity {
        chip.set_fidelity(fidelity);
        chip.host().drain();
    }
    chip.reset();
    chip.host().drain();
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
            chip.host().drain();
            chip.host().now = saved;
        }
        while next < log.commands.len() && log.commands[next].sample <= sample {
            match &log.commands[next].action {
                Action::Write(pairs) => {
                    for (offset, data) in pairs {
                        chip.write(*offset, *data);
                        chip.host().drain();
                    }
                }
                Action::Fidelity(fidelity) => {
                    chip.set_fidelity(*fidelity);
                    chip.host().drain();
                }
                Action::Read(offset) => {
                    let value = chip.read(*offset);
                    chip.host().drain();
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
        chip.host().drain();

        let base = sample as usize * outputs;
        for (index, value) in output.iter().enumerate() {
            let want = expected.samples[base + index];
            if *value != want {
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
/// golden render, which must not be silent.
pub fn check<C: Chip>(family: &str, name: &str) {
    check_render::<C>(family, name, true);
}

/// Replays the log `name` of a family through `C` and holds it to its
/// golden render; with `audible`, a golden of silence fails, since it would
/// pass a chip that renders nothing. A log whose point is its events (an
/// SSG override's calls) can be silent, but must record events.
pub fn check_render<C: Chip>(family: &str, name: &str, audible: bool) {
    let log = read_log(family, name);
    let golden =
        std::fs::read(directory(family).join(format!("{name}.golden"))).expect("the golden reads");
    let expected = decode(&golden, C::OUTPUTS, log.samples as usize)
        .unwrap_or_else(|reason| panic!("{name}: {reason}"));
    if audible {
        assert!(
            expected.samples.iter().any(|sample| *sample != 0),
            "{name}: the golden is silent"
        );
    } else {
        assert!(
            !expected.events.is_empty(),
            "{name}: the golden records nothing"
        );
    }
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
/// replayed, and with `YMFM_KEEP` set every log is; `generate` must start its log with the `chip` and `clock`
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
    let keep = std::env::var_os("YMFM_KEEP").is_some();
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
            Ok(()) if !keep => {
                std::fs::remove_file(&path).ok();
            }
            Ok(()) => {}
            Err(reason) => failures.push(format!("seed {seed}: {reason}")),
        }
    }
    if failures.is_empty() && !keep {
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
