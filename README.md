# ymfm-oxide

A pure-Rust port of [ymfm](https://github.com/aaronsgiles/ymfm), Aaron
Giles's BSD-3-Clause emulator of the Yamaha FM sound chips. No dependencies,
`#![no_std]`, no allocation.

## Credit

The emulation is a translation of ymfm at upstream commit
[`81aec25ccbb98f4873a255f7551ac4dadac59b4a`](https://github.com/aaronsgiles/ymfm/commit/81aec25ccbb98f4873a255f7551ac4dadac59b4a)
(July 2026). Each translated file keeps the original copyright notice,
conditions and disclaimer, and says which upstream files it translates and
what the translation changed. `src/lib.rs`, the tests and the oracle are new
code. This project is not affiliated with or endorsed by the upstream project
or its author.

## What is ported

| Module | Upstream files | Chips |
|---|---|---|
| `ymfm` | `ymfm.h` | the host interface, outputs, DAC helpers |
| `fm` (private) | `ymfm_fm.h`, `ymfm_fm.ipp` | the generic FM engine |
| `opl` | `ymfm_opl.h`, `ymfm_opl.cpp` | YM3526 (OPL), YM3812 (OPL2), YMF262 (OPL3) |
| `opm` | `ymfm_opm.h`, `ymfm_opm.cpp` | YM2151 (OPM), YM2164 (OPP) |

Still to come, each in the module of its upstream file: the OPN family
(`opn`: YM2203, YM2608, YMF288, YM2610, YM2610B, YM2612, YM3438, YMF276) with
`ssg` and `adpcm`; the OPZ (`opz`: YM2414); the OPQ (`opq`: YM3806, YM3533);
the YM2149 (`misc`); and the rest of `opl`: the Y8950, the YMF289B, the
YMF278B with `pcm`, and the OPLL family (YM2413, YM2423, YMF281, DS1001).

Everything a chip does is ported, not only its sound: the status register,
busy time, both timers, the IRQ line, CSM, and external I/O. Save states
are replaced by `Clone`.

## Usage

```toml
[dependencies]
ymfm-oxide = { git = "https://github.com/albanmgicquel/ymfm-oxide" }
```

```rust
use ymfm_oxide::opm::Ym2151;
use ymfm_oxide::{NullInterface, Output};

let mut chip = Ym2151::new(NullInterface);
chip.write(0, 0x20); // address port
chip.write(1, 0xc7); // data port
// ...
let mut block = [Output::<2>::default(); 1024];
chip.generate(&mut block);
let rate = chip.sample_rate(3_579_545); // 55 930 Hz
```

A chip takes writes and reads on its ports, as the hardware does (`write`,
`read`, or `write_address`/`write_data` and `read_status`), and generates
one `Output<N>` per sample, every output the chip has, at
`sample_rate(clock)`. Resampling to a device's rate is the caller's.

The `Interface` trait is the chip's connection to the system: timers
(`set_timer`, answered by calling the chip's `timer_expired` once the
duration has passed), busy time, the IRQ line, and external memory and I/O.
Every method has a default; `NullInterface` uses them all. A chip owns its
interface, and `&mut T` is an interface too.

## Verification

The output is held to the original sample for sample, together with the
IRQ line after every sample, every status read, and every external write.

- **Golden renders.** `tests/<family>/` holds synthetic register logs, each
  with a golden render made by the original C++ (14 + 3 for the OPL, 13 for
  the OPM). `cargo test` replays every log and compares everything; a
  mismatch reports the first difference, the commands that led to it and
  the register file at that point.
- **Randomized differential tests.** With an oracle built, each chip's test
  also writes random logs, renders each through both, and compares.

The oracle, `oracle/oracle.cpp`, is an offline development tool: Cargo never
builds it and it is not part of the package. It replays a chip-tagged log
through any upstream chip; its header documents the log format, the time
model and the output format. Build it against an upstream checkout at the
commit above:

```sh
YMFM=/path/to/ymfm
c++ -std=c++17 -O2 -I"$YMFM/src" oracle/oracle.cpp \
    "$YMFM"/src/ymfm_adpcm.cpp "$YMFM"/src/ymfm_misc.cpp \
    "$YMFM"/src/ymfm_opl.cpp "$YMFM"/src/ymfm_opm.cpp \
    "$YMFM"/src/ymfm_opn.cpp "$YMFM"/src/ymfm_opq.cpp \
    "$YMFM"/src/ymfm_opz.cpp "$YMFM"/src/ymfm_pcm.cpp \
    "$YMFM"/src/ymfm_ssg.cpp -o oracle/oracle
```

Regenerate a golden with `oracle/oracle tests/opm/opm-lfo.log >
tests/opm/opm-lfo.golden`. Run the randomized tests with:

```sh
YMFM_ORACLE=$PWD/oracle/oracle YMFM_CASES=200 cargo test
```

`YMFM_SEED` sets the first seed (1 by default). Without `YMFM_ORACLE` those
tests return at once. Failing logs are kept in a temporary directory so a
seed can be replayed; `YMFM_KEEP` keeps every log.

A log is text: a header (`chip ym2151`, `clock 3579545`, `samples 30000`,
and `mem <class> <file>` for an external memory image), then commands led by
the sample they happen before, `<sample> w <offset> <data> ...` for port
writes and `<sample> r <offset>` for a read, offsets and data in hex.

## Licence

BSD-3-Clause; see [LICENSE](LICENSE).
