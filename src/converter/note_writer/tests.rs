use super::{
    write_note_bundle, Curve, InkObject, Node, NoteExport, Paper, RichText, Shape, ShapeGeometry,
    Stroke,
};
use crate::converter::archive::KeyedArchive;
use crate::converter::model::{NoteDocument, StrokeCurve};
use crate::converter::note::{load_note_document, read_zip_entry};
use crate::converter::ntb::{load_ntb_document, load_ntb_export};
use plist::Value;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use zip::ZipArchive;

const SAMPLE_NTB: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/input/NTB Note Apr 26, 2026.ntb"
);
/// The same note as Notability saved it as a `.note`.
const SAMPLE_NOTE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/input/NOTE Note Apr 26, 2026.note"
);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "notability-note-writer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Page, tool, color, dash and a position that survives Notability's two ways of
/// writing an ellipse: the center of its four anchors, else the first point.
fn curve_key(curve: &StrokeCurve) -> (usize, u8, [u8; 4], Option<u8>, i64, i64) {
    let (x, y) = if curve.points.len() == 13 && !curve.preserve_vertices {
        let anchors = [0, 3, 6, 9].map(|index| curve.points[index]);
        (
            anchors.iter().map(|p| p.0).sum::<f32>() / 4.0,
            anchors.iter().map(|p| p.1).sum::<f32>() / 4.0,
        )
    } else {
        curve.points[0]
    };
    (
        curve.page_index,
        curve.style,
        curve.rgba,
        curve.dash_pattern,
        (x * 20.0).round() as i64,
        (y * 20.0).round() as i64,
    )
}

fn assert_same_content(ntb: &NoteDocument, note: &NoteDocument) {
    assert_eq!(note.page_width_doc, ntb.page_width_doc);
    assert!((note.page_height_doc - ntb.page_height_doc).abs() < 0.01);
    // A .note copies strokes onto every page they reach; a bundle places each once.
    let mut available: BTreeMap<_, usize> = BTreeMap::new();
    for curve in &note.curves {
        *available.entry(curve_key(curve)).or_default() += 1;
    }
    for curve in &ntb.curves {
        let key = curve_key(curve);
        let count = available
            .get_mut(&key)
            .filter(|count| **count > 0)
            .unwrap_or_else(|| panic!("no .note curve matches {key:?}"));
        *count -= 1;
    }

    // Bundles order media by a sort key, notes by a z-index from 0 on each page:
    // both must stack the same images and stickies.
    let stacked = |document: &NoteDocument| {
        let mut media: Vec<(usize, i64, [f32; 4])> = document
            .media_images
            .iter()
            .map(|image| {
                (
                    image.page_index,
                    image.z_index,
                    [image.x, image.y, image.width, image.height],
                )
            })
            .chain(document.sticky_notes.iter().map(|sticky| {
                (
                    sticky.page_index,
                    sticky.z_index,
                    [sticky.x, sticky.y, sticky.width, sticky.height],
                )
            }))
            .collect();
        media.sort_by_key(|(page, z, _)| (*page, *z));
        media
            .into_iter()
            .map(|(page, _, frame)| (page, frame.map(|v| (v * 100.0).round() as i64)))
            .collect::<Vec<_>>()
    };
    assert_eq!(stacked(note), stacked(ntb));
    assert_eq!(note.media_images.len(), ntb.media_images.len());
    for a in &ntb.media_images {
        let b = note
            .media_images
            .iter()
            .find(|b| (a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01)
            .expect("image position");
        assert!((a.rotation_degrees - b.rotation_degrees).abs() < 0.01);
        // A .note always stores a crop rectangle; a bundle only a real crop.
        if let (Some(a), Some(b)) = (a.crop, b.crop) {
            let a = [a.x, a.y, a.width, a.height];
            let b = [b.x, b.y, b.width, b.height];
            assert!(
                a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01),
                "{a:?} {b:?}"
            );
        }
    }
    for (a, b) in ntb.sticky_notes.iter().zip(&note.sticky_notes) {
        assert_eq!(a.color, b.color);
    }

    assert_eq!(note.text, ntb.text);
    let styles = |document: &NoteDocument| -> Vec<_> {
        let mut styles = Vec::new();
        for span in &document.text_spans {
            for _ in 0..span.length {
                let style = &span.style;
                styles.push((
                    style.font_size.to_bits(),
                    style.color,
                    style.underline,
                    style.strikethrough,
                    style.indent_level,
                    style.indent_decoration_style,
                ));
            }
        }
        styles
    };
    assert_eq!(styles(note), styles(ntb));
}

#[test]
fn the_sample_bundle_reads_back_with_the_same_content() {
    let sample = Path::new(SAMPLE_NTB);
    if !sample.exists() {
        eprintln!("skipped: {SAMPLE_NTB} is not present");
        return;
    }
    let directory = TempDirectory::new();
    let output = directory.0.join("sample.note");
    let export = load_ntb_export(sample).unwrap();
    assert!(export.warnings.is_empty(), "{:?}", export.warnings);
    write_note_bundle(&export, &output).unwrap();
    let ntb = load_ntb_document(sample).unwrap();
    let note = load_note_document(&output).unwrap();
    assert_same_content(&ntb, &note);
    // Stroke counts as Notability writes them: 149 strokes and 15 erased shapes,
    // 73 shapes and 4 groups.
    let strokes = export
        .ink
        .iter()
        .filter(|object| matches!(object, InkObject::Stroke(_)))
        .count();
    let groups = export
        .ink
        .iter()
        .filter(|object| matches!(object, InkObject::Group(_)))
        .count();
    assert_eq!(
        (strokes, export.ink.len() - strokes - groups, groups),
        (164, 73, 4)
    );
}

#[test]
fn the_sample_text_is_styled_like_notabilitys_own_note() {
    let (sample, reference) = (Path::new(SAMPLE_NTB), Path::new(SAMPLE_NOTE));
    if !sample.exists() || !reference.exists() {
        return;
    }
    let directory = TempDirectory::new();
    let output = directory.0.join("sample.note");
    write_note_bundle(&load_ntb_export(sample).unwrap(), &output).unwrap();
    let converted = load_note_document(&output).unwrap();
    let reference = load_note_document(reference).unwrap();
    // Notability's copy ends with one more empty line.
    assert!(reference.text.starts_with(&converted.text));
    let styles = |document: &NoteDocument| -> Vec<_> {
        let mut styles = Vec::new();
        for span in &document.text_spans {
            for _ in 0..span.length {
                let style = &span.style;
                styles.push((
                    style.font_name.clone(),
                    style.font_size.to_bits(),
                    style.underline,
                    style.indent_level,
                    style.indent_decoration_style,
                    style.indent_decoration_number,
                    style.line_spacing_multiplier.to_bits(),
                ));
            }
        }
        styles
    };
    let count = converted.text.chars().count();
    assert_eq!(styles(&converted), styles(&reference)[..count]);
}

#[test]
fn conversion_is_deterministic() {
    let sample = Path::new(SAMPLE_NTB);
    if !sample.exists() {
        return;
    }
    let directory = TempDirectory::new();
    let export = load_ntb_export(sample).unwrap();
    let [first, second] = ["a.note", "b.note"].map(|name| {
        let path = directory.0.join(name);
        write_note_bundle(&export, &path).unwrap();
        fs::read(path).unwrap()
    });
    assert!(first == second);
}

fn minimal_note() -> NoteExport {
    let curve = Curve {
        points: vec![(10.0, 900.0), (20.0, 900.0), (30.0, 905.0), (40.0, 910.0)],
        nodes: vec![Node::UPRIGHT; 2],
    };
    let erased = Curve {
        points: vec![(50.0, 910.0)],
        nodes: vec![Node {
            force: 0.5,
            ..Node::UPRIGHT
        }],
    };
    NoteExport {
        title: "A/B: test".to_owned(),
        language: "en_US".to_owned(),
        font_family: "Helvetica Neue".to_owned(),
        font_size: 12.0,
        created_ms: Some(1_777_193_637_206),
        modified_ms: Some(1_788_159_111_887),
        page_width: 679.0,
        page_height: 891.187_5,
        paper: Paper {
            identifier: "Legacy:13".to_owned(),
            lines: None,
        },
        pdfs: Vec::new(),
        pdf_pages: Vec::new(),
        ink: vec![InkObject::Stroke(Stroke {
            curves: vec![curve, erased],
            width: 1.5,
            rgba: [237, 54, 36, 255],
            style: 5,
            dash: Some(2),
        })],
        media: Vec::new(),
        text: RichText::default(),
        thumbnail: None,
        seed: 7,
        warnings: Vec::new(),
    }
}

#[test]
fn writes_the_package_layout_notability_uses() {
    let directory = TempDirectory::new();
    let output = directory.0.join("out.note");
    write_note_bundle(&minimal_note(), &output).unwrap();
    let mut zip = ZipArchive::new(File::open(&output).unwrap()).unwrap();
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    for name in [
        "A-B- test/",
        "A-B- test/Session.plist",
        "A-B- test/metadata.plist",
        "A-B- test/thumb12x.png",
        "A-B- test/Recordings/library.plist",
        "A-B- test/Images/",
        "A-B- test/PDFs/",
    ] {
        assert!(names.iter().any(|entry| entry == name), "missing {name}");
    }
    for index in 0..zip.len() {
        assert_eq!(
            zip.by_index(index).unwrap().compression(),
            zip::CompressionMethod::Stored
        );
    }
    let thumbnail = read_zip_entry(&mut zip, "A-B- test/thumb12x.png").unwrap();
    let thumbnail = image::load_from_memory(&thumbnail).unwrap();
    assert_eq!((thumbnail.width(), thumbnail.height()), (576, 756));

    let metadata = read_zip_entry(&mut zip, "A-B- test/metadata.plist").unwrap();
    let metadata = KeyedArchive::new(Value::from_reader(Cursor::new(metadata)).unwrap()).unwrap();
    let info = metadata.root.as_dictionary().unwrap();
    assert_eq!(metadata.as_text(&info["noteName"]).unwrap(), "A/B: test");
    // The package path names the folder, which cannot hold the slash.
    assert_eq!(
        metadata.as_text(&info["notePackagePath"]).unwrap(),
        "A-B- test"
    );
    let uuid = metadata.as_text(&info["uuidKey"]).unwrap();
    assert_eq!(uuid.len(), 36);
    assert_eq!(&uuid[14..15], "4");
}

#[test]
fn counts_styles_and_dashes_per_stroke_and_arrays_per_curve_and_node() {
    let directory = TempDirectory::new();
    let output = directory.0.join("out.note");
    write_note_bundle(&minimal_note(), &output).unwrap();
    let mut zip = ZipArchive::new(File::open(&output).unwrap()).unwrap();
    let session = read_zip_entry(&mut zip, "A-B- test/Session.plist").unwrap();
    let archive = KeyedArchive::new(Value::from_reader(Cursor::new(session)).unwrap()).unwrap();
    let session = archive.root.as_dictionary().unwrap();
    let rich_text = archive.deref(&session["richText"]).as_dictionary().unwrap();
    let overlay = archive
        .deref(&rich_text["Handwriting Overlay"])
        .as_dictionary()
        .unwrap();
    let hash = archive
        .deref(&overlay["SpatialHash"])
        .as_dictionary()
        .unwrap();
    let data = |key: &str| archive.deref(&hash[key]).as_data().unwrap().len();
    let number = |key: &str| archive.deref(&hash[key]).as_signed_integer().unwrap();
    assert_eq!(
        (
            number("numcurves"),
            number("numpoints"),
            number("numfractionalwidths")
        ),
        (2, 5, 3)
    );
    assert_eq!((data("curvesnumpoints"), data("curvespoints")), (8, 40));
    assert_eq!(
        (data("curvesforces"), data("curvesazimuthunitvector")),
        (12, 24)
    );
    assert_eq!((data("curveUUIDs"), data("curvescolors")), (32, 8));
    assert_eq!((data("curvesstyles"), data("options")), (1, 8));
    let uuids = archive.deref(&hash["curveUUIDs"]).as_data().unwrap();
    assert_eq!(uuids[..16], uuids[16..]);
    let dashes = archive.deref(&hash["dashStyles"]).as_data().unwrap();
    let dashes = Value::from_reader(Cursor::new(dashes)).unwrap();
    let pattern = dashes.as_dictionary().unwrap()["objectPatterns"]
        .as_dictionary()
        .unwrap()["0"]
        .as_dictionary()
        .unwrap()["pattern"]
        .as_signed_integer();
    assert_eq!(pattern, Some(2));

    let note = load_note_document(&output).unwrap();
    assert!(note
        .curves
        .iter()
        .all(|curve| curve.style == 5 && curve.dash_pattern == Some(2)));
    assert!(note.curves.iter().all(|curve| curve.page_index == 1));
}

fn spatial_hash_value(output: &Path, key: &str) -> Value {
    let mut zip = ZipArchive::new(File::open(output).unwrap()).unwrap();
    let name = zip
        .file_names()
        .find(|name| name.ends_with("Session.plist"))
        .unwrap()
        .to_owned();
    let session = read_zip_entry(&mut zip, &name).unwrap();
    let archive = KeyedArchive::new(Value::from_reader(Cursor::new(session)).unwrap()).unwrap();
    let session = archive.root.as_dictionary().unwrap();
    let rich_text = archive.deref(&session["richText"]).as_dictionary().unwrap();
    let overlay = archive
        .deref(&rich_text["Handwriting Overlay"])
        .as_dictionary()
        .unwrap();
    let hash = archive
        .deref(&overlay["SpatialHash"])
        .as_dictionary()
        .unwrap();
    archive.deref(&hash[key]).clone()
}

#[test]
fn empty_strokes_take_no_z_order_position() {
    let mut note = minimal_note();
    let InkObject::Stroke(stroke) = note.ink.remove(0) else {
        unreachable!()
    };
    let triangle = Shape {
        geometry: ShapeGeometry::Polygon(vec![(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)]),
        width: 1.0,
        rgba: [0, 0, 0, 255],
        style: 3,
        dash: None,
        force: None,
    };
    note.ink = vec![
        InkObject::Stroke(Stroke {
            curves: Vec::new(),
            ..stroke.clone()
        }),
        InkObject::Shape(triangle),
        InkObject::Stroke(stroke),
    ];
    let directory = TempDirectory::new();
    let output = directory.0.join("out.note");
    write_note_bundle(&note, &output).unwrap();
    let shapes = spatial_hash_value(&output, "shapes");
    let shapes = Value::from_reader(Cursor::new(shapes.as_data().unwrap())).unwrap();
    let indices = shapes.as_dictionary().unwrap()["indices"]
        .as_array()
        .unwrap();
    assert_eq!(indices, &vec![Value::Integer(0.into())]);
    let kinds = shapes.as_dictionary().unwrap()["kinds"].as_array().unwrap();
    assert_eq!(kinds[0].as_string(), Some("triangle"));
    assert_eq!(
        spatial_hash_value(&output, "curvesstyles")
            .as_data()
            .map(<[u8]>::len),
        Some(1)
    );
}

#[test]
fn bounds_the_still_image_of_a_gif_with_a_huge_logical_size() {
    let mut gif = Vec::new();
    image::DynamicImage::new_rgba8(4, 2)
        .write_to(&mut Cursor::new(&mut gif), image::ImageFormat::Gif)
        .unwrap();
    let mut note = minimal_note();
    note.media.push(super::Media::Image {
        frame: super::MediaFrame {
            origin: (10.0, 10.0),
            size: (100.0, 50.0),
            rotation: 0.0,
            z_index: 0,
        },
        image: super::ImageData {
            bytes: gif,
            kind: super::ImageKind::Gif,
            logical_size: (40_000.0, 20_000.0),
            crop: None,
        },
        locked: false,
    });
    let directory = TempDirectory::new();
    let output = directory.0.join("out.note");
    write_note_bundle(&note, &output).unwrap();
    let mut zip = ZipArchive::new(File::open(&output).unwrap()).unwrap();
    let still = read_zip_entry(&mut zip, "A-B- test/Images/Image .png").unwrap();
    let still = image::load_from_memory(&still).unwrap();
    assert_eq!((still.width(), still.height()), (4096, 2048));
    assert!(zip.file_names().any(|name| name.ends_with("/media.gif")));
}
