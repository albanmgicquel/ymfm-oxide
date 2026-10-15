# ymfm-oxide

A Yamaha OPL2 (YM3812) and OPL3 (YMF262) FM synthesizer in pure Rust: no
dependencies, `#![no_std]`, no allocation. You write registers the way a DOS
driver does and pull samples at the chip's own rate, 49 716 Hz.

## Credit

The emulation is a Rust translation of the OPL core of
[ymfm](https://github.com/aaronsgiles/ymfm) by Aaron Giles (BSD-3-Clause), at
commit `81aec25`. `src/fm.rs` translates its FM engine (`ymfm.h`,
`ymfm_fm.h`, `ymfm_fm.ipp`) and `src/registers.rs` its OPL register file and
chip front ends (`ymfm_opl.h`, `ymfm_opl.cpp`). Both files keep the original
copyright notice, conditions and disclaimer, along with a note on what the
translation changed. `src/lib.rs`, the tests and the oracle are new code.

## What it covers

- **OPL2**: nine 2-operator channels, four waveforms (behind the 0x01
  waveform-select bit), mono output.
- **OPL3**: eighteen channels, which pair into 4-operator voices through
  0x104; eight waveforms; the 0x105 NEW bit, which gates the second register
  bank and the OPL3 features; and stereo from the pan bits of 0xC0 to 0xC8.
- On both chips: feedback, every connection, the envelope with key scaling,
  tremolo and vibrato at both depths, and rhythm mode.

It leaves out everything that is read back rather than heard: the status
register, timers, IRQs and busy time.

## Usage

```toml
[dependencies]
ymfm-oxide = { git = "https://github.com/albanmgicquel/ymfm-oxide" }
```

```rust
use ymfm_oxide::{Chip, Kind, RATE};

let mut chip = Chip::new(Kind::Opl3);
// A held sine on channel 0, about 440 Hz, keyed on.
for (register, value) in [
    (0x20, 0x01), (0x23, 0x21), (0x40, 0x3f), (0x43, 0x00),
    (0x63, 0xf0), (0x83, 0x0f), (0xc0, 0x30), (0xa0, 0x41), (0xb0, 0x32),
] {
    chip.write(register, value);
}
// A block of stereo frames at the chip's rate, RATE per second.
let mut out = [(0i16, 0i16); 1024];
for frame in out.iter_mut() {
    *frame = chip.generate();
}
chip.reset();
```

`Chip::write` takes the full register address (0x000 to 0x0FF on the OPL2, up
to 0x1FF on the OPL3); `Kind::last_register` gives the bound. `Chip::generate`
returns one `(left, right)` frame; the OPL2 repeats its mono output on both
sides. `Chip::reset` returns the chip to its power-on state and
`Chip::registers` exposes the register file for diagnostics. Resampling to an
output device's rate is left to the caller.

## Verification

The output is held to ymfm sample for sample. `tests/opl/` holds 14 synthetic
register logs (every waveform, 2- and 4-operator connections, feedback,
envelopes, key scaling, both LFOs, rhythm mode on both chips, OPL3 panning and
the NEW bit, key timing, and a full chip loud enough to clip), each with a
golden render made by the original C++. `cargo test` replays every log and
compares every sample; a mismatch reports the first differing sample, the
writes that led to it and the register file at that point.

The goldens come from `tests/opl/oracle.cpp`, which renders a log through
ymfm. Cargo never builds it. To build it and regenerate the goldens:

```sh
YMFM=/path/to/ymfm   # a checkout at 81aec25
cd tests/opl
c++ -std=c++17 -O2 -I"$YMFM/src" oracle.cpp "$YMFM/src/ymfm_opl.cpp" \
    "$YMFM/src/ymfm_adpcm.cpp" "$YMFM/src/ymfm_pcm.cpp" -o oracle
for log in *.log; do ./oracle "$log" > "${log%.log}.golden"; done
```

With the oracle built, a randomized comparison runs as well: it writes random
register logs, renders each through both, and compares them.

```sh
YMFM_ORACLE=$PWD/tests/opl/oracle YMFM_CASES=200 cargo test
```

Without `YMFM_ORACLE` that test returns at once. Failing logs are kept in
a temporary directory, so a seed can be replayed.

## Licence

BSD-3-Clause; see [LICENSE](LICENSE).
