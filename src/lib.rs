//! Convert Notability `.note` and `.ntb` files to PDF, SVG or `.note`.
#[macro_use]
extern crate lopdf;

mod converter;
pub mod pdf;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub use converter::{convert, run_cli, Conversion, OutputFormat};
