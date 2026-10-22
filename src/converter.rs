mod archive;
mod cli;
mod geometry;
mod model;
mod note;
mod note_writer;
mod ntb;
mod render;
mod util;

use std::io::Cursor;
use std::path::Path;
use zip::ZipArchive;

pub use model::OutputFormat;

pub fn run_cli(args: Vec<String>) -> crate::Result<()> {
    cli::run(args)
}

/// A file converted in memory.
pub struct Conversion {
    pub bytes: Vec<u8>,
    /// Content that could not be converted.
    pub warnings: Vec<String>,
}

/// Converts a `.note` or `.ntb` file held in memory. The extension of `file_name`
/// tells the formats apart, and its stem titles a note that has no title.
///
/// A `.note` is already in the note format and is returned unchanged.
pub fn convert(input: &[u8], file_name: &str, format: OutputFormat) -> crate::Result<Conversion> {
    let path = Path::new(file_name);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let is_ntb = match extension.as_deref() {
        Some("ntb") => true,
        Some("note") => false,
        _ => return Err(format!("{file_name} is not a .note or .ntb file").into()),
    };
    let bundle = || ZipArchive::new(Cursor::new(input));
    if format == OutputFormat::Note {
        if !is_ntb {
            return Ok(Conversion {
                bytes: input.to_vec(),
                warnings: Vec::new(),
            });
        }
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let note = ntb::read_ntb_export(bundle()?, &stem)?;
        let mut bytes = Cursor::new(Vec::new());
        note_writer::write_note_bundle_to(&note, &mut bytes)?;
        return Ok(Conversion {
            bytes: bytes.into_inner(),
            warnings: note.warnings,
        });
    }
    let document = if is_ntb {
        ntb::read_ntb_document(bundle()?)?
    } else {
        note::read_note_document(bundle()?)?
    };
    Ok(Conversion {
        bytes: render::render_note(&document, &mut bundle()?, format)?,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::{convert, note, note_writer, ntb, render, OutputFormat};
    use std::path::Path;

    const SAMPLES: [&str; 2] = [
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/input/NTB Note Apr 26, 2026.ntb"
        ),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/input/NOTE Note Apr 26, 2026.note"
        ),
    ];

    #[test]
    fn converts_in_memory_exactly_as_from_files() {
        let directory =
            std::env::temp_dir().join(format!("notability-convert-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        for sample in SAMPLES
            .map(Path::new)
            .into_iter()
            .filter(|path| path.exists())
        {
            let input = std::fs::read(sample).unwrap();
            let name = sample.file_name().unwrap().to_str().unwrap();
            let is_ntb = name.ends_with(".ntb");
            let document = if is_ntb {
                ntb::load_ntb_document(sample).unwrap()
            } else {
                note::load_note_document(sample).unwrap()
            };
            let pdf = directory.join("out.pdf");
            render::write_note_output(&document, sample, &pdf, OutputFormat::Pdf).unwrap();
            let converted = convert(&input, name, OutputFormat::Pdf).unwrap();
            assert!(
                converted.bytes == std::fs::read(&pdf).unwrap(),
                "{name} to PDF"
            );
            let converted = convert(&input, name, OutputFormat::Note).unwrap();
            if is_ntb {
                let note = directory.join("out.note");
                note_writer::write_note_bundle(&ntb::load_ntb_export(sample).unwrap(), &note)
                    .unwrap();
                assert!(
                    converted.bytes == std::fs::read(&note).unwrap(),
                    "{name} to .note"
                );
            } else {
                assert!(converted.bytes == input, "a .note stays as it is");
            }
        }
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn refuses_files_that_are_not_notes() {
        let error = convert(b"text", "notes.txt", OutputFormat::Pdf)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("not a .note or .ntb file"));
        assert!(convert(b"not a zip", "a.ntb", OutputFormat::Note).is_err());
    }
}
