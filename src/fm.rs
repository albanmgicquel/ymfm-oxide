// This file is a Rust translation of ymfm's `ymfm_fm.h` and `ymfm_fm.ipp` at
// commit 81aec25 (https://github.com/aaronsgiles/ymfm), with changes: the
// register class is a trait, `FmRegisters`, that the engine is generic over;
// operators and channels refer to each other by index into the engine's
// arrays rather than by pointer, and those arrays are sized for the largest
// chip rather than per register class; the engine owns its interface; the
// channel-isolation debug masks, key-on logging, WAV logging and save states
// are gone; and the interface's synchronization hooks are notifications, the
// engine applying the change itself (see `ymfm.rs`). Every computation that
// reaches a sample, a status bit or a timer is kept as ymfm writes it,
// including its integer widths.
//
// The original carries this notice, which applies to this translation:
//
// BSD 3-Clause License
//
// Copyright (c) 2021, Aaron Giles
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this
//    list of conditions and the following disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its
//    contributors may be used to endorse or promote products derived from
//    this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
// FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
// CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
// OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

//! The generic FM engine: operators, channels, and the engine that clocks
//! them, over a family's register set.
//!
//! Arithmetic here is deliberately written in ymfm's integer widths. Where the
//! C++ relies on an implicit conversion, an unsigned wrap or a truncation to
//! sixteen bits, the Rust spells the same operation out, and a comment says so
//! where the reason is not obvious.

use crate::ymfm::{
    bitfield, clamp, Interface, EG_ATTACK, EG_DECAY, EG_DEPRESS, EG_RELEASE, EG_REVERB, EG_STATES,
    EG_SUSTAIN,
};

//*********************************************************
//  GLOBAL ENUMERATORS
//*********************************************************

/// The three key-on sources; the effective key is their union.
pub(crate) const KEYON_NORMAL: u32 = 0;
pub(crate) const KEYON_RHYTHM: u32 = 1;
pub(crate) const KEYON_CSM: u32 = 2;

/// The key-on target a register write reports for the OPL rhythm channels.
pub(crate) const RHYTHM_CHANNEL: u32 = 0xff;

/// The size of a full sine waveform.
pub(crate) const WAVEFORM_LENGTH: usize = 0x400;

/// The most channels and operators any chip has, which size the engine's
/// arrays: the OPL3's 18 and 36.
pub(crate) const MAX_CHANNELS: usize = 18;
pub(crate) const MAX_OPERATORS: usize = 36;

//*********************************************************
//  GLOBAL TABLE LOOKUPS
//*********************************************************

/// Returns the absolute value of sin(input), where 0 to 2 pi maps onto ten
/// bits, as a 4.8 fixed-point logarithmic attenuation.
pub(crate) const fn abs_sin_attenuation(input: u32) -> u32 {
    // The values here are stored as 4.8 logarithmic values for 1/4 phase;
    // this matches the internal format of the OPN chip, extracted from the
    // die.
    const SIN_TABLE: [u16; 256] = [
        0x859, 0x6c3, 0x607, 0x58b, 0x52e, 0x4e4, 0x4a6, 0x471, 0x443, 0x41a, 0x3f5, 0x3d3, 0x3b5,
        0x398, 0x37e, 0x365, 0x34e, 0x339, 0x324, 0x311, 0x2ff, 0x2ed, 0x2dc, 0x2cd, 0x2bd, 0x2af,
        0x2a0, 0x293, 0x286, 0x279, 0x26d, 0x261, 0x256, 0x24b, 0x240, 0x236, 0x22c, 0x222, 0x218,
        0x20f, 0x206, 0x1fd, 0x1f5, 0x1ec, 0x1e4, 0x1dc, 0x1d4, 0x1cd, 0x1c5, 0x1be, 0x1b7, 0x1b0,
        0x1a9, 0x1a2, 0x19b, 0x195, 0x18f, 0x188, 0x182, 0x17c, 0x177, 0x171, 0x16b, 0x166, 0x160,
        0x15b, 0x155, 0x150, 0x14b, 0x146, 0x141, 0x13c, 0x137, 0x133, 0x12e, 0x129, 0x125, 0x121,
        0x11c, 0x118, 0x114, 0x10f, 0x10b, 0x107, 0x103, 0x0ff, 0x0fb, 0x0f8, 0x0f4, 0x0f0, 0x0ec,
        0x0e9, 0x0e5, 0x0e2, 0x0de, 0x0db, 0x0d7, 0x0d4, 0x0d1, 0x0cd, 0x0ca, 0x0c7, 0x0c4, 0x0c1,
        0x0be, 0x0bb, 0x0b8, 0x0b5, 0x0b2, 0x0af, 0x0ac, 0x0a9, 0x0a7, 0x0a4, 0x0a1, 0x09f, 0x09c,
        0x099, 0x097, 0x094, 0x092, 0x08f, 0x08d, 0x08a, 0x088, 0x086, 0x083, 0x081, 0x07f, 0x07d,
        0x07a, 0x078, 0x076, 0x074, 0x072, 0x070, 0x06e, 0x06c, 0x06a, 0x068, 0x066, 0x064, 0x062,
        0x060, 0x05e, 0x05c, 0x05b, 0x059, 0x057, 0x055, 0x053, 0x052, 0x050, 0x04e, 0x04d, 0x04b,
        0x04a, 0x048, 0x046, 0x045, 0x043, 0x042, 0x040, 0x03f, 0x03e, 0x03c, 0x03b, 0x039, 0x038,
        0x037, 0x035, 0x034, 0x033, 0x031, 0x030, 0x02f, 0x02e, 0x02d, 0x02b, 0x02a, 0x029, 0x028,
        0x027, 0x026, 0x025, 0x024, 0x023, 0x022, 0x021, 0x020, 0x01f, 0x01e, 0x01d, 0x01c, 0x01b,
        0x01a, 0x019, 0x018, 0x017, 0x017, 0x016, 0x015, 0x014, 0x014, 0x013, 0x012, 0x011, 0x011,
        0x010, 0x00f, 0x00f, 0x00e, 0x00d, 0x00d, 0x00c, 0x00c, 0x00b, 0x00a, 0x00a, 0x009, 0x009,
        0x008, 0x008, 0x007, 0x007, 0x007, 0x006, 0x006, 0x005, 0x005, 0x005, 0x004, 0x004, 0x004,
        0x003, 0x003, 0x003, 0x002, 0x002, 0x002, 0x002, 0x001, 0x001, 0x001, 0x001, 0x001, 0x001,
        0x001, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000, 0x000,
    ];
    // If the top bit is set, we're in the second half of the curve, which is
    // a mirror image, so invert the index.
    let input = if bitfield(input, 8, 1) != 0 {
        !input
    } else {
        input
    };
    SIN_TABLE[(input & 0xff) as usize] as u32
}

/// Builds one entry of the power table: the 10-bit mantissa with its implied
/// leading bit folded in, shifted up by two so a right shift is all a lookup
/// needs.
const fn power(mantissa: u16) -> u16 {
    (mantissa | 0x400) << 2
}

/// Converts a 5.8 fixed-point logarithmic attenuation to a 13-bit linear
/// volume.
#[inline(always)]
pub(crate) fn attenuation_to_volume(input: u32) -> u32 {
    // The values here are 10-bit mantissas with an implied leading bit, which
    // matches the internal format of the OPN chip, extracted from the die;
    // the order is reversed to save inverting the input.
    const POWER_TABLE: [u16; 256] = {
        const MANTISSAS: [u16; 256] = [
            0x3fa, 0x3f5, 0x3ef, 0x3ea, 0x3e4, 0x3df, 0x3da, 0x3d4, 0x3cf, 0x3c9, 0x3c4, 0x3bf,
            0x3b9, 0x3b4, 0x3ae, 0x3a9, 0x3a4, 0x39f, 0x399, 0x394, 0x38f, 0x38a, 0x384, 0x37f,
            0x37a, 0x375, 0x370, 0x36a, 0x365, 0x360, 0x35b, 0x356, 0x351, 0x34c, 0x347, 0x342,
            0x33d, 0x338, 0x333, 0x32e, 0x329, 0x324, 0x31f, 0x31a, 0x315, 0x310, 0x30b, 0x306,
            0x302, 0x2fd, 0x2f8, 0x2f3, 0x2ee, 0x2e9, 0x2e5, 0x2e0, 0x2db, 0x2d6, 0x2d2, 0x2cd,
            0x2c8, 0x2c4, 0x2bf, 0x2ba, 0x2b5, 0x2b1, 0x2ac, 0x2a8, 0x2a3, 0x29e, 0x29a, 0x295,
            0x291, 0x28c, 0x288, 0x283, 0x27f, 0x27a, 0x276, 0x271, 0x26d, 0x268, 0x264, 0x25f,
            0x25b, 0x257, 0x252, 0x24e, 0x249, 0x245, 0x241, 0x23c, 0x238, 0x234, 0x230, 0x22b,
            0x227, 0x223, 0x21e, 0x21a, 0x216, 0x212, 0x20e, 0x209, 0x205, 0x201, 0x1fd, 0x1f9,
            0x1f5, 0x1f0, 0x1ec, 0x1e8, 0x1e4, 0x1e0, 0x1dc, 0x1d8, 0x1d4, 0x1d0, 0x1cc, 0x1c8,
            0x1c4, 0x1c0, 0x1bc, 0x1b8, 0x1b4, 0x1b0, 0x1ac, 0x1a8, 0x1a4, 0x1a0, 0x19c, 0x199,
            0x195, 0x191, 0x18d, 0x189, 0x185, 0x181, 0x17e, 0x17a, 0x176, 0x172, 0x16f, 0x16b,
            0x167, 0x163, 0x160, 0x15c, 0x158, 0x154, 0x151, 0x14d, 0x149, 0x146, 0x142, 0x13e,
            0x13b, 0x137, 0x134, 0x130, 0x12c, 0x129, 0x125, 0x122, 0x11e, 0x11b, 0x117, 0x114,
            0x110, 0x10c, 0x109, 0x106, 0x102, 0x0ff, 0x0fb, 0x0f8, 0x0f4, 0x0f1, 0x0ed, 0x0ea,
            0x0e7, 0x0e3, 0x0e0, 0x0dc, 0x0d9, 0x0d6, 0x0d2, 0x0cf, 0x0cc, 0x0c8, 0x0c5, 0x0c2,
            0x0be, 0x0bb, 0x0b8, 0x0b5, 0x0b1, 0x0ae, 0x0ab, 0x0a8, 0x0a4, 0x0a1, 0x09e, 0x09b,
            0x098, 0x094, 0x091, 0x08e, 0x08b, 0x088, 0x085, 0x082, 0x07e, 0x07b, 0x078, 0x075,
            0x072, 0x06f, 0x06c, 0x069, 0x066, 0x063, 0x060, 0x05d, 0x05a, 0x057, 0x054, 0x051,
            0x04e, 0x04b, 0x048, 0x045, 0x042, 0x03f, 0x03c, 0x039, 0x036, 0x033, 0x030, 0x02d,
            0x02a, 0x028, 0x025, 0x022, 0x01f, 0x01c, 0x019, 0x016, 0x014, 0x011, 0x00e, 0x00b,
            0x008, 0x006, 0x003, 0x000,
        ];
        let mut table = [0u16; 256];
        let mut index = 0;
        while index < 256 {
            table[index] = power(MANTISSAS[index]);
            index += 1;
        }
        table
    };
    // Look up the fractional part, then shift by the whole.
    (POWER_TABLE[(input & 0xff) as usize] as u32) >> (input >> 8)
}

/// Returns the 4-bit attenuation increment for a 6-bit rate at one of eight
/// steps, or for an attack the fractional scale to decrease by.
#[inline(always)]
pub(crate) fn attenuation_increment(rate: u32, index: u32) -> u32 {
    const INCREMENT_TABLE: [u32; 64] = [
        0x00000000, 0x00000000, 0x10101010, 0x10101010, // 0-3    (0x00-0x03)
        0x10101010, 0x10101010, 0x11101110, 0x11101110, // 4-7    (0x04-0x07)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 8-11   (0x08-0x0B)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 12-15  (0x0C-0x0F)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 16-19  (0x10-0x13)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 20-23  (0x14-0x17)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 24-27  (0x18-0x1B)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 28-31  (0x1C-0x1F)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 32-35  (0x20-0x23)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 36-39  (0x24-0x27)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 40-43  (0x28-0x2B)
        0x10101010, 0x10111010, 0x11101110, 0x11111110, // 44-47  (0x2C-0x2F)
        0x11111111, 0x21112111, 0x21212121, 0x22212221, // 48-51  (0x30-0x33)
        0x22222222, 0x42224222, 0x42424242, 0x44424442, // 52-55  (0x34-0x37)
        0x44444444, 0x84448444, 0x84848484, 0x88848884, // 56-59  (0x38-0x3B)
        0x88888888, 0x88888888, 0x88888888, 0x88888888, // 60-63  (0x3C-0x3F)
    ];
    bitfield(INCREMENT_TABLE[rate as usize], 4 * index, 4)
}

/// Returns the 6-bit signed phase displacement for a 3-bit detune value at
/// a 5-bit key code. The table has been verified against Nuked's equations,
/// which are complicated enough that ymfm keeps the table.
pub(crate) fn detune_adjustment(detune: u32, keycode: u32) -> i32 {
    const DETUNE_ADJUSTMENT: [[u8; 4]; 32] = [
        [0, 0, 1, 2],
        [0, 0, 1, 2],
        [0, 0, 1, 2],
        [0, 0, 1, 2],
        [0, 1, 2, 2],
        [0, 1, 2, 3],
        [0, 1, 2, 3],
        [0, 1, 2, 3],
        [0, 1, 2, 4],
        [0, 1, 3, 4],
        [0, 1, 3, 4],
        [0, 1, 3, 5],
        [0, 2, 4, 5],
        [0, 2, 4, 6],
        [0, 2, 4, 6],
        [0, 2, 5, 7],
        [0, 2, 5, 8],
        [0, 3, 6, 8],
        [0, 3, 6, 9],
        [0, 3, 7, 10],
        [0, 4, 8, 11],
        [0, 4, 8, 12],
        [0, 4, 9, 13],
        [0, 5, 10, 14],
        [0, 5, 11, 16],
        [0, 6, 12, 17],
        [0, 6, 13, 19],
        [0, 7, 14, 20],
        [0, 8, 16, 22],
        [0, 8, 16, 22],
        [0, 8, 16, 22],
        [0, 8, 16, 22],
    ];
    let result = i32::from(DETUNE_ADJUSTMENT[keycode as usize][(detune & 3) as usize]);
    if bitfield(detune, 2, 1) != 0 {
        -result
    } else {
        result
    }
}

/// Converts an OPM concatenated block (3 bits), key code (4 bits) and key
/// fraction (6 bits) to a 0.10 phase step, after applying `delta`. This
/// serves the OPM and the OPZ.
pub(crate) fn opm_key_code_to_phase_step(block_freq: u32, delta: i32) -> u32 {
    // The phase step is essentially the fnum in OPN-speak. ymfm derives this
    // table from the YM2608 manual's fnum formula adjusted to the YM2151's
    // nominal clock, corrected where it differs from data verified on an
    // actual chip; the table comes from David Viens' analysis.
    const PHASE_STEP: [u32; 12 * 64] = [
        41568, 41600, 41632, 41664, 41696, 41728, 41760, 41792, 41856, 41888, 41920, 41952, 42016,
        42048, 42080, 42112, 42176, 42208, 42240, 42272, 42304, 42336, 42368, 42400, 42464, 42496,
        42528, 42560, 42624, 42656, 42688, 42720, 42784, 42816, 42848, 42880, 42912, 42944, 42976,
        43008, 43072, 43104, 43136, 43168, 43232, 43264, 43296, 43328, 43392, 43424, 43456, 43488,
        43552, 43584, 43616, 43648, 43712, 43744, 43776, 43808, 43872, 43904, 43936, 43968, 44032,
        44064, 44096, 44128, 44192, 44224, 44256, 44288, 44352, 44384, 44416, 44448, 44512, 44544,
        44576, 44608, 44672, 44704, 44736, 44768, 44832, 44864, 44896, 44928, 44992, 45024, 45056,
        45088, 45152, 45184, 45216, 45248, 45312, 45344, 45376, 45408, 45472, 45504, 45536, 45568,
        45632, 45664, 45728, 45760, 45792, 45824, 45888, 45920, 45984, 46016, 46048, 46080, 46144,
        46176, 46208, 46240, 46304, 46336, 46368, 46400, 46464, 46496, 46528, 46560, 46656, 46688,
        46720, 46752, 46816, 46848, 46880, 46912, 46976, 47008, 47072, 47104, 47136, 47168, 47232,
        47264, 47328, 47360, 47392, 47424, 47488, 47520, 47552, 47584, 47648, 47680, 47744, 47776,
        47808, 47840, 47904, 47936, 48032, 48064, 48096, 48128, 48192, 48224, 48288, 48320, 48384,
        48416, 48448, 48480, 48544, 48576, 48640, 48672, 48736, 48768, 48800, 48832, 48896, 48928,
        48992, 49024, 49088, 49120, 49152, 49184, 49248, 49280, 49344, 49376, 49440, 49472, 49504,
        49536, 49600, 49632, 49696, 49728, 49792, 49824, 49856, 49888, 49952, 49984, 50048, 50080,
        50144, 50176, 50208, 50240, 50304, 50336, 50400, 50432, 50496, 50528, 50560, 50592, 50656,
        50688, 50752, 50784, 50880, 50912, 50944, 50976, 51040, 51072, 51136, 51168, 51232, 51264,
        51328, 51360, 51424, 51456, 51488, 51520, 51616, 51648, 51680, 51712, 51776, 51808, 51872,
        51904, 51968, 52000, 52064, 52096, 52160, 52192, 52224, 52256, 52384, 52416, 52448, 52480,
        52544, 52576, 52640, 52672, 52736, 52768, 52832, 52864, 52928, 52960, 52992, 53024, 53120,
        53152, 53216, 53248, 53312, 53344, 53408, 53440, 53504, 53536, 53600, 53632, 53696, 53728,
        53792, 53824, 53920, 53952, 54016, 54048, 54112, 54144, 54208, 54240, 54304, 54336, 54400,
        54432, 54496, 54528, 54592, 54624, 54688, 54720, 54784, 54816, 54880, 54912, 54976, 55008,
        55072, 55104, 55168, 55200, 55264, 55296, 55360, 55392, 55488, 55520, 55584, 55616, 55680,
        55712, 55776, 55808, 55872, 55936, 55968, 56032, 56064, 56128, 56160, 56224, 56288, 56320,
        56384, 56416, 56480, 56512, 56576, 56608, 56672, 56736, 56768, 56832, 56864, 56928, 56960,
        57024, 57120, 57152, 57216, 57248, 57312, 57376, 57408, 57472, 57536, 57568, 57632, 57664,
        57728, 57792, 57824, 57888, 57952, 57984, 58048, 58080, 58144, 58208, 58240, 58304, 58368,
        58400, 58464, 58496, 58560, 58624, 58656, 58720, 58784, 58816, 58880, 58912, 58976, 59040,
        59072, 59136, 59200, 59232, 59296, 59328, 59392, 59456, 59488, 59552, 59648, 59680, 59744,
        59776, 59840, 59904, 59936, 60000, 60064, 60128, 60160, 60224, 60288, 60320, 60384, 60416,
        60512, 60544, 60608, 60640, 60704, 60768, 60800, 60864, 60928, 60992, 61024, 61088, 61152,
        61184, 61248, 61280, 61376, 61408, 61472, 61536, 61600, 61632, 61696, 61760, 61824, 61856,
        61920, 61984, 62048, 62080, 62144, 62208, 62272, 62304, 62368, 62432, 62496, 62528, 62592,
        62656, 62720, 62752, 62816, 62880, 62944, 62976, 63040, 63104, 63200, 63232, 63296, 63360,
        63424, 63456, 63520, 63584, 63648, 63680, 63744, 63808, 63872, 63904, 63968, 64032, 64096,
        64128, 64192, 64256, 64320, 64352, 64416, 64480, 64544, 64608, 64672, 64704, 64768, 64832,
        64896, 64928, 65024, 65056, 65120, 65184, 65248, 65312, 65376, 65408, 65504, 65536, 65600,
        65664, 65728, 65792, 65856, 65888, 65984, 66016, 66080, 66144, 66208, 66272, 66336, 66368,
        66464, 66496, 66560, 66624, 66688, 66752, 66816, 66848, 66944, 66976, 67040, 67104, 67168,
        67232, 67296, 67328, 67424, 67456, 67520, 67584, 67648, 67712, 67776, 67808, 67904, 67936,
        68000, 68064, 68128, 68192, 68256, 68288, 68384, 68448, 68512, 68544, 68640, 68672, 68736,
        68800, 68896, 68928, 68992, 69056, 69120, 69184, 69248, 69280, 69376, 69440, 69504, 69536,
        69632, 69664, 69728, 69792, 69920, 69952, 70016, 70080, 70144, 70208, 70272, 70304, 70400,
        70464, 70528, 70560, 70656, 70688, 70752, 70816, 70912, 70976, 71040, 71104, 71136, 71232,
        71264, 71360, 71424, 71488, 71552, 71616, 71648, 71744, 71776, 71872, 71968, 72032, 72096,
        72160, 72192, 72288, 72320, 72416, 72480, 72544, 72608, 72672, 72704, 72800, 72832, 72928,
        72992, 73056, 73120, 73184, 73216, 73312, 73344, 73440, 73504, 73568, 73632, 73696, 73728,
        73824, 73856, 73952, 74080, 74144, 74208, 74272, 74304, 74400, 74432, 74528, 74592, 74656,
        74720, 74784, 74816, 74912, 74944, 75040, 75136, 75200, 75264, 75328, 75360, 75456, 75488,
        75584, 75648, 75712, 75776, 75840, 75872, 75968, 76000, 76096, 76224, 76288, 76352, 76416,
        76448, 76544, 76576, 76672, 76736, 76800, 76864, 76928, 77024, 77120, 77152, 77248, 77344,
        77408, 77472, 77536, 77568, 77664, 77696, 77792, 77856, 77920, 77984, 78048, 78144, 78240,
        78272, 78368, 78464, 78528, 78592, 78656, 78688, 78784, 78816, 78912, 78976, 79040, 79104,
        79168, 79264, 79360, 79392, 79488, 79616, 79680, 79744, 79808, 79840, 79936, 79968, 80064,
        80128, 80192, 80256, 80320, 80416, 80512, 80544, 80640, 80768, 80832, 80896, 80960, 80992,
        81088, 81120, 81216, 81280, 81344, 81408, 81472, 81568, 81664, 81696, 81792, 81952, 82016,
        82080, 82144, 82176, 82272, 82304, 82400, 82464, 82528, 82592, 82656, 82752, 82848, 82880,
        82976,
    ];

    // Extract the block (octave) first.
    let mut block = bitfield(block_freq, 10, 3);

    // The keycode (bits 6-9) is "gappy", mapping 12 values over 16 in each
    // octave; to correct for this, multiply the 4-bit value by 3/4 (or
    // rather subtract 1/4); an (invalid) value of 15 bleeds into the next
    // octave, which is confirmed.
    let adjusted_code = bitfield(block_freq, 6, 4) - bitfield(block_freq, 8, 2);

    // Re-insert the 6-bit fraction, then add the delta now the gaps are gone.
    let mut eff_freq = ((adjusted_code << 6) | bitfield(block_freq, 0, 6)) as i32;
    eff_freq += delta;

    // Handle over- and underflow by adjusting the block.
    if (eff_freq as u32) >= 768 {
        if eff_freq < 0 {
            // The minimum delta is -512 (PM), so this underflows one octave.
            eff_freq += 768;
            let was = block;
            block = block.wrapping_sub(1);
            if was == 0 {
                return PHASE_STEP[0] >> 7;
            }
        } else {
            // The maximum delta is +512+608 (PM and detune), so this can
            // overflow two octaves.
            eff_freq -= 768;
            if eff_freq >= 768 {
                block += 1;
                eff_freq -= 768;
            }
            let was = block;
            block += 1;
            if was >= 7 {
                return PHASE_STEP[767];
            }
        }
    }

    // Look up the phase step for the key code, then shift by octave.
    PHASE_STEP[eff_freq as usize] >> (block ^ 7)
}

/// Returns the signed PM adjustment to the frequency for the 7 most
/// significant frequency number bits, a 3-bit PM depth and a signed 5-bit raw
/// PM value, written to match Nuked's behaviour. This serves the OPN family.
pub(crate) fn opn_lfo_pm_phase_adjustment(
    fnum_bits: u32,
    pm_sensitivity: u32,
    lfo_raw_pm: i32,
) -> i32 {
    // Two shift values to apply to the top 7 bits of fnum: effectively a
    // cheap multiply by a constant of 0 to 2 bits.
    const LFO_PM_SHIFTS: [[u8; 8]; 8] = [
        [0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77],
        [0x77, 0x77, 0x77, 0x77, 0x72, 0x72, 0x72, 0x72],
        [0x77, 0x77, 0x77, 0x72, 0x72, 0x72, 0x17, 0x17],
        [0x77, 0x77, 0x72, 0x72, 0x17, 0x17, 0x12, 0x12],
        [0x77, 0x77, 0x72, 0x17, 0x17, 0x17, 0x12, 0x07],
        [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
        [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
        [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
    ];

    // Look up the relevant shifts.
    let abs_pm = if lfo_raw_pm < 0 {
        -lfo_raw_pm
    } else {
        lfo_raw_pm
    };
    let shifts =
        u32::from(LFO_PM_SHIFTS[pm_sensitivity as usize][bitfield(abs_pm as u32, 0, 3) as usize]);

    // Compute the adjustment.
    let mut adjust =
        ((fnum_bits >> bitfield(shifts, 0, 4)) + (fnum_bits >> bitfield(shifts, 4, 4))) as i32;
    if pm_sensitivity > 5 {
        adjust <<= pm_sensitivity - 5;
    }
    adjust >>= 2;

    // Every 16 cycles it inverts sign.
    if lfo_raw_pm < 0 {
        -adjust
    } else {
        adjust
    }
}

//*********************************************************
//  CORE IMPLEMENTATION
//*********************************************************

/// Data computed once when an operator is prepared and read on every sample
/// after it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OpdataCache {
    /// The waveform, as an index into the register set's tables; it stands
    /// in for ymfm's pointer.
    pub(crate) waveform: u8,
    /// The phase step, or `PHASE_STEP_DYNAMIC` if PM is active.
    pub(crate) phase_step: u32,
    /// Total level times eight, plus key scale level.
    pub(crate) total_level: u32,
    /// The raw block frequency the phase step is computed from.
    pub(crate) block_freq: u32,
    /// The detune value the phase step is computed from.
    pub(crate) detune: i32,
    /// The frequency multiple, as an x.1 value.
    pub(crate) multiple: u32,
    /// The sustain level, shifted up to envelope values.
    pub(crate) eg_sustain: u32,
    /// The envelope rate for each state, key scale rate included.
    pub(crate) eg_rate: [u8; EG_STATES],
    /// The envelope shift amount.
    pub(crate) eg_shift: u8,
}

impl OpdataCache {
    /// Marks a phase step that has to be recomputed every sample, because
    /// the PM LFO moves it.
    pub(crate) const PHASE_STEP_DYNAMIC: u32 = 1;
}

/// Packs four operator numbers into one operator-map entry; 0xff is an
/// empty slot.
pub(crate) const fn operator_list(o1: u8, o2: u8, o3: u8, o4: u8) -> u32 {
    (o1 as u32) | ((o2 as u32) << 8) | ((o3 as u32) << 16) | ((o4 as u32) << 24)
}

/// Applies KSR to a raw ADSR rate, ignoring it if the rate is 0 and clamping
/// to 63.
pub(crate) const fn effective_rate(rawrate: u32, ksr: u32) -> u8 {
    if rawrate == 0 {
        0
    } else if rawrate + ksr < 63 {
        (rawrate + ksr) as u8
    } else {
        63
    }
}

/// A family's register set, which the engine is generic over: ymfm's
/// `RegisterType`, including the defaults of `fm_registers_base`.
pub(crate) trait FmRegisters {
    /// The number of outputs exposed (1-4).
    const OUTPUTS: usize;
    /// The number of channels on the chip.
    const CHANNELS: usize;
    /// A bitmask of all channels.
    const ALL_CHANNELS: u32;
    /// The number of operators on the chip.
    const OPERATORS: usize;
    /// The starting clock prescale.
    const DEFAULT_PRESCALE: u32;
    /// The clock divider of the envelope generator.
    const EG_CLOCK_DIVIDER: u32;
    /// The channels CSM mode triggers.
    const CSM_TRIGGER_MASK: u32;
    /// The address of the mode register, which controls the timers.
    const REG_MODE: u32;
    /// The status bit timer A sets.
    const STATUS_TIMERA: u8;
    /// The status bit timer B sets.
    const STATUS_TIMERB: u8;
    /// The status bit for busy.
    const STATUS_BUSY: u8;
    /// The status bit for an IRQ.
    const STATUS_IRQ: u8;
    /// Whether operators can be reassigned to channels at run time (OPL3+).
    const DYNAMIC_OPS: bool = false;
    /// Whether the envelope has a DP ("depress"?) stage (OPLL).
    const EG_HAS_DEPRESS: bool = false;
    /// Whether the envelope has a faux reverb stage (OPQ/OPZ).
    const EG_HAS_REVERB: bool = false;
    /// Whether the chip has SSG envelopes (OPN).
    const EG_HAS_SSG: bool = false;
    /// Whether the modulator is delayed by a sample (OPL before the OPL3).
    const MODULATOR_DELAY: bool = false;

    /// Returns the register set as the constructor leaves it.
    fn new() -> Self;
    /// Resets to the initial state.
    fn reset(&mut self);
    /// Maps a channel number to its register offset.
    fn channel_offset(chnum: u32) -> u32;
    /// Maps an operator number to its register offset.
    fn operator_offset(opnum: u32) -> u32;
    /// Fills in each channel's operators, as `operator_list` entries.
    fn operator_map(&self, dest: &mut [u32; MAX_CHANNELS]);
    /// Writes a register and returns the key-on it carries, as a channel and
    /// an operator mask, if any.
    fn write(&mut self, index: u16, data: u8) -> Option<(u32, u32)>;
    /// Clocks the noise and LFO, returning the LFO PM value.
    fn clock_noise_and_lfo(&mut self) -> i32;
    /// Returns the LFO AM offset for a channel.
    fn lfo_am_offset(&self, choffs: u32) -> u32;
    /// Returns the noise state.
    fn noise_state(&self) -> u32;
    /// Fills an operator's cache.
    fn cache_operator_data(&mut self, choffs: u32, opoffs: u32, cache: &mut OpdataCache);
    /// Computes the phase step for a PM value.
    fn compute_phase_step(
        &mut self,
        choffs: u32,
        opoffs: u32,
        cache: &OpdataCache,
        lfo_raw_pm: i32,
    ) -> u32;
    /// Returns the waveform table a cache selects.
    fn waveform(&self, index: u8) -> &[u16; WAVEFORM_LENGTH];

    // system-wide registers
    fn timer_a_value(&self) -> u32;
    fn timer_b_value(&self) -> u32;
    fn csm(&self) -> u32;
    fn reset_timer_a(&self) -> u32;
    fn reset_timer_b(&self) -> u32;
    fn enable_timer_a(&self) -> u32;
    fn enable_timer_b(&self) -> u32;
    fn load_timer_a(&self) -> u32;
    fn load_timer_b(&self) -> u32;
    fn status_mask(&self) -> u32 {
        0 // OPL only
    }
    fn irq_reset(&self) -> u32 {
        0 // OPL only
    }
    fn noise_enable(&self) -> u32 {
        0 // OPM only
    }
    fn rhythm_enable(&self) -> u32 {
        0 // OPL only
    }

    // per-channel registers
    fn ch_output_any(&self, choffs: u32) -> u32;
    fn ch_output_0(&self, choffs: u32) -> u32;
    fn ch_output_1(&self, choffs: u32) -> u32;
    fn ch_output_2(&self, choffs: u32) -> u32;
    fn ch_output_3(&self, choffs: u32) -> u32;
    fn ch_feedback(&self, choffs: u32) -> u32;
    fn ch_algorithm(&self, choffs: u32) -> u32;

    // per-operator registers
    fn op_lfo_am_enable(&self, opoffs: u32) -> u32;
    fn op_ssg_eg_enable(&self, _opoffs: u32) -> u32 {
        0 // OPN(A) only
    }
    fn op_ssg_eg_mode(&self, _opoffs: u32) -> u32 {
        0 // OPN(A) only
    }
}

//*********************************************************
//  FM OPERATOR
//*********************************************************

/// An FM operator (or "slot"), which produces an output sine wave modulated
/// by an envelope.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FmOperator {
    /// The channel offset in the registers.
    choffs: u32,
    /// The operator offset in the registers.
    opoffs: u32,
    /// The current phase, in 10.10 format.
    phase: u32,
    /// The computed envelope attenuation, in 4.6 format.
    env_attenuation: u16,
    /// The current envelope state.
    env_state: usize,
    /// Non-zero if the output should be inverted (bit 0).
    ssg_inverted: u8,
    /// The current key state: on or off (bit 0).
    key_state: u8,
    /// The live key-on state (bit 0 direct, bit 1 rhythm, bit 2 CSM).
    keyon_live: u8,
    /// Cached values for performance.
    cache: OpdataCache,
}

impl FmOperator {
    /// The "quiet" value, past which there is no work to do.
    const EG_QUIET: u16 = 0x380;

    const fn new(opoffs: u32) -> FmOperator {
        FmOperator {
            choffs: 0,
            opoffs,
            phase: 0,
            env_attenuation: 0x3ff,
            env_state: EG_RELEASE,
            ssg_inverted: 0,
            key_state: 0,
            keyon_live: 0,
            cache: OpdataCache {
                waveform: 0,
                phase_step: 0,
                total_level: 0,
                block_freq: 0,
                detune: 0,
                multiple: 0,
                eg_sustain: 0,
                eg_rate: [0; EG_STATES],
                eg_shift: 0,
            },
        }
    }

    /// Resets the operator state.
    fn reset(&mut self) {
        self.phase = 0;
        self.env_attenuation = 0x3ff;
        self.env_state = EG_RELEASE;
        self.ssg_inverted = 0;
        self.key_state = 0;
        self.keyon_live = 0;
    }

    /// Returns the current phase value.
    #[inline(always)]
    pub(crate) fn phase(&self) -> u32 {
        self.phase >> 10
    }

    /// Prepares for clocking, and reports whether the operator is active.
    fn prepare<R: FmRegisters>(&mut self, regs: &mut R) -> bool {
        // cache the data
        regs.cache_operator_data(self.choffs, self.opoffs, &mut self.cache);

        // clock the key state
        self.clock_keystate(regs, u32::from(self.keyon_live != 0));
        self.keyon_live &= !(1 << KEYON_CSM);

        // we're active until we're quiet after the release
        let done = if R::EG_HAS_REVERB {
            EG_REVERB
        } else {
            EG_RELEASE
        };
        self.env_state != done || self.env_attenuation < Self::EG_QUIET
    }

    /// The master clocking function.
    fn clock<R: FmRegisters>(&mut self, regs: &mut R, env_counter: u32, lfo_raw_pm: i32) {
        // clock the SSG-EG state (OPN/OPNA)
        if regs.op_ssg_eg_enable(self.opoffs) != 0 {
            self.clock_ssg_eg_state(regs);
        } else {
            self.ssg_inverted = 0;
        }

        // clock the envelope if on an envelope cycle; env_counter is a x.2
        // value
        if bitfield(env_counter, 0, 2) == 0 {
            self.clock_envelope(regs, env_counter >> 2);
        }

        // clock the phase
        self.clock_phase(regs, lfo_raw_pm);
    }

    /// Computes the 14-bit signed volume of this operator, given a phase
    /// modulation and an AM LFO offset.
    #[inline(always)]
    fn compute_volume<R: FmRegisters>(&self, regs: &R, phase: u32, am_offset: u32) -> i32 {
        // the low 10 bits of phase represents a full 2*PI period over the
        // full sin wave

        // early out if the envelope is effectively off
        if self.env_attenuation > Self::EG_QUIET {
            return 0;
        }

        // get the absolute value of the sin, as attenuation, as a 4.8 fixed
        // point value
        let waveform = regs.waveform(self.cache.waveform);
        let sin_attenuation = u32::from(waveform[(phase as usize) & (WAVEFORM_LENGTH - 1)]);

        // get the attenuation from the evelope generator as a 4.6 value,
        // shifted up to 4.8
        let env_attenuation = self.envelope_attenuation(regs, am_offset) << 2;

        // combine into a 5.8 value, then convert from attenuation to 13-bit
        // linear volume
        let result = attenuation_to_volume((sin_attenuation & 0x7fff) + env_attenuation) as i32;

        // negate if in the negative part of the sin wave (sign bit gives 14
        // bits)
        if bitfield(sin_attenuation, 15, 1) != 0 {
            -result
        } else {
            result
        }
    }

    /// Computes the 14-bit signed noise volume of this operator, given an AM
    /// offset.
    fn compute_noise_volume<R: FmRegisters>(&self, regs: &R, am_offset: u32) -> i32 {
        // the application manual says the logarithmic transform is not
        // applied here, so just use the raw envelope attenuation, inverted
        // (since 0 attenuation should be maximum), and shift it up from a
        // 10-bit value to an 11-bit value
        let result = ((self.envelope_attenuation(regs, am_offset) ^ 0x3ff) << 1) as i32;

        // QUESTION (ymfm's): is AM applied still?

        // negate based on the noise state
        if bitfield(regs.noise_state(), 0, 1) != 0 {
            -result
        } else {
            result
        }
    }

    /// Signals a key on/off event.
    fn keyonoff(&mut self, on: u32, kind: u32) {
        self.keyon_live = (self.keyon_live & !(1u8 << kind)) | ((bitfield(on, 0, 1) as u8) << kind);
    }

    /// Starts the attack phase, on a key on or when an SSG-EG cycle
    /// completes and restarts.
    fn start_attack<R: FmRegisters>(&mut self, regs: &R, is_restart: bool) {
        // don't change anything if already in attack state
        if self.env_state == EG_ATTACK {
            return;
        }
        self.env_state = EG_ATTACK;

        // generally not inverted at start, except if SSG-EG is enabled and
        // one of the inverted modes is specified; leave this alone on a
        // restart, as it is managed by the clock_ssg_eg_state() code
        if R::EG_HAS_SSG && !is_restart {
            self.ssg_inverted = (regs.op_ssg_eg_enable(self.opoffs)
                & bitfield(regs.op_ssg_eg_mode(self.opoffs), 2, 1))
                as u8;
        }

        // reset the phase when we start an attack due to a key on (but not
        // when due to an SSG-EG restart except in certain cases managed
        // directly by the SSG-EG code)
        if !is_restart {
            self.phase = 0;
        }

        // if the attack rate >= 62 then immediately go to max attenuation
        if self.cache.eg_rate[EG_ATTACK] >= 62 {
            self.env_attenuation = 0;
        }
    }

    /// Starts the release phase, on a key off.
    fn start_release<R: FmRegisters>(&mut self) {
        // don't change anything if already in release state
        if self.env_state >= EG_RELEASE {
            return;
        }
        self.env_state = EG_RELEASE;

        // if attenuation if inverted due to SSG-EG, snap the inverted
        // attenuation as the starting point
        if R::EG_HAS_SSG && self.ssg_inverted != 0 {
            self.env_attenuation = ((0x200 - i32::from(self.env_attenuation)) & 0x3ff) as u16;
            self.ssg_inverted = 0;
        }
    }

    /// Clocks the key state to match the incoming one.
    fn clock_keystate<R: FmRegisters>(&mut self, regs: &R, keystate: u32) {
        // has the key changed?
        if (keystate ^ u32::from(self.key_state)) != 0 {
            self.key_state = keystate as u8;

            // if the key has turned on, start the attack
            if keystate != 0 {
                // OPLL has a DP ("depress"?) state to bring the volume down
                // before starting the attack
                if R::EG_HAS_DEPRESS && self.env_attenuation < 0x200 {
                    self.env_state = EG_DEPRESS;
                } else {
                    self.start_attack(regs, false);
                }
            }
            // otherwise, start the release
            else {
                self.start_release::<R>();
            }
        }
    }

    /// Clocks the SSG-EG state; only called if SSG-EG is enabled.
    fn clock_ssg_eg_state<R: FmRegisters>(&mut self, regs: &R) {
        // work only happens once the attenuation crosses above 0x200
        if bitfield(u32::from(self.env_attenuation), 9, 1) == 0 {
            return;
        }

        // 8 SSG-EG modes:
        //    000: repeat normally
        //    001: run once, hold low
        //    010: repeat, alternating between inverted/non-inverted
        //    011: run once, hold high
        //    100: inverted repeat normally
        //    101: inverted run once, hold low
        //    110: inverted repeat, alternating between inverted/non-inverted
        //    111: inverted run once, hold high
        let mode = regs.op_ssg_eg_mode(self.opoffs);

        // hold modes (1/3/5/7)
        if bitfield(mode, 0, 1) != 0 {
            // set the inverted flag to the end state (0 for modes 1/7, 1 for
            // modes 3/5)
            self.ssg_inverted = (bitfield(mode, 2, 1) ^ bitfield(mode, 1, 1)) as u8;

            // if holding, force the attenuation to the expected value once
            // we're past the attack phase
            if self.env_state != EG_ATTACK {
                self.env_attenuation = if self.ssg_inverted != 0 { 0x200 } else { 0x3ff };
            }
        }
        // continuous modes (0/2/4/6)
        else {
            // toggle invert in alternating mode (even in attack state)
            self.ssg_inverted ^= bitfield(mode, 1, 1) as u8;

            // restart attack if in decay/sustain states
            if self.env_state == EG_DECAY || self.env_state == EG_SUSTAIN {
                self.start_attack(regs, true);
            }

            // phase is reset to 0 in modes 0/4
            if bitfield(mode, 1, 1) == 0 {
                self.phase = 0;
            }
        }

        // in all modes, once we hit release state, attenuation is forced to
        // maximum
        if self.env_state == EG_RELEASE {
            self.env_attenuation = 0x3ff;
        }
    }

    /// Clocks the envelope state according to the given count.
    fn clock_envelope<R: FmRegisters>(&mut self, regs: &R, env_counter: u32) {
        // handle attack->decay transitions
        if self.env_state == EG_ATTACK && self.env_attenuation == 0 {
            self.env_state = EG_DECAY;
        }

        // handle decay->sustain transitions; it is important to do this
        // immediately after the attack->decay transition above in the event
        // that the sustain level is set to 0 (in which case we will skip
        // right to sustain without doing any decay)
        if self.env_state == EG_DECAY && u32::from(self.env_attenuation) >= self.cache.eg_sustain {
            self.env_state = EG_SUSTAIN;
        }

        // fetch the appropriate 6-bit rate value from the cache
        let rate = u32::from(self.cache.eg_rate[self.env_state]);

        // compute the rate shift value; this is the shift needed to apply to
        // the env_counter such that it becomes a 5.11 fixed point number
        let rate_shift = rate >> 2;
        let env_counter = env_counter << rate_shift;

        // see if the fractional part is 0; if not, it's not time to clock
        if bitfield(env_counter, 0, 11) != 0 {
            return;
        }

        // determine the increment based on the non-fractional part of
        // env_counter
        let relevant_bits = bitfield(
            env_counter,
            if rate_shift <= 11 { 11 } else { rate_shift },
            3,
        );
        let increment = attenuation_increment(rate, relevant_bits);

        // attack is the only one that increases
        if self.env_state == EG_ATTACK {
            // glitch means that attack rates of 62/63 don't increment if
            // changed after the initial key on (where they are handled
            // specially); nukeykt confirms this happens on OPM, OPN, OPL/OPLL
            // at least so assuming it is true for everyone
            if rate < 62 {
                // `att += (~att * increment) >> 4` on a uint16_t, which C
                // evaluates in 32 bits, unsigned once the increment joins,
                // and truncates back.
                let att = u32::from(self.env_attenuation);
                self.env_attenuation = att.wrapping_add((!att).wrapping_mul(increment) >> 4) as u16;
            }
        }
        // all other cases are similar
        else {
            // non-SSG-EG cases just apply the increment
            if regs.op_ssg_eg_enable(self.opoffs) == 0 {
                self.env_attenuation = self.env_attenuation.wrapping_add(increment as u16);
            }
            // SSG-EG only applies if less than mid-point, and then at 4x
            else if self.env_attenuation < 0x200 {
                self.env_attenuation = self.env_attenuation.wrapping_add(4 * increment as u16);
            }

            // clamp the final attenuation
            if self.env_attenuation >= 0x400 {
                self.env_attenuation = 0x3ff;
            }

            // transition from depress to attack
            if R::EG_HAS_DEPRESS && self.env_state == EG_DEPRESS && self.env_attenuation >= 0x200 {
                self.start_attack(regs, false);
            }

            // transition from release to reverb, should switch at -18dB
            if R::EG_HAS_REVERB && self.env_state == EG_RELEASE && self.env_attenuation >= 0xc0 {
                self.env_state = EG_REVERB;
            }
        }
    }

    /// Clocks the 10.10 phase value.
    fn clock_phase<R: FmRegisters>(&mut self, regs: &mut R, lfo_raw_pm: i32) {
        // read from the cache, or recalculate if PM active
        let mut phase_step = self.cache.phase_step;
        if phase_step == OpdataCache::PHASE_STEP_DYNAMIC {
            phase_step = regs.compute_phase_step(self.choffs, self.opoffs, &self.cache, lfo_raw_pm);
        }

        // finally apply the step to the current phase value
        self.phase = self.phase.wrapping_add(phase_step);
    }

    /// Returns the effective attenuation of the envelope.
    fn envelope_attenuation<R: FmRegisters>(&self, regs: &R, am_offset: u32) -> u32 {
        let mut result = u32::from(self.env_attenuation) >> self.cache.eg_shift;

        // invert if necessary due to SSG-EG
        if R::EG_HAS_SSG && self.ssg_inverted != 0 {
            result = 0x200u32.wrapping_sub(result) & 0x3ff;
        }

        // add in LFO AM modulation
        if regs.op_lfo_am_enable(self.opoffs) != 0 {
            result += am_offset;
        }

        // add in total level and KSL from the cache
        result += self.cache.total_level;

        // clamp to max and return
        result.min(0x3ff)
    }
}

//*********************************************************
//  FM CHANNEL
//*********************************************************

/// No operator in this slot of a channel.
const NO_OPERATOR: u8 = 0xff;

/// The 4-operator connections, OPM and OPN's eight plus OPL3's four as 8 to
/// 11. Each entry packs which input operators 2, 3 and 4 take from the
/// running `opout` table and which of operators 1, 2 and 3 join the final
/// sum; see `output_4op`.
const fn algorithm(
    op2in: u16,
    op3in: u16,
    op4in: u16,
    op1out: u16,
    op2out: u16,
    op3out: u16,
) -> u16 {
    op2in | (op3in << 1) | (op4in << 4) | (op1out << 7) | (op2out << 8) | (op3out << 9)
}

const ALGORITHM_OPS: [u16; 8 + 4] = [
    algorithm(1, 2, 3, 0, 0, 0), //  0: O1 -> O2 -> O3 -> O4 -> out (O4)
    algorithm(0, 5, 3, 0, 0, 0), //  1: (O1 + O2) -> O3 -> O4 -> out (O4)
    algorithm(0, 2, 6, 0, 0, 0), //  2: (O1 + (O2 -> O3)) -> O4 -> out (O4)
    algorithm(1, 0, 7, 0, 0, 0), //  3: ((O1 -> O2) + O3) -> O4 -> out (O4)
    algorithm(1, 0, 3, 0, 1, 0), //  4: ((O1 -> O2) + (O3 -> O4)) -> out (O2+O4)
    algorithm(1, 1, 1, 0, 1, 1), //  5: ((O1 -> O2) + (O1 -> O3) + (O1 -> O4)) -> out (O2+O3+O4)
    algorithm(1, 0, 0, 0, 1, 1), //  6: ((O1 -> O2) + O3 + O4) -> out (O2+O3+O4)
    algorithm(0, 0, 0, 1, 1, 1), //  7: (O1 + O2 + O3 + O4) -> out (O1+O2+O3+O4)
    algorithm(1, 2, 3, 0, 0, 0), //  8: O1 -> O2 -> O3 -> O4 -> out (O4)         [same as 0]
    algorithm(0, 2, 3, 1, 0, 0), //  9: (O1 + (O2 -> O3 -> O4)) -> out (O1+O4)   [unique]
    algorithm(1, 0, 3, 0, 1, 0), // 10: ((O1 -> O2) + (O3 -> O4)) -> out (O2+O4) [same as 4]
    algorithm(0, 2, 0, 1, 0, 1), // 11: (O1 + (O2 -> O3) + O4) -> out (O1+O3+O4) [unique]
];

/// An FM channel, which combines the output of 2 or 4 operators into a final
/// result. Its operators are indices into the engine's operator array, which
/// every method that needs them is handed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FmChannel {
    /// The channel offset in the registers.
    choffs: u32,
    /// The feedback memory for operator 1.
    feedback: [i16; 2],
    /// The next input value for operator 1's feedback, set in output.
    feedback_in: i16,
    /// Up to four operators, or `NO_OPERATOR`.
    op: [u8; 4],
}

impl FmChannel {
    const fn new(choffs: u32) -> FmChannel {
        FmChannel {
            choffs,
            feedback: [0, 0],
            feedback_in: 0,
            op: [NO_OPERATOR; 4],
        }
    }

    /// Resets the channel state.
    fn reset(&mut self) {
        self.feedback = [0, 0];
        self.feedback_in = 0;
    }

    /// Assigns an operator to a slot, or empties it.
    fn assign(&mut self, index: usize, opnum: u8, operators: &mut [FmOperator]) {
        self.op[index] = opnum;
        if opnum != NO_OPERATOR {
            operators[opnum as usize].choffs = self.choffs;
        }
    }

    /// Signals key on/off to the channel's operators.
    fn keyonoff(&self, operators: &mut [FmOperator], states: u32, kind: u32) {
        for (opnum, slot) in self.op.iter().enumerate() {
            if *slot != NO_OPERATOR {
                operators[*slot as usize].keyonoff(bitfield(states, opnum as u32, 1), kind);
            }
        }
    }

    /// Prepares the operators for clocking and reports whether any is
    /// active.
    fn prepare<R: FmRegisters>(&self, operators: &mut [FmOperator], regs: &mut R) -> bool {
        let mut active_mask = 0u32;

        // prepare all operators and determine if they are active
        for (opnum, slot) in self.op.iter().enumerate() {
            if *slot != NO_OPERATOR && operators[*slot as usize].prepare(regs) {
                active_mask |= 1 << opnum;
            }
        }
        active_mask != 0
    }

    /// The master clock of all operators.
    fn clock<R: FmRegisters>(
        &mut self,
        operators: &mut [FmOperator],
        regs: &mut R,
        env_counter: u32,
        lfo_raw_pm: i32,
    ) {
        // clock the feedback through
        self.feedback[0] = self.feedback[1];
        self.feedback[1] = self.feedback_in;

        for slot in self.op {
            if slot != NO_OPERATOR {
                operators[slot as usize].clock(regs, env_counter, lfo_raw_pm);
            }
        }
    }

    /// Reports whether this is a 4-operator channel.
    fn is4op<R: FmRegisters>(&self) -> bool {
        if R::DYNAMIC_OPS {
            return self.op[2] != NO_OPERATOR;
        }
        R::OPERATORS / R::CHANNELS == 4
    }

    /// Adds a value to the outputs the channel is enabled on.
    fn add_to_output<R: FmRegisters>(&self, regs: &R, output: &mut [i32], value: i32) {
        let choffs = self.choffs;
        if R::OUTPUTS == 1 || regs.ch_output_0(choffs) != 0 {
            output[0] += value;
        }
        if R::OUTPUTS >= 2 && regs.ch_output_1(choffs) != 0 {
            output[1 % R::OUTPUTS] += value;
        }
        if R::OUTPUTS >= 3 && regs.ch_output_2(choffs) != 0 {
            output[2 % R::OUTPUTS] += value;
        }
        if R::OUTPUTS >= 4 && regs.ch_output_3(choffs) != 0 {
            output[3 % R::OUTPUTS] += value;
        }
    }

    /// Computes operator 1 with its optional self-feedback, stores the next
    /// feedback input, and returns the value as the sixteen bits ymfm stores
    /// it in.
    fn operator1<R: FmRegisters>(
        &mut self,
        operators: &[FmOperator],
        regs: &R,
        am_offset: u32,
    ) -> i32 {
        // operator 1 has optional self-feedback
        let mut opmod = 0i32;
        let feedback = regs.ch_feedback(self.choffs);
        if feedback != 0 {
            opmod = (i32::from(self.feedback[0]) + i32::from(self.feedback[1])) >> (10 - feedback);
        }

        // compute the 14-bit volume/value of operator 1 and update the
        // feedback; `op1value = m_feedback_in = ...` reads back the int16_t
        let op = &operators[self.op[0] as usize];
        let value = op.compute_volume(regs, op.phase().wrapping_add(opmod as u32), am_offset);
        self.feedback_in = value as i16;
        i32::from(self.feedback_in)
    }

    /// Computes an operator with a phase modulation added to its phase.
    #[inline(always)]
    fn modulated<R: FmRegisters>(
        &self,
        operators: &[FmOperator],
        regs: &R,
        index: usize,
        opmod: i32,
        am_offset: u32,
    ) -> i32 {
        let op = &operators[self.op[index] as usize];
        op.compute_volume(regs, op.phase().wrapping_add(opmod as u32), am_offset)
    }

    /// Combines 2 operators according to the algorithm, adding a sum shaped
    /// by `rshift` and `clipmax`, which vary between implementations.
    fn output_2op<R: FmRegisters>(
        &mut self,
        operators: &[FmOperator],
        regs: &R,
        output: &mut [i32],
        rshift: u32,
        clipmax: i32,
    ) {
        // AM amount is the same across all operators; compute it once
        let am_offset = regs.lfo_am_offset(self.choffs);

        let op1value = self.operator1(operators, regs, am_offset);

        // now that the feedback has been computed, skip the rest if all
        // volumes are clear; no need to do all this work for nothing
        if regs.ch_output_any(self.choffs) == 0 {
            return;
        }

        // some OPL chips use the previous sample for modulation instead of
        // the current sample
        let modulator = if R::MODULATOR_DELAY {
            i32::from(self.feedback[1])
        } else {
            op1value
        };

        // Algorithms for two-operator case:
        //    0: O1 -> O2 -> out
        //    1: (O1 + O2) -> out
        let result = if bitfield(regs.ch_algorithm(self.choffs), 0, 1) == 0 {
            let opmod = modulator >> 1;
            self.modulated(operators, regs, 1, opmod, am_offset) >> rshift
        } else {
            let mut result = modulator >> rshift;
            result += self.modulated(operators, regs, 1, 0, am_offset) >> rshift;
            let clipmin = -clipmax - 1;
            clamp(result, clipmin, clipmax)
        };

        // add to the output
        self.add_to_output(regs, output, result);
    }

    /// Combines 4 operators according to the algorithm, adding a sum shaped
    /// by `rshift` and `clipmax`, which vary between implementations.
    fn output_4op<R: FmRegisters>(
        &mut self,
        operators: &[FmOperator],
        regs: &R,
        output: &mut [i32],
        rshift: u32,
        clipmax: i32,
    ) {
        // AM amount is the same across all operators; compute it once
        let am_offset = regs.lfo_am_offset(self.choffs);

        let op1value = self.operator1(operators, regs, am_offset);

        // now that the feedback has been computed, skip the rest if all
        // volumes are clear; no need to do all this work for nothing
        if regs.ch_output_any(self.choffs) == 0 {
            return;
        }

        // The operators are computed in order, with the inputs pulled from an
        // array of values (opout) that is populated as we go:
        //    0 = 0
        //    1 = O1
        //    2 = O2
        //    3 = O3
        //    4 = (O4)
        //    5 = O1+O2
        //    6 = O1+O3
        //    7 = O2+O3
        // ymfm holds it as int16_t, so the sums truncate.
        let algorithm_ops = u32::from(ALGORITHM_OPS[regs.ch_algorithm(self.choffs) as usize]);
        let mut opout = [0i16; 8];
        opout[1] = op1value as i16;

        // compute the 14-bit volume/value of operator 2
        let opmod = i32::from(opout[bitfield(algorithm_ops, 0, 1) as usize]) >> 1;
        opout[2] = self.modulated(operators, regs, 1, opmod, am_offset) as i16;
        opout[5] = (i32::from(opout[1]) + i32::from(opout[2])) as i16;

        // compute the 14-bit volume/value of operator 3
        let opmod = i32::from(opout[bitfield(algorithm_ops, 1, 3) as usize]) >> 1;
        opout[3] = self.modulated(operators, regs, 2, opmod, am_offset) as i16;
        opout[6] = (i32::from(opout[1]) + i32::from(opout[3])) as i16;
        opout[7] = (i32::from(opout[2]) + i32::from(opout[3])) as i16;

        // compute the 14-bit volume/value of operator 4; this could be a
        // noise value on the OPM; all algorithms consume OP4 output at a
        // minimum
        let mut result = if regs.noise_enable() != 0 && self.choffs == 7 {
            operators[self.op[3] as usize].compute_noise_volume(regs, am_offset)
        } else {
            let opmod = i32::from(opout[bitfield(algorithm_ops, 4, 3) as usize]) >> 1;
            self.modulated(operators, regs, 3, opmod, am_offset)
        };
        result >>= rshift;

        // optionally add OP1, OP2, OP3
        let clipmin = -clipmax - 1;
        if bitfield(algorithm_ops, 7, 1) != 0 {
            result = clamp(result + (i32::from(opout[1]) >> rshift), clipmin, clipmax);
        }
        if bitfield(algorithm_ops, 8, 1) != 0 {
            result = clamp(result + (i32::from(opout[2]) >> rshift), clipmin, clipmax);
        }
        if bitfield(algorithm_ops, 9, 1) != 0 {
            result = clamp(result + (i32::from(opout[3]) >> rshift), clipmin, clipmax);
        }

        // add to the output
        self.add_to_output(regs, output, result);
    }

    /// OPL channel 6 in rhythm mode: the bass drum.
    fn output_rhythm_ch6<R: FmRegisters>(
        &mut self,
        operators: &[FmOperator],
        regs: &R,
        output: &mut [i32],
        rshift: u32,
    ) {
        // AM amount is the same across all operators; compute it once
        let am_offset = regs.lfo_am_offset(self.choffs);

        // Bass Drum: this uses operators 12 and 15 (i.e., channel 6) in an
        // almost-normal way, except that if the algorithm is 1, the first
        // operator is ignored instead of added in
        let opout1 = self.operator1(operators, regs, am_offset);

        // compute the 14-bit volume/value of operator 2, which is the result
        let opmod = if bitfield(regs.ch_algorithm(self.choffs), 0, 1) != 0 {
            0
        } else {
            opout1 >> 1
        };
        let result = self.modulated(operators, regs, 1, opmod, am_offset) >> rshift;

        // add to the output
        self.add_to_output(regs, output, result * 2);
    }

    /// OPL channel 7 in rhythm mode: the high hat and the snare drum.
    fn output_rhythm_ch7<R: FmRegisters>(
        &self,
        operators: &[FmOperator],
        regs: &R,
        phase_select: u32,
        output: &mut [i32],
        rshift: u32,
        clipmax: i32,
    ) {
        // AM amount is the same across all operators; compute it once
        let am_offset = regs.lfo_am_offset(self.choffs);
        let noise_state = bitfield(regs.noise_state(), 0, 1);
        let op0 = &operators[self.op[0] as usize];
        let op1 = &operators[self.op[1] as usize];

        // High Hat: this uses the envelope from operator 13 (channel 7), and
        // a combination of noise and the operator 13/17 phase select to
        // compute the phase
        let phase = (phase_select << 9) | (0xd0 >> (2 * (noise_state ^ phase_select)));
        let mut result = op0.compute_volume(regs, phase, am_offset) >> rshift;

        // Snare Drum: this uses the envelope from operator 16 (channel 7), and
        // a combination of noise and operator 13 phase to pick a phase
        let op13phase = op0.phase();
        let phase = (0x100 << bitfield(op13phase, 8, 1)) ^ (noise_state << 8);
        result += op1.compute_volume(regs, phase, am_offset) >> rshift;
        let result = clamp(result, -clipmax - 1, clipmax);

        // add to the output
        self.add_to_output(regs, output, result * 2);
    }

    /// OPL channel 8 in rhythm mode: the tom tom and the top cymbal.
    fn output_rhythm_ch8<R: FmRegisters>(
        &self,
        operators: &[FmOperator],
        regs: &R,
        phase_select: u32,
        output: &mut [i32],
        rshift: u32,
        clipmax: i32,
    ) {
        // AM amount is the same across all operators; compute it once
        let am_offset = regs.lfo_am_offset(self.choffs);
        let op0 = &operators[self.op[0] as usize];
        let op1 = &operators[self.op[1] as usize];

        // Tom Tom: this is just a single operator processed normally
        let mut result = op0.compute_volume(regs, op0.phase(), am_offset) >> rshift;

        // Top Cymbal: this uses the envelope from operator 17 (channel 8),
        // and the operator 13/17 phase select to compute the phase
        let phase = 0x100 | (phase_select << 9);
        result += op1.compute_volume(regs, phase, am_offset) >> rshift;
        let result = clamp(result, -clipmax - 1, clipmax);

        // add to the output
        self.add_to_output(regs, output, result * 2);
    }
}

//*********************************************************
//  FM ENGINE BASE
//*********************************************************

/// A set of operators and channels which together form a Yamaha FM core;
/// chips that implement other engines (ADPCM, wavetable, etc) take this
/// output and combine it with the others externally.
#[derive(Clone)]
pub(crate) struct FmEngine<R, I> {
    /// The system interface.
    intf: I,
    /// The envelope counter; the low 2 bits are a sub-counter.
    env_counter: u32,
    /// The current status register.
    status: u8,
    /// The prescale factor (2/3/6).
    clock_prescale: u8,
    /// The mask of which bits signal IRQs.
    irq_mask: u8,
    /// The current IRQ state.
    irq_state: u8,
    /// The current timer running state.
    timer_running: [u8; 2],
    /// The low 8 bits of the total number of clocks processed.
    total_clocks: u8,
    /// The mask of active channels, computed by prepare.
    active_channels: u32,
    /// The mask of channels that have been modified.
    modified_channels: u32,
    /// A counter to do periodic prepare sweeps.
    prepare_count: u32,
    /// The register set.
    regs: R,
    channels: [FmChannel; MAX_CHANNELS],
    operators: [FmOperator; MAX_OPERATORS],
}

impl<R: FmRegisters, I: Interface> FmEngine<R, I> {
    // expose some constants from the registers; not every chip reads every
    // one of them
    #[allow(dead_code)] // ymfm exposes it; no chip reads it here
    pub(crate) const OUTPUTS: usize = R::OUTPUTS;
    #[allow(dead_code)] // ymfm exposes it; no chip reads it here
    pub(crate) const CHANNELS: usize = R::CHANNELS;
    pub(crate) const ALL_CHANNELS: u32 = R::ALL_CHANNELS;
    #[allow(dead_code)] // ymfm exposes it; no chip reads it here
    pub(crate) const OPERATORS: usize = R::OPERATORS;

    // also expose status flags for consumers that inject additional bits
    pub(crate) const STATUS_TIMERA: u8 = R::STATUS_TIMERA;
    pub(crate) const STATUS_TIMERB: u8 = R::STATUS_TIMERB;
    pub(crate) const STATUS_BUSY: u8 = R::STATUS_BUSY;
    #[allow(dead_code)] // ymfm exposes it; no chip reads it here
    pub(crate) const STATUS_IRQ: u8 = R::STATUS_IRQ;

    /// Creates an engine in the state ymfm's constructor leaves it.
    pub(crate) fn new(intf: I) -> Self {
        let mut channels = [FmChannel::new(0); MAX_CHANNELS];
        for (chnum, channel) in channels.iter_mut().enumerate().take(R::CHANNELS) {
            *channel = FmChannel::new(R::channel_offset(chnum as u32));
        }
        let mut operators = [FmOperator::new(0); MAX_OPERATORS];
        for (opnum, operator) in operators.iter_mut().enumerate().take(R::OPERATORS) {
            *operator = FmOperator::new(R::operator_offset(opnum as u32));
        }
        let mut engine = FmEngine {
            intf,
            env_counter: 0,
            status: 0,
            clock_prescale: R::DEFAULT_PRESCALE as u8,
            irq_mask: R::STATUS_TIMERA | R::STATUS_TIMERB,
            irq_state: 0,
            timer_running: [0, 0],
            total_clocks: 0,
            active_channels: R::ALL_CHANNELS,
            modified_channels: R::ALL_CHANNELS,
            prepare_count: 0,
            regs: R::new(),
            channels,
            operators,
        };

        // do the initial operator assignment
        engine.assign_operators();
        engine
    }

    /// Resets the overall state.
    pub(crate) fn reset(&mut self) {
        // reset all status bits
        self.set_reset_status(0, 0xff);

        // register type-specific initialization
        self.regs.reset();

        // explicitly write to the mode register since it has side-effects
        // QUESTION (ymfm's): old cores initialize this to 0x30 -- who is
        // right?
        self.write(R::REG_MODE as u16, 0);

        // reset the channels
        for channel in self.channels.iter_mut() {
            channel.reset();
        }

        // reset the operators
        for operator in self.operators.iter_mut() {
            operator.reset();
        }
    }

    /// Iterates over the channels in `chanmask`, clocking them forward one
    /// step, and returns the envelope counter, which clocks ADPCM-A.
    pub(crate) fn clock(&mut self, chanmask: u32) -> u32 {
        // update the clock counter
        self.total_clocks = self.total_clocks.wrapping_add(1);

        // if something was modified, prepare; also prepare every 4k samples
        // to catch ending notes. The count only moves when nothing was
        // modified, as C's short-circuit `||` leaves it.
        let mut due = self.modified_channels != 0;
        if !due {
            due = self.prepare_count >= 4096;
            self.prepare_count += 1;
        }
        if due {
            // reassign operators to channels if dynamic
            if R::DYNAMIC_OPS {
                self.assign_operators();
            }

            // call each channel to prepare
            self.active_channels = 0;
            for chnum in 0..R::CHANNELS {
                if bitfield(chanmask, chnum as u32, 1) != 0
                    && self.channels[chnum].prepare(&mut self.operators, &mut self.regs)
                {
                    self.active_channels |= 1 << chnum;
                }
            }

            // reset the modified channels and prepare count
            self.modified_channels = 0;
            self.prepare_count = 0;
        }

        // if the envelope clock divider is 1, just increment by 4; otherwise,
        // increment by 1 and manually wrap when we reach the divide count
        if R::EG_CLOCK_DIVIDER == 1 {
            self.env_counter = self.env_counter.wrapping_add(4);
        } else {
            self.env_counter = self.env_counter.wrapping_add(1);
            if bitfield(self.env_counter, 0, 2) == R::EG_CLOCK_DIVIDER {
                self.env_counter = self.env_counter.wrapping_add(4 - R::EG_CLOCK_DIVIDER);
            }
        }

        // clock the noise generator
        let lfo_raw_pm = self.regs.clock_noise_and_lfo();

        // now update the state of all the channels and operators
        for chnum in 0..R::CHANNELS {
            if bitfield(chanmask, chnum as u32, 1) != 0 {
                self.channels[chnum].clock(
                    &mut self.operators,
                    &mut self.regs,
                    self.env_counter,
                    lfo_raw_pm,
                );
            }
        }

        // return the envelope counter as it is used to clock ADPCM-A
        self.env_counter
    }

    /// Adds the sum of the channels in `chanmask` to `output`, which holds
    /// one value per output.
    pub(crate) fn output(&mut self, output: &mut [i32], rshift: u32, clipmax: i32, chanmask: u32) {
        // mask out inactive channels
        let chanmask = chanmask & self.active_channels;
        let operators = &self.operators;
        let regs = &self.regs;

        // handle the rhythm case, where some of the operators are dedicated
        // to percussion (this is an OPL-specific feature)
        if regs.rhythm_enable() != 0 {
            // precompute the operator 13+17 phase selection value
            let op13phase = operators[13].phase();
            let op17phase = operators[17].phase();
            let phase_select = (bitfield(op13phase, 2, 1) ^ bitfield(op13phase, 7, 1))
                | bitfield(op13phase, 3, 1)
                | (bitfield(op17phase, 5, 1) ^ bitfield(op17phase, 3, 1));

            // sum over all the desired channels
            for chnum in 0..R::CHANNELS {
                if bitfield(chanmask, chnum as u32, 1) == 0 {
                    continue;
                }
                let channel = &mut self.channels[chnum];
                match chnum {
                    6 => channel.output_rhythm_ch6(operators, regs, output, rshift),
                    7 => channel.output_rhythm_ch7(
                        operators,
                        regs,
                        phase_select,
                        output,
                        rshift,
                        clipmax,
                    ),
                    8 => channel.output_rhythm_ch8(
                        operators,
                        regs,
                        phase_select,
                        output,
                        rshift,
                        clipmax,
                    ),
                    _ if channel.is4op::<R>() => {
                        channel.output_4op(operators, regs, output, rshift, clipmax)
                    }
                    _ => channel.output_2op(operators, regs, output, rshift, clipmax),
                }
            }
        } else {
            // sum over all the desired channels
            for chnum in 0..R::CHANNELS {
                if bitfield(chanmask, chnum as u32, 1) == 0 {
                    continue;
                }
                let channel = &mut self.channels[chnum];
                if channel.is4op::<R>() {
                    channel.output_4op(operators, regs, output, rshift, clipmax);
                } else {
                    channel.output_2op(operators, regs, output, rshift, clipmax);
                }
            }
        }
    }

    /// Writes a register, applying a key on or off it carries.
    pub(crate) fn write(&mut self, regnum: u16, data: u8) {
        // special case: writes to the mode register can impact IRQs; ymfm
        // schedules these through the interface to order them with timers
        if u32::from(regnum) == R::REG_MODE {
            self.intf.sync_mode_write(data);
            self.engine_mode_write(data);
            return;
        }

        // for now just mark all channels as modified
        self.modified_channels = R::ALL_CHANNELS;

        // most writes are passive, consumed only when needed
        if let Some((keyon_channel, keyon_opmask)) = self.regs.write(regnum, data) {
            // handle writes to the keyon register(s)
            if (keyon_channel as usize) < R::CHANNELS {
                // normal channel on/off
                self.channels[keyon_channel as usize].keyonoff(
                    &mut self.operators,
                    keyon_opmask,
                    KEYON_NORMAL,
                );
            } else if R::CHANNELS >= 9 && keyon_channel == RHYTHM_CHANNEL {
                // special case for the OPL rhythm channels
                let ops = &mut self.operators;
                self.channels[6].keyonoff(
                    ops,
                    if bitfield(keyon_opmask, 4, 1) != 0 {
                        3
                    } else {
                        0
                    },
                    KEYON_RHYTHM,
                );
                self.channels[7].keyonoff(
                    ops,
                    bitfield(keyon_opmask, 0, 1) | (bitfield(keyon_opmask, 3, 1) << 1),
                    KEYON_RHYTHM,
                );
                self.channels[8].keyonoff(
                    ops,
                    bitfield(keyon_opmask, 2, 1) | (bitfield(keyon_opmask, 1, 1) << 1),
                    KEYON_RHYTHM,
                );
            }
        }
    }

    /// Returns the current status.
    pub(crate) fn status(&self) -> u8 {
        self.status & !R::STATUS_BUSY & !(self.regs.status_mask() as u8)
    }

    /// Sets and resets bits in the status register, updating the IRQ state.
    pub(crate) fn set_reset_status(&mut self, set: u8, reset: u8) -> u8 {
        self.status = (self.status | set) & !(reset | R::STATUS_BUSY);
        self.intf.sync_check_interrupts();
        self.engine_check_interrupts();
        self.status & !(self.regs.status_mask() as u8)
    }

    /// Sets the IRQ mask.
    pub(crate) fn set_irq_mask(&mut self, mask: u8) {
        self.irq_mask = mask;
        self.intf.sync_check_interrupts();
        self.engine_check_interrupts();
    }

    /// Returns the current clock prescale.
    pub(crate) fn clock_prescale(&self) -> u32 {
        u32::from(self.clock_prescale)
    }

    /// Sets the prescale factor (2/3/6).
    pub(crate) fn set_clock_prescale(&mut self, prescale: u32) {
        self.clock_prescale = prescale as u8;
    }

    /// Computes the sample rate for an input clock.
    pub(crate) fn sample_rate(&self, baseclock: u32) -> u32 {
        baseclock / (u32::from(self.clock_prescale) * R::OPERATORS as u32)
    }

    /// Returns the system interface.
    pub(crate) fn intf(&self) -> &I {
        &self.intf
    }

    /// Returns the system interface, mutably.
    pub(crate) fn intf_mut(&mut self) -> &mut I {
        &mut self.intf
    }

    /// Returns the register set.
    pub(crate) fn regs(&self) -> &R {
        &self.regs
    }

    /// Returns the register set, mutably.
    pub(crate) fn regs_mut(&mut self) -> &mut R {
        &mut self.regs
    }

    /// Invalidates any caches.
    pub(crate) fn invalidate_caches(&mut self) {
        self.modified_channels = R::ALL_CHANNELS;
    }

    /// Handles a timer's expiry: signals status and possibly IRQs, triggers
    /// CSM, and reloads the timer.
    pub(crate) fn engine_timer_expired(&mut self, tnum: u32) {
        // update status
        if tnum == 0 && self.regs.enable_timer_a() != 0 {
            self.set_reset_status(R::STATUS_TIMERA, 0);
        } else if tnum == 1 && self.regs.enable_timer_b() != 0 {
            self.set_reset_status(R::STATUS_TIMERB, 0);
        }

        // if timer A fired in CSM mode, trigger CSM on all relevant channels
        if tnum == 0 && self.regs.csm() != 0 {
            for chnum in 0..R::CHANNELS {
                if bitfield(R::CSM_TRIGGER_MASK, chnum as u32, 1) != 0 {
                    self.channels[chnum].keyonoff(&mut self.operators, 0xf, KEYON_CSM);
                    self.modified_channels |= 1 << chnum;
                }
            }
        }

        // reset
        self.timer_running[tnum as usize] = 0;
        self.update_timer(tnum, 1, 0);
    }

    /// Checks the interrupt sources for interrupts.
    pub(crate) fn engine_check_interrupts(&mut self) {
        // update the state
        let old_state = self.irq_state;
        self.irq_state =
            u8::from((self.status & self.irq_mask & !(self.regs.status_mask() as u8)) != 0);

        // set the IRQ status bit
        if self.irq_state != 0 {
            self.status |= R::STATUS_IRQ;
        } else {
            self.status &= !R::STATUS_IRQ;
        }

        // if changed, signal the new state
        if old_state != self.irq_state {
            self.intf.update_irq(self.irq_state != 0);
        }
    }

    /// Handles a mode register write.
    pub(crate) fn engine_mode_write(&mut self, data: u8) {
        // mark all channels as modified
        self.modified_channels = R::ALL_CHANNELS;

        // actually write the mode register now
        self.regs.write(R::REG_MODE as u16, data);

        // reset IRQ status -- when written, all other bits are ignored
        // QUESTION (ymfm's): should this maybe just reset the IRQ bit and
        // not all the bits? That is, check_interrupts would only set, this
        // would only clear?
        if self.regs.irq_reset() != 0 {
            self.set_reset_status(0, 0x78);
        } else {
            // reset timer status
            let mut reset_mask = 0u8;
            if self.regs.reset_timer_b() != 0 {
                reset_mask |= R::STATUS_TIMERB;
            }
            if self.regs.reset_timer_a() != 0 {
                reset_mask |= R::STATUS_TIMERA;
            }
            self.set_reset_status(0, reset_mask);

            // load timers; note that timer B gets a small negative adjustment
            // because the *16 multiplier is free-running, so the first tick
            // of the clock is a bit shorter
            let load_b = self.regs.load_timer_b();
            self.update_timer(1, load_b, -i32::from(self.total_clocks & 15));
            let load_a = self.regs.load_timer_a();
            self.update_timer(0, load_a, 0);
        }
    }

    /// Assigns the current set of operators to channels.
    fn assign_operators(&mut self) {
        let mut map = [operator_list(0xff, 0xff, 0xff, 0xff); MAX_CHANNELS];
        self.regs.operator_map(&mut map);

        for (channel, packed) in self.channels.iter_mut().zip(map).take(R::CHANNELS) {
            for index in 0..4 {
                let opnum = bitfield(packed, 8 * index as u32, 8) as u8;
                channel.assign(index, opnum, &mut self.operators);
            }
        }
    }

    /// Updates the state of the given timer.
    fn update_timer(&mut self, tnum: u32, enable: u32, delta_clocks: i32) {
        // if the timer is live, but not currently enabled, set the timer
        if enable != 0 && self.timer_running[tnum as usize] == 0 {
            // period comes from the registers, and is different for each
            let mut period = if tnum == 0 {
                1024 - self.regs.timer_a_value()
            } else {
                16 * (256 - self.regs.timer_b_value())
            };

            // caller can also specify a delta to account for other effects
            period = period.wrapping_add(delta_clocks as u32);

            // reset it
            let duration = period
                .wrapping_mul(R::OPERATORS as u32)
                .wrapping_mul(u32::from(self.clock_prescale));
            self.intf.set_timer(tnum, duration as i32);
            self.timer_running[tnum as usize] = 1;
        }
        // if the timer is not live, ensure it is not enabled
        else if enable == 0 {
            self.intf.set_timer(tnum, -1);
            self.timer_running[tnum as usize] = 0;
        }
    }
}
