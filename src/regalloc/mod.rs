//! Register allocation
//!
//! This is a graph-colouring based on Chaitin 1982

mod colouring;
mod interference;
mod liveness;

pub use colouring::{Allocation, Location, Reg};
