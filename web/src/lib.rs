//! Browser bindings: the converter compiled to WebAssembly and run in a web worker,
//! so notes are converted on the device and never uploaded.
use std::io::{Cursor, Write};
use wasm_bindgen::prelude::*;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

#[wasm_bindgen]
pub struct Conversion {
    bytes: Vec<u8>,
    warnings: Vec<String>,
}

#[wasm_bindgen]
impl Conversion {
    /// The converted file.
    #[wasm_bindgen(getter)]
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    /// Content that could not be converted.
    #[wasm_bindgen(getter)]
    pub fn warnings(&self) -> Vec<String> {
        self.warnings.clone()
    }
}

/// Converts a `.note` or `.ntb` file to `format`: `pdf`, `svg` or `note`.
#[wasm_bindgen]
pub fn convert(input: &[u8], file_name: &str, format: &str) -> Result<Conversion, JsError> {
    let format = format
        .parse()
        .map_err(|error: String| JsError::new(&error))?;
    let conversion = notability_converter::convert(input, file_name, format)
        .map_err(|error| JsError::new(&error.to_string()))?;
    Ok(Conversion {
        bytes: conversion.bytes,
        warnings: conversion.warnings,
    })
}

/// Collects converted files into one ZIP archive, for a single download.
#[wasm_bindgen]
pub struct Archive {
    writer: ZipWriter<Cursor<Vec<u8>>>,
}

#[wasm_bindgen]
impl Archive {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Archive {
        Archive {
            writer: ZipWriter::new(Cursor::new(Vec::new())),
        }
    }

    pub fn add(&mut self, name: &str, bytes: &[u8]) -> Result<(), JsError> {
        // PDFs and notes are compressed already.
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        self.writer
            .start_file(name, options)
            .map_err(|error| JsError::new(&error.to_string()))?;
        self.writer
            .write_all(bytes)
            .map_err(|error| JsError::new(&error.to_string()))
    }

    pub fn finish(self) -> Result<Vec<u8>, JsError> {
        let output = self
            .writer
            .finish()
            .map_err(|error| JsError::new(&error.to_string()))?;
        Ok(output.into_inner())
    }
}
