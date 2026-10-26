//! Logic for the Cairn trail device that does not touch hardware, so it can be
//! tested on the host. The firmware feeds it bytes and register values; it
//! returns decoded values. Nothing here allocates.
#![no_std]

pub mod battery;
pub mod bme280;
pub mod elevation;
pub mod gpt;
pub mod pcf8563;
pub mod ubx;
