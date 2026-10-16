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
| `ssg` | `ymfm_ssg.h`, `ymfm_ssg.cpp` | the SSG engine, with its override hook |
| `adpcm` | `ymfm_adpcm.h`, `ymfm_adpcm.cpp` | the ADPCM-A and ADPCM-B engines |
| `pcm` | `ymfm_pcm.h`, `ymfm_pcm.cpp` | the OPL4's PCM wavetable engine |
| `misc` | `ymfm_misc.h`, `ymfm_misc.cpp` | YM2149 (SSG) |
| `opl` | `ymfm_opl.h`, `ymfm_opl.cpp` | YM3526 (OPL), Y8950 (MSX-Audio), YM3812 (OPL2), YMF262 (OPL3), YMF289B (OPL3L), YMF278B (OPL4), YM2413 (OPLL), YM2423 (OPLL-X), YMF281 (OPLLP), DS1001 (Konami VRC7) |
| `opm` | `ymfm_opm.h`, `ymfm_opm.cpp` | YM2151 (OPM), YM2164 (OPP) |
| `opn` | `ymfm_opn.h`, `ymfm_opn.cpp` | YM2612 (OPN2), YM3438 (OPN2C), YMF276 (OPN2L) |
| `opq` | `ymfm_opq.h`, `ymfm_opq.cpp` | YM3806 (OPQ), YM3533 |
| `opz` | `ymfm_opz.h`, `ymfm_opz.cpp` | YM2414 (OPZ) |

Still to come, in `opn`: the chips that combine the FM engine with the SSG
and ADPCM, the YM2203 (OPN), YM2608 (OPNA), YMF288 (OPN3L), YM2610 (OPNB)
and YM2610B (OPNB2), with the fidelity setting and the SSG resampler they
share.

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
  with a golden render made by the original C++: 17 for the OPL, 12 for the
  OPLL, 1 for the YMF289B, 8 for the Y8950 and 4 for the ADPCM engines
  alone, 12 for the OPL4, 13 for the OPM, 19 for the OPN, 14 for the OPQ,
  16 for the OPZ and 5 for the SSG. `cargo test` replays every log and
  compares everything; a mismatch reports the first difference, the
  commands that led to it and the register file at that point. Memory
  images are synthetic too: the tests build them (ADPCM by encoding sines,
  ramps and noise, the OPL4 wavetable from generated waves) and check the
  committed files against what they build, so no sample data comes from
  anywhere else.
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

The ADPCM engines also have an oracle of their own, `tests/adpcm/engines.cpp`,
which drives upstream's ADPCM-A and ADPCM-B engines outside any chip (ADPCM-B
with the fixed address shift, right pan, limit register and read path the
Y8950 does not expose). It renders the `engine-*` goldens, and with
`YMFM_ENGINE_ORACLE` naming it the engines' randomized tests run too:

```sh
c++ -std=c++17 -O2 -I"$YMFM/src" tests/adpcm/engines.cpp \
    "$YMFM"/src/ymfm_adpcm.cpp -o oracle/engines
YMFM_ENGINE_ORACLE=$PWD/oracle/engines cargo test --test adpcm
```

`YMFM_WRITE_IMAGES` rewrites the synthetic ADPCM images in `tests/adpcm`,
and `YMFM_PCM_WRITE_IMAGE` the OPL4 wavetable `tests/pcm/wavetable.bin`,
from the generators in the tests; without them the tests only check that
the committed images are the generated ones.

A log is text: a header (`chip ym2151`, `clock 3579545`, `samples 30000`,
and `mem <class> <file>` for an external memory image), then commands led by
the sample they happen before, `<sample> w <offset> <data> ...` for port
writes and `<sample> r <offset>` for a read, offsets and data in hex.

## Licence

BSD-3-Clause; see [LICENSE](LICENSE).
