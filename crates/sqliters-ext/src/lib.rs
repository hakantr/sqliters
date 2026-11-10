//! `sqliters-ext`: built-in virtual table modules and extensions for `sqliters`.
#![forbid(unsafe_code)]

pub mod generate_series;

pub use generate_series::GenerateSeriesModule;
