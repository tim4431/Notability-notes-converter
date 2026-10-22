//! Write Notability `.note` bundles.
//!
//! The output mirrors a note saved by Notability 16 (`sessionFormatVersion` 9): a ZIP
//! whose members live under one `<Name>/` folder holding `Session.plist` (a
//! `GLKeyedArchiver` object graph), `metadata.plist`, `thumb12x.png`,
//! `Recordings/library.plist` and the media under `Images/`, `Assets/` and `PDFs/`.
//!
//! Everything in [`NoteExport`] is already in `.note` document coordinates: one
//! continuous vertical strip of pages, `page_width` wide, with x measured from the
//! content inset (`page_width / 38.4`) rather than from the paper edge.
use super::model::TextSpan;
use crate::Result;
use std::fs::File;
use std::io::{Cursor, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

mod archive;
mod ink;
mod session;
mod text;

/// Seconds between the Unix epoch and the `NSDate` reference date (2001-01-01).
const APPLE_EPOCH_OFFSET_SECONDS: f64 = 978_307_200.0;
const THUMBNAIL_WIDTH: u32 = 576;
const LIBRARY_APPLICATION_VERSION: &str = "6852";
const MAX_FOLDER_NAME_BYTES: usize = 200;

pub(crate) type Point = (f64, f64);

pub(crate) struct NoteExport {
    pub(crate) title: String,
    pub(crate) language: String,
    pub(crate) font_family: String,
    pub(crate) font_size: f32,
    pub(crate) created_ms: Option<i64>,
    pub(crate) modified_ms: Option<i64>,
    pub(crate) page_width: f32,
    pub(crate) page_height: f32,
    pub(crate) paper: Paper,
    pub(crate) pdfs: Vec<Vec<u8>>,
    pub(crate) pdf_pages: Vec<PdfPage>,
    /// Ink in z-order. Shapes and groups take part in the same order as strokes.
    pub(crate) ink: Vec<InkObject>,
    /// Media in the order of their events; `frame.z_index` orders them on a page.
    pub(crate) media: Vec<Media>,
    pub(crate) text: RichText,
    pub(crate) thumbnail: Option<Vec<u8>>,
    /// Seeds the UUIDs, so converting the same bundle twice gives the same note.
    pub(crate) seed: u64,
    /// Content that could not be converted, for the conversion report.
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Paper {
    pub(crate) identifier: String,
    pub(crate) lines: Option<PaperLines>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PaperLines {
    pub(crate) pattern: LinePattern,
    pub(crate) spacing_inches: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinePattern {
    Ruled,
    Dots,
    Grid,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PdfPage {
    pub(crate) file: usize,
    pub(crate) source_page: usize,
}

#[derive(Clone, Debug)]
pub(crate) enum InkObject {
    Stroke(Stroke),
    Shape(Shape),
    /// Members are strokes and shapes, already placed in document coordinates.
    Group(Vec<InkObject>),
}

/// One pen, pencil or highlighter stroke. Pieces left by the eraser are separate
/// curves of the same stroke.
#[derive(Clone, Debug)]
pub(crate) struct Stroke {
    pub(crate) curves: Vec<Curve>,
    pub(crate) width: f32,
    pub(crate) rgba: [u8; 4],
    pub(crate) style: u8,
    /// 1 dashed, 2 dotted.
    pub(crate) dash: Option<u8>,
}

/// A cubic Bezier chain: an anchor, then (control, control, anchor) per segment,
/// with one node per anchor.
#[derive(Clone, Debug)]
pub(crate) struct Curve {
    pub(crate) points: Vec<(f32, f32)>,
    pub(crate) nodes: Vec<Node>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Node {
    pub(crate) fractional_width: f32,
    pub(crate) force: f32,
    /// Pencil altitude in radians; pi/2 for an upright or unknown tilt.
    pub(crate) altitude: f32,
    pub(crate) azimuth: (f32, f32),
}

impl Node {
    pub(crate) const UPRIGHT: Node = Node {
        fractional_width: 1.0,
        force: 1.0,
        altitude: std::f32::consts::FRAC_PI_2,
        azimuth: (1.0, 0.0),
    };
}

#[derive(Clone, Debug)]
pub(crate) struct Shape {
    pub(crate) geometry: ShapeGeometry,
    pub(crate) width: f64,
    pub(crate) rgba: [u8; 4],
    pub(crate) style: u8,
    pub(crate) dash: Option<u8>,
    /// Pencil force, stored on pencil shapes only.
    pub(crate) force: Option<f64>,
}

#[derive(Clone, Debug)]
pub(crate) enum ShapeGeometry {
    /// A straight line, or a quadratic curve through `control`.
    Line {
        start: Point,
        end: Point,
        control: Option<Point>,
    },
    /// A closed polygon.
    Polygon(Vec<Point>),
    /// The ellipse inscribed in a possibly rotated rectangle, given as its top-left,
    /// top-right, bottom-right and bottom-left corners before rotation.
    Ellipse([Point; 4]),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MediaFrame {
    /// Top-left corner before rotation.
    pub(crate) origin: (f32, f32),
    pub(crate) size: (f32, f32),
    /// Radians, about the center.
    pub(crate) rotation: f32,
    pub(crate) z_index: i64,
}

#[derive(Clone, Debug)]
pub(crate) enum Media {
    Image {
        frame: MediaFrame,
        image: ImageData,
        locked: bool,
    },
    Sticky {
        frame: MediaFrame,
        color: [u8; 4],
        lines: Option<PaperLines>,
    },
    TextBox {
        frame: MediaFrame,
        text: RichText,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ImageData {
    pub(crate) bytes: Vec<u8>,
    pub(crate) kind: ImageKind,
    /// The size crop rectangles refer to; not always the pixel size.
    pub(crate) logical_size: (f32, f32),
    pub(crate) crop: Option<[f32; 4]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImageKind {
    Jpeg,
    Png,
    Gif,
}

impl ImageKind {
    pub(crate) fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&[0xff, 0xd8]) {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RichText {
    pub(crate) text: String,
    pub(crate) spans: Vec<TextSpan>,
}

pub(crate) fn write_note_bundle(note: &NoteExport, output_path: &Path) -> Result<()> {
    let mut ids = IdSource::new(note.seed);
    let folder = bundle_folder_name(&note.title);
    let session = session::build_session(note, &folder, &mut ids)?;
    let metadata = metadata_plist(note, &folder, &mut ids)?;
    let thumbnail = thumbnail_png(note)?;
    let modified = zip_time(note.modified_ms.or(note.created_ms));

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut zip = ZipWriter::new(File::create(output_path)?);
    // Notability stores every member uncompressed, with explicit folder entries.
    let mut options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644);
    if let Some(time) = modified {
        options = options.last_modified_time(time);
    }
    let directories = [
        "",
        "Assets/",
        "Images/",
        "PDFs/",
        "HandwritingIndex/",
        "Recordings/",
    ];
    for directory in directories {
        zip.add_directory(
            format!("{folder}/{directory}"),
            options.unix_permissions(0o755),
        )?;
    }
    let mut added_directories = Vec::new();
    for (name, _) in &session.files {
        if let Some((directory, _)) = name.rsplit_once('/') {
            let directory = format!("{directory}/");
            if !directories.contains(&directory.as_str()) && !added_directories.contains(&directory)
            {
                zip.add_directory(
                    format!("{folder}/{directory}"),
                    options.unix_permissions(0o755),
                )?;
                added_directories.push(directory);
            }
        }
    }
    let mut members = vec![
        ("Session.plist".to_owned(), session.session.as_slice()),
        ("metadata.plist".to_owned(), metadata.as_slice()),
        ("thumb12x.png".to_owned(), thumbnail.as_slice()),
    ];
    let library = library_plist()?;
    members.push(("Recordings/library.plist".to_owned(), library.as_slice()));
    members.extend(
        session
            .files
            .iter()
            .map(|(name, bytes)| (name.clone(), bytes.as_slice())),
    );
    for (name, bytes) in members {
        zip.start_file(format!("{folder}/{name}"), options)?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(())
}

/// The package folder name, which `packagePath` repeats: the title as a single path
/// component of at most 200 bytes.
fn bundle_folder_name(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' => '-',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect();
    let name = name.trim().trim_start_matches('.').trim();
    let mut end = name.len().min(MAX_FOLDER_NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let name = name[..end].trim_end();
    if name.is_empty() {
        "Note".to_owned()
    } else {
        name.to_owned()
    }
}

fn metadata_plist(note: &NoteExport, package_path: &str, ids: &mut IdSource) -> Result<Vec<u8>> {
    use archive::{integer, ArchiveBuilder};
    let created = note.created_ms.unwrap_or_default();
    let modified = note.modified_ms.unwrap_or(created);
    let mut b = ArchiveBuilder::new("NSKeyedArchiver", "root");
    let root = b.reserve();
    let products = b.array(Vec::new());
    let name = b.string(&note.title);
    let package_path = b.string(package_path);
    let created_date = b.date(created);
    let modified_date = b.date(modified);
    // Last change per content type (bit flags whose meaning is unknown); a new note
    // dates 4 and 16 at creation and the others at its last edit.
    let mut per_type = Vec::new();
    for (key, ms) in [
        (4, created),
        (2, modified),
        (16, created),
        (1, modified),
        (8, modified),
    ] {
        let key = b.add(integer(key));
        let date = b.date(ms);
        per_type.push((key, date));
    }
    let per_type = b.dictionary(true, per_type);
    let subject = b.string("unsortedNotesKey");
    let tags = b.string("");
    let uuid = b.string(&uuid_string(&ids.uuid()));
    b.fill_object(
        &root,
        "SessionInfo",
        vec![
            ("associatedProductsKey", products.clone()),
            ("galleryPublishHistoryKey", products),
            ("noteCreationDateKey", created_date),
            ("noteHasRecordingKey", plist::Value::Boolean(false)),
            ("noteLastChangeDatePerTypeKey", per_type),
            ("noteModifiedDateKey", modified_date),
            ("noteName", name),
            ("notePackagePath", package_path),
            ("noteSubject", subject),
            ("noteTags", tags),
            ("uuidKey", uuid),
        ],
    );
    b.finish(&root)
}

fn library_plist() -> Result<Vec<u8>> {
    let mut library = plist::Dictionary::new();
    library.insert(
        "application version".to_owned(),
        LIBRARY_APPLICATION_VERSION.into(),
    );
    library.insert("library-format-version".to_owned(), "1.0".into());
    library.insert(
        "recordings".to_owned(),
        plist::Value::Dictionary(plist::Dictionary::new()),
    );
    let mut bytes = Vec::new();
    plist::Value::Dictionary(library).to_writer_xml(&mut bytes)?;
    Ok(bytes)
}

/// `thumb12x.png`: the first page at 576 px wide. Readers also take the page aspect
/// from it, so it is sized to the page even when the bundle's preview is not.
fn thumbnail_png(note: &NoteExport) -> Result<Vec<u8>> {
    let aspect = match note.pdf_pages.first() {
        Some(page) => pdf_page_aspect(note, page).unwrap_or(note.page_height / note.page_width),
        None => note.page_height / note.page_width,
    };
    let height = ((THUMBNAIL_WIDTH as f32 * aspect).round() as u32).clamp(1, 16 * THUMBNAIL_WIDTH);
    let image = note
        .thumbnail
        .as_deref()
        .and_then(|bytes| image::load_from_memory(bytes).ok())
        .map(|preview| {
            preview
                .resize_exact(
                    THUMBNAIL_WIDTH,
                    height,
                    image::imageops::FilterType::Triangle,
                )
                .to_rgba8()
        })
        .unwrap_or_else(|| {
            image::RgbaImage::from_pixel(THUMBNAIL_WIDTH, height, image::Rgba([255, 255, 255, 255]))
        });
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

fn pdf_page_aspect(note: &NoteExport, page: &PdfPage) -> Option<f32> {
    let doc = lopdf::Document::load_mem(note.pdfs.get(page.file)?).ok()?;
    let (width, height) = crate::pdf::page_size(&doc, page.source_page).ok()?;
    Some(height / width)
}

pub(super) fn apple_seconds(unix_ms: i64) -> f64 {
    unix_ms as f64 / 1000.0 - APPLE_EPOCH_OFFSET_SECONDS
}

fn zip_time(unix_ms: Option<i64>) -> Option<zip::DateTime> {
    let seconds = unix_ms?.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    zip::DateTime::from_date_and_time(
        u16::try_from(year).ok()?,
        month as u8,
        day as u8,
        (time / 3600) as u8,
        (time / 60 % 60) as u8,
        (time % 60) as u8,
    )
    .ok()
}

/// Deterministic UUIDs (SplitMix64 over a seed).
pub(super) struct IdSource(u64);

impl IdSource {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(super) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    /// A version 4 UUID.
    pub(super) fn uuid(&mut self) -> [u8; 16] {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&self.next_u64().to_le_bytes());
        bytes[8..].copy_from_slice(&self.next_u64().to_le_bytes());
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        bytes
    }
}

pub(super) fn uuid_string(bytes: &[u8; 16]) -> String {
    let hex: String = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// FNV-1a, for seeding [`IdSource`] from file contents.
pub(crate) fn content_seed(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests;
