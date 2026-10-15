// This file is a Rust translation of ymfm's `ymfm.h` at commit 81aec25
// (https://github.com/aaronsgiles/ymfm), with changes: the debugging helpers,
// the WAV file writer and the saved-state class are gone (a chip is `Clone`,
// which is how its state is saved); `ymfm_output` is `Output`, a plain array
// wrapper; `ymfm_interface` is the `Interface` trait, whose methods lose the
// `ymfm_` prefix and which a chip owns rather than refers to. Because the
// engine cannot be called back through the interface, the two
// synchronization hooks become notifications made just before the engine
// applies the change itself, and a timer's expiry is reported by calling the
// chip's `timer_expired`. The floating-point helpers are kept as ymfm writes
// them.
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

//! What every chip shares: the interface to the host, the output type, and
//! the helpers for the floating-point format of Yamaha's external DACs.

/// Extracts `length` bits of `value` starting at bit `start`.
#[inline(always)]
pub(crate) const fn bitfield(value: u32, start: u32, length: u32) -> u32 {
    (value >> start) & ((1 << length) - 1)
}

/// Clamps `value` between `minval` and `maxval`.
#[inline(always)]
pub(crate) const fn clamp(value: i32, minval: i32, maxval: i32) -> i32 {
    if value < minval {
        minval
    } else if value > maxval {
        maxval
    } else {
        value
    }
}

// Many of the Yamaha FM chips emit a floating-point value, which is sent to a
// DAC for processing. The exact format of this floating-point value is
// documented below. This description only makes sense if the "internal"
// format treats sign as 1=positive and 0=negative, so the helpers below
// presume that.
//
// Internal OPx data      16-bit signed data     Exp Sign Mantissa
// =================      =================      === ==== ========
// 1 1xxxxxxxx------  ->  0 1xxxxxxxx------  ->  111   1  1xxxxxxx
// 1 01xxxxxxxx-----  ->  0 01xxxxxxxx-----  ->  110   1  1xxxxxxx
// 1 001xxxxxxxx----  ->  0 001xxxxxxxx----  ->  101   1  1xxxxxxx
// 1 0001xxxxxxxx---  ->  0 0001xxxxxxxx---  ->  100   1  1xxxxxxx
// 1 00001xxxxxxxx--  ->  0 00001xxxxxxxx--  ->  011   1  1xxxxxxx
// 1 000001xxxxxxxx-  ->  0 000001xxxxxxxx-  ->  010   1  1xxxxxxx
// 1 000000xxxxxxxxx  ->  0 000000xxxxxxxxx  ->  001   1  xxxxxxxx
// 0 111111xxxxxxxxx  ->  1 111111xxxxxxxxx  ->  001   0  xxxxxxxx
// 0 111110xxxxxxxx-  ->  1 111110xxxxxxxx-  ->  010   0  0xxxxxxx
// 0 11110xxxxxxxx--  ->  1 11110xxxxxxxx--  ->  011   0  0xxxxxxx
// 0 1110xxxxxxxx---  ->  1 1110xxxxxxxx---  ->  100   0  0xxxxxxx
// 0 110xxxxxxxx----  ->  1 110xxxxxxxx----  ->  101   0  0xxxxxxx
// 0 10xxxxxxxx-----  ->  1 10xxxxxxxx-----  ->  110   0  0xxxxxxx
// 0 0xxxxxxxx------  ->  1 0xxxxxxxx------  ->  111   0  0xxxxxxx

/// Converts a 32-bit signed value to the signed 3.10 floating-point format.
pub fn encode_fp(value: i32) -> i16 {
    if value < -32768 {
        #[allow(clippy::identity_op)] // as ymfm writes it
        return (7 << 10) | 0x000;
    }
    if value > 32767 {
        return (7 << 10) | 0x3ff;
    }
    // Count the leading sign bits after the sign, inverting negative values
    // so that a count of leading zeros does it.
    let scanvalue = value ^ (value >> 31);
    // The exponent comes from the leading bits from bit 14 down, at least 1.
    let exponent = (7 - ((scanvalue as u32) << 17).leading_zeros() as i32).max(1);
    let mantissa = value >> (exponent - 1);
    // Assemble the result, inverting the sign.
    (((exponent << 10) | (mantissa & 0x3ff)) ^ 0x200) as i16
}

/// Converts a 3.10 floating-point value back to a signed 16-bit value.
pub fn decode_fp(value: i16) -> i16 {
    // Invert the sign and the exponent.
    let value = value ^ 0x1e00;
    // Shift the mantissa up to 16 bits, then apply the inverted exponent.
    (value << 6) >> bitfield(value as u16 as u32, 10, 3)
}

/// Returns what a round trip through `encode_fp` and `decode_fp` leaves of a
/// value: the low bits its exponent cannot carry are cleared.
pub fn roundtrip_fp(value: i32) -> i16 {
    if value < -32768 {
        return -32768;
    }
    if value > 32767 {
        return 32767;
    }
    let scanvalue = value ^ (value >> 31);
    let exponent = (7 - ((scanvalue as u32) << 17).leading_zeros() as i32).max(1);
    // Shift back and forth to clear the bits that are lost.
    let exponent = exponent - 1;
    let mask = (1i32 << exponent) - 1;
    (value & !mask) as i16
}

/// The envelope generator states. The values index the cached rate table.
pub(crate) const EG_DEPRESS: usize = 0; // OPLL only; set EG_HAS_DEPRESS to enable
pub(crate) const EG_ATTACK: usize = 1;
pub(crate) const EG_DECAY: usize = 2;
pub(crate) const EG_SUSTAIN: usize = 3;
pub(crate) const EG_RELEASE: usize = 4;
pub(crate) const EG_REVERB: usize = 5; // OPQ/OPZ only; set EG_HAS_REVERB to enable
pub(crate) const EG_STATES: usize = 6;

/// The kinds of memory and I/O a chip reaches outside itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccessClass {
    /// General-purpose I/O: the OPM and OPZ CT pins, the SSG ports, the
    /// Y8950 keyboard and I/O ports.
    Io = 0,
    /// ADPCM-A sample ROM.
    AdpcmA = 1,
    /// ADPCM-B sample ROM or RAM.
    AdpcmB = 2,
    /// PCM wavetable memory.
    Pcm = 3,
}

/// The number of access classes.
pub const ACCESS_CLASSES: usize = 4;

/// One sample of every output a chip has, as `N` signed values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output<const N: usize> {
    /// The outputs, in the chip's order.
    pub data: [i32; N],
}

impl<const N: usize> Default for Output<N> {
    fn default() -> Self {
        Output { data: [0; N] }
    }
}

impl<const N: usize> Output<N> {
    /// Sets every output to zero.
    pub fn clear(&mut self) -> &mut Self {
        self.data = [0; N];
        self
    }

    /// Clamps every output to a signed 16-bit value.
    pub fn clamp16(&mut self) -> &mut Self {
        for value in self.data.iter_mut() {
            *value = clamp(*value, -32768, 32767);
        }
        self
    }

    /// Runs every output through the floating-point format of the external
    /// DAC, as `roundtrip_fp` does.
    pub fn roundtrip_fp(&mut self) -> &mut Self {
        for value in self.data.iter_mut() {
            *value = i32::from(roundtrip_fp(*value));
        }
        self
    }
}

/// The connection between a chip and the system around it: timers, the IRQ
/// line, busy time and external memory.
///
/// Every method has a default that is enough for a chip that only makes
/// sound: timers never fire, the chip is never busy, and external memory
/// reads as zero. A host that wants timers arranges, when `set_timer` asks,
/// to call the chip's `timer_expired` once the duration has passed.
///
/// Durations are in clocks of the chip's input clock. A chip owns its
/// interface; `&mut T` is an interface too, for a host that keeps its own.
pub trait Interface {
    /// Called when a write reaches the mode register, which can affect the
    /// timers and IRQs, just before the chip applies it.
    fn sync_mode_write(&mut self, _data: u8) {}

    /// Called when the chip's status changes, which can affect the IRQ
    /// state, just before the chip checks it.
    fn sync_check_interrupts(&mut self) {}

    /// Asks the host to report timer `tnum`'s expiry, through the chip's
    /// `timer_expired`, once `duration_in_clocks` have passed; a negative
    /// duration cancels the timer.
    fn set_timer(&mut self, _tnum: u32, _duration_in_clocks: i32) {}

    /// Tells the host the chip is busy until `clocks` have passed.
    fn set_busy_end(&mut self, _clocks: u32) {}

    /// Asks whether the chip is still busy since the last `set_busy_end`.
    fn is_busy(&mut self) -> bool {
        false
    }

    /// Reports a change of the IRQ line.
    fn update_irq(&mut self, _asserted: bool) {}

    /// Reads a byte of external memory or I/O.
    fn external_read(&mut self, _access: AccessClass, _address: u32) -> u8 {
        0
    }

    /// Writes a byte to external memory or I/O.
    fn external_write(&mut self, _access: AccessClass, _address: u32, _data: u8) {}
}

impl<T: Interface + ?Sized> Interface for &mut T {
    fn sync_mode_write(&mut self, data: u8) {
        (**self).sync_mode_write(data)
    }
    fn sync_check_interrupts(&mut self) {
        (**self).sync_check_interrupts()
    }
    fn set_timer(&mut self, tnum: u32, duration_in_clocks: i32) {
        (**self).set_timer(tnum, duration_in_clocks)
    }
    fn set_busy_end(&mut self, clocks: u32) {
        (**self).set_busy_end(clocks)
    }
    fn is_busy(&mut self) -> bool {
        (**self).is_busy()
    }
    fn update_irq(&mut self, asserted: bool) {
        (**self).update_irq(asserted)
    }
    fn external_read(&mut self, access: AccessClass, address: u32) -> u8 {
        (**self).external_read(access, address)
    }
    fn external_write(&mut self, access: AccessClass, address: u32, data: u8) {
        (**self).external_write(access, address, data)
    }
}

/// An interface with every default: no timers, never busy, no memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NullInterface;

impl Interface for NullInterface {}
