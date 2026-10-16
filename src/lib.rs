// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause

//! A pure-Rust port of ymfm.

#![no_std]
#![warn(missing_docs)]

mod fm;
pub mod opl;
pub mod opm;
pub mod ymfm;

pub use ymfm::{AccessClass, Interface, NullInterface, Output};
