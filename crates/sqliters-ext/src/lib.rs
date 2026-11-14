//! `sqliters-ext`: built-in virtual table modules and extensions for `sqliters`.
#![forbid(unsafe_code)]

pub mod fts5;
mod fts5_index;
mod fts5_tokenize;
mod fts5_unicode;
pub mod generate_series;
pub mod json_vtab;
pub mod rtree;

pub use fts5::{Fts5AuxRegistry, Fts5Module, Fts5VocabModule};
pub use generate_series::GenerateSeriesModule;
pub use json_vtab::{JsonEachModule, JsonTreeModule, JsonbEachModule, JsonbTreeModule};
pub use rtree::{RTreeCoordMode, RTreeModule, eval_rtreedepth, eval_rtreenode, run_rtreecheck};
