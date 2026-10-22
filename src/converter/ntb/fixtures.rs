//! Small FlatBuffers/ZIP fixtures for capabilities absent from the example note.
use super::{load_ntb_document, load_ntb_export, FlatBuffer};
use crate::converter::{
    model::OutputFormat,
    note::load_note_document,
    note_writer::{write_note_bundle, InkObject, ShapeGeometry},
    render::write_note_output,
};
use lopdf::{
    content::{Content, Operation},
    dictionary, Dictionary, Document, Stream,
};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use zip::{write::SimpleFileOptions, ZipWriter};

struct Buffer(Vec<u8>);

impl Buffer {
    fn table(&mut self, slots: usize, fields: &[(usize, Vec<u8>)]) -> usize {
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        let vtable = self.0.len();
        self.0.extend(vec![0; 4 + slots * 2]);
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        let table = self.0.len();
        self.0.extend(((table - vtable) as i32).to_le_bytes());
        for (slot, bytes) in fields {
            while self.0.len() % 4 != 0 {
                self.0.push(0);
            }
            let offset = (self.0.len() - table) as u16;
            self.0[vtable + 4 + slot * 2..vtable + 6 + slot * 2]
                .copy_from_slice(&offset.to_le_bytes());
            self.0.extend(bytes);
        }
        let size = (self.0.len() - table) as u16;
        self.0[vtable..vtable + 2].copy_from_slice(&((4 + slots * 2) as u16).to_le_bytes());
        self.0[vtable + 2..vtable + 4].copy_from_slice(&size.to_le_bytes());
        table
    }

    fn link(&mut self, table: usize, slot: usize, target: usize) {
        let field = FlatBuffer::new(&self.0).field(table, slot).unwrap();
        assert!(target >= field);
        self.0[field..field + 4].copy_from_slice(&((target - field) as u32).to_le_bytes());
    }

    fn vector(&mut self, length: usize, items: &[u8]) -> usize {
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        let offset = self.0.len();
        self.0.extend((length as u32).to_le_bytes());
        self.0.extend(items);
        offset
    }

    fn string(&mut self, text: &str) -> usize {
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        let offset = self.0.len();
        self.0.extend((text.len() as u32).to_le_bytes());
        self.0.extend(text.as_bytes());
        self.0.push(0);
        offset
    }
}

fn id(index: u32) -> Vec<u8> {
    [7u32, index]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}

fn position(index: u32, offset: u32) -> Vec<u8> {
    [7u32, index, offset]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}

fn pair(x: f32, y: f32) -> Vec<u8> {
    [x, y].into_iter().flat_map(f32::to_le_bytes).collect()
}

fn fixture() -> Vec<u8> {
    // Nonsequential ids, interleaved stores, and styles before their text.
    let kinds = [
        (0, 1),
        (10, 8),
        (60, 8),
        (300, 12),
        (100, 22),
        (200, 22),
        (50, 8),
        (301, 12),
        (11, 8),
        (99, 7),
    ];
    let mut b = Buffer(vec![0; 4]);
    let root = b.table(7, &[(6, vec![0; 4])]);
    let vector = b.0.len();
    b.0.extend((kinds.len() as u32).to_le_bytes());
    b.0.extend(vec![0; kinds.len() * 4]);
    b.link(root, 6, vector);
    b.0[..4].copy_from_slice(&(root as u32).to_le_bytes());
    let mut wrappers = Vec::new();
    for (index, kind) in kinds {
        wrappers.push(b.table(6, &[(0, id(index)), (4, vec![kind]), (5, vec![0; 4])]));
    }
    for (i, wrapper) in wrappers.iter().enumerate() {
        let slot = vector + 4 + i * 4;
        b.0[slot..slot + 4].copy_from_slice(&((*wrapper - slot) as u32).to_le_bytes());
    }
    let mut payloads = Vec::new();
    for (index, kind) in kinds {
        let fields = match kind {
            8 => vec![
                (
                    0,
                    id(match index {
                        50 => 100,
                        60 => 200,
                        _ => 0,
                    }),
                ),
                (1, vec![0; 4]),
            ],
            22 => vec![
                (2, position(1, if index == 100 { 3 } else { 1 })),
                (3, pair(if index == 100 { 40.0 } else { 220.0 }, 70.0)),
                (6, pair(200.0, 100.0)),
            ],
            12 => vec![
                (
                    0,
                    position(
                        if index == 300 { 50 } else { 60 },
                        if index == 300 { 3 } else { 0 },
                    ),
                ),
                (
                    1,
                    position(
                        if index == 300 { 50 } else { 60 },
                        if index == 300 { 8 } else { 1 },
                    ),
                ),
                (9, vec![0; 4]),
            ],
            7 => vec![(0, position(11, 4))],
            _ => vec![],
        };
        payloads.push(b.table(
            if kind == 22 {
                16
            } else if kind == 12 {
                14
            } else {
                3
            },
            &fields,
        ));
    }
    for (wrapper, payload) in wrappers.iter().zip(&payloads) {
        b.link(*wrapper, 5, *payload);
    }
    for ((index, kind), payload) in kinds.into_iter().zip(payloads) {
        if kind == 8 {
            let text = match index {
                10 => "Main ",
                11 => "body",
                50 => "A😀green",
                60 => "B red",
                _ => unreachable!(),
            };
            let string = b.string(text);
            b.link(payload, 1, string);
        } else if kind == 12 {
            let color = b.table(
                1,
                &[(
                    0,
                    if index == 300 {
                        vec![10, 200, 30, 255]
                    } else {
                        vec![200, 10, 30, 255]
                    },
                )],
            );
            b.link(payload, 9, color);
        }
    }
    b.0
}

/// A bundle of `(id, kind)` events whose payloads `payload` writes.
fn events_bundle(
    events: &[(u32, u8)],
    mut payload: impl FnMut(&mut Buffer, u32) -> usize,
) -> Vec<u8> {
    let mut b = Buffer(vec![0; 4]);
    let root = b.table(7, &[(6, vec![0; 4])]);
    let vector = b.0.len();
    b.0.extend((events.len() as u32).to_le_bytes());
    b.0.extend(vec![0; events.len() * 4]);
    b.link(root, 6, vector);
    b.0[..4].copy_from_slice(&(root as u32).to_le_bytes());
    let wrappers: Vec<usize> = events
        .iter()
        .map(|(index, kind)| b.table(6, &[(0, id(*index)), (4, vec![*kind]), (5, vec![0; 4])]))
        .collect();
    for (i, wrapper) in wrappers.iter().enumerate() {
        let slot = vector + 4 + i * 4;
        b.0[slot..slot + 4].copy_from_slice(&((*wrapper - slot) as u32).to_le_bytes());
    }
    for ((index, _), wrapper) in events.iter().zip(wrappers) {
        let table = payload(&mut b, *index);
        b.link(wrapper, 5, table);
    }
    b.0
}

fn id_list(b: &mut Buffer, table: usize, ids: &[u32]) {
    let items: Vec<u8> = ids.iter().flat_map(|index| id(*index)).collect();
    let vector = b.vector(ids.len(), &items);
    b.link(table, 0, vector);
}

/// Ink on page 1, a grouped triangle, a second stroke an erase record removed, and
/// the group of the first stroke and the triangle.
fn ink_fixture() -> Vec<u8> {
    events_bundle(
        &[(0, 1), (10, 15), (11, 15), (12, 25), (13, 18), (14, 20)],
        |b, index| match index {
            10 | 11 => {
                let ink = b.table(
                    10,
                    &[
                        (0, position(1, 1)),
                        (1, pair(100.0, 50.0)),
                        (7, vec![237, 54, 36, 255]),
                        (8, 1.5f32.to_le_bytes().to_vec()),
                        (9, vec![0; 4]),
                    ],
                );
                // Two nodes (width 0.5, force 1) around one cubic segment.
                let mut path = vec![0, 2, 0, 3, 0, 0, 0, 0, 0x00, 0x38, 0x00, 0x3c, 255, 0, 0];
                for value in [0x3c00u16, 0, 0x4000, 0, 0x4200, 0] {
                    path.extend(value.to_le_bytes());
                }
                path.extend([0x00, 0x38, 0x00, 0x3c, 255, 0]);
                let vector = b.vector(path.len(), &path);
                b.link(ink, 9, vector);
                ink
            }
            12 => {
                let erase = b.table(1, &[(0, vec![0; 4])]);
                id_list(b, erase, &[11]);
                erase
            }
            13 => {
                let shape = b.table(
                    11,
                    &[
                        (0, position(1, 1)),
                        (1, pair(200.0, 60.0)),
                        (4, vec![2]),
                        (5, vec![0; 4]),
                        (9, vec![0, 0, 0, 255]),
                        (10, 2.0f32.to_le_bytes().to_vec()),
                    ],
                );
                let points = b.table(1, &[(0, vec![0; 4])]);
                b.link(shape, 5, points);
                let corners: Vec<u8> = [(0.0f32, 0.0f32), (30.0, 0.0), (0.0, 40.0)]
                    .into_iter()
                    .flat_map(|(x, y)| pair(x, y))
                    .collect();
                let vector = b.vector(3, &corners);
                b.link(points, 0, vector);
                shape
            }
            14 => {
                let group = b.table(1, &[(0, vec![0; 4])]);
                id_list(b, group, &[10, 13]);
                group
            }
            _ => b.table(1, &[]),
        },
    )
}

struct Bundle {
    directory: PathBuf,
    path: PathBuf,
}

impl Bundle {
    fn new(bytes: &[u8], assets: &[(&str, Vec<u8>)]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "notability-fixture-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("fixture.ntb");
        let mut zip = ZipWriter::new(fs::File::create(&path).unwrap());
        zip.start_file("noteBundle", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
        for (name, data) in assets {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        Self { directory, path }
    }
}

impl Drop for Bundle {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn background_pdf(pages: usize) -> Vec<u8> {
    let mut pdf = Document::with_version("1.5");
    let tree = pdf.new_object_id();
    let mut kids = Vec::new();
    for _ in 0..pages {
        let content = Content {
            operations: vec![
                Operation::new("rg", vec![0.2.into(), 0.4.into(), 0.6.into()]),
                Operation::new("re", vec![0.into(), 0.into(), 100.into(), 200.into()]),
                Operation::new("f", vec![]),
            ],
        };
        let content = pdf.add_object(Stream::new(Dictionary::new(), content.encode().unwrap()));
        kids.push(
            pdf.add_object(
                dictionary! { "Type" => "Page", "Parent" => tree, "Contents" => content },
            ),
        );
    }
    pdf.objects.insert(tree, dictionary! { "Type" => "Pages", "Count" => pages as i64,
        "Kids" => kids.into_iter().map(Into::into).collect::<Vec<lopdf::Object>>(),
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 200.into()], "Resources" => Dictionary::new(),
    }.into());
    let root = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => tree });
    pdf.trailer.set("Root", root);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).unwrap();
    bytes
}

#[test]
fn links_interleaved_text_boxes_and_utf16_styles_by_event_id() {
    let bundle = Bundle::new(&fixture(), &[]);
    let note = load_ntb_document(&bundle.path).unwrap();
    assert_eq!(note.text, "Main body");
    assert_eq!(note.text_blocks.len(), 2);
    let a = note.text_blocks.iter().find(|b| b.page_index == 3).unwrap();
    let b = note.text_blocks.iter().find(|b| b.page_index == 1).unwrap();
    assert_eq!(a.text, "A😀green");
    assert_eq!(b.text, "B red");
    let green = a
        .text_spans
        .iter()
        .find(|s| s.style.color == [10, 200, 30, 255])
        .unwrap();
    assert_eq!((green.start, green.length), (2, 5));
    assert_eq!(b.text_spans[0].style.color, [200, 10, 30, 255]);
    assert_eq!(note.text_spans[0].style.color, [0, 0, 0, 255]);
    assert!((a.x + note.page_width_doc / 38.4 - 40.0).abs() < 0.001);
}

#[test]
fn renders_annotations_on_their_own_pages_and_preserves_blank_pdf_pages() {
    let bundle = Bundle::new(&fixture(), &[("assets/background.pdf", background_pdf(5))]);
    let note = load_ntb_document(&bundle.path).unwrap();
    assert_eq!(note.pdf_pages.len(), 5);
    let output = bundle.directory.join("out.pdf");
    write_note_output(&note, &bundle.path, &output, OutputFormat::Pdf).unwrap();
    let pdf = Document::load(&output).unwrap();
    assert_eq!(pdf.get_pages().len(), 5);
    for (index, page) in pdf.get_pages().values().enumerate() {
        let content = Content::decode(&pdf.get_page_content(*page).unwrap()).unwrap();
        assert!(content
            .operations
            .iter()
            .any(|op| op.operator == "Do" && op.operands[0].as_name().unwrap() == b"Base"));
        let text: Vec<u8> = content
            .operations
            .iter()
            .filter(|op| op.operator == "Tj")
            .flat_map(|op| op.operands[0].as_str().unwrap().to_vec())
            .collect();
        match index {
            0 => assert_eq!(text, b"Main body"),
            1 => assert_eq!(text, b"B red"),
            3 => assert_eq!(String::from_utf8(text).unwrap(), "A😀green"),
            _ => assert!(text.is_empty()),
        }
    }
}

#[test]
fn renders_text_box_svg_with_page_local_coordinates() {
    let bundle = Bundle::new(&fixture(), &[]);
    let note = load_ntb_document(&bundle.path).unwrap();
    let output = bundle.directory.join("out.svg");
    write_note_output(&note, &bundle.path, &output, OutputFormat::Svg).unwrap();
    let svg = fs::read_to_string(output).unwrap();
    let page = svg.split("id=\"page-4\"").nth(1).unwrap();
    assert!(page.contains(">A</text>"));
    assert!(page.contains("x=\"40.00\""));
    assert!(page.contains("text-clip-3-0"));
}

#[test]
fn preserves_main_text_pages_when_there_is_no_ink_or_media() {
    let bundle = Bundle::new(&fixture(), &[]);
    let mut note = load_ntb_document(&bundle.path).unwrap();
    note.text_blocks.clear();
    note.text = "Line\n".repeat(100);
    let output = bundle.directory.join("out.pdf");
    write_note_output(&note, &bundle.path, &output, OutputFormat::Pdf).unwrap();
    let pdf = Document::load(output).unwrap();
    assert_eq!(pdf.get_pages().len(), 3);
    let text: Vec<u8> = pdf
        .get_pages()
        .values()
        .flat_map(|page| {
            Content::decode(&pdf.get_page_content(*page).unwrap())
                .unwrap()
                .operations
                .into_iter()
                .filter(|op| op.operator == "Tj")
                .flat_map(|op| op.operands[0].as_str().unwrap().to_vec())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(text, "Line".repeat(100).as_bytes());
}

#[test]
fn crossing_stroke_is_clipped_consistently_in_pdf_and_svg() {
    use crate::converter::{geometry::stroke_pages, model::StrokeCurve};
    let bundle = Bundle::new(&fixture(), &[]);
    let mut note = load_ntb_document(&bundle.path).unwrap();
    note.text.clear();
    note.text_blocks.clear();
    note.line_style = None;
    note.page_width_doc = 100.0;
    note.export_width_pt = 100.0;
    note.page_height_doc = 100.0;
    note.export_height_pt = 100.0;
    let inset = note.page_width_doc / 38.4;
    note.curves = stroke_pages(
        StrokeCurve {
            page_index: 0,
            points: vec![(50.0 - inset, 90.0), (50.0 - inset, 110.0)],
            preserve_vertices: true,
            path_commands: Vec::new(),
            fill_path: false,
            width: 1.0,
            rgba: [0, 0, 255, 255],
            style: 3,
            dash_pattern: None,
            pressures: vec![1.0, 1.0],
            fractional_widths: vec![1.0, 1.0],
            opacities: Vec::new(),
        },
        100.0,
    );
    let output = bundle.directory.join("out.pdf");
    write_note_output(&note, &bundle.path, &output, OutputFormat::Pdf).unwrap();
    let pdf = Document::load(output).unwrap();
    assert_eq!(pdf.get_pages().len(), 2);
    for (index, page) in pdf.get_pages().values().enumerate() {
        let ops = Content::decode(&pdf.get_page_content(*page).unwrap())
            .unwrap()
            .operations;
        let start = ops.iter().find(|op| op.operator == "m").unwrap();
        let end = ops.iter().find(|op| op.operator == "l").unwrap();
        assert_eq!(
            start.operands[1].as_float().unwrap(),
            if index == 0 { 10.0 } else { 110.0 }
        );
        assert_eq!(
            end.operands[1].as_float().unwrap(),
            if index == 0 { -10.0 } else { 90.0 }
        );
    }
    let output = bundle.directory.join("out.svg");
    write_note_output(&note, &bundle.path, &output, OutputFormat::Svg).unwrap();
    let svg = fs::read_to_string(output).unwrap();
    assert!(svg.contains("clip-path=\"url(#page-clip-1)\""));
    assert!(svg.contains("clip-path=\"url(#page-clip-2)\""));
    assert!(svg.contains("M 50.00 90.00 L 50.00 110.00"));
    assert!(svg.contains("M 50.00 -10.00 L 50.00 10.00"));
}

#[test]
fn refuses_to_guess_the_order_of_multiple_pdf_assets() {
    let bundle = Bundle::new(
        &fixture(),
        &[
            ("assets/a.pdf", background_pdf(1)),
            ("assets/b.pdf", background_pdf(1)),
        ],
    );
    let error = load_ntb_document(&bundle.path).unwrap_err().to_string();
    assert!(error.contains("multiple PDF assets"));
}

#[test]
fn draws_annotation_text_above_a_sticky_background_with_the_same_z_index() {
    use crate::converter::model::StickyNote;
    let bundle = Bundle::new(&fixture(), &[]);
    let mut note = load_ntb_document(&bundle.path).unwrap();
    let block = note.text_blocks.iter().find(|b| b.page_index == 1).unwrap();
    note.sticky_notes.push(StickyNote {
        page_index: block.page_index,
        x: block.x,
        y: block.y,
        width: block.width,
        height: block.height,
        rotation_degrees: 0.0,
        color: [255, 244, 142, 255],
        line_style: None,
        line_start_multiple: 2.0,
        z_index: block.z_index,
    });
    let output = bundle.directory.join("out.pdf");
    write_note_output(&note, &bundle.path, &output, OutputFormat::Pdf).unwrap();
    let pdf = Document::load(output).unwrap();
    let page = *pdf.get_pages().get(&2).unwrap();
    let operations = Content::decode(&pdf.get_page_content(page).unwrap())
        .unwrap()
        .operations;
    let background = operations
        .iter()
        .rposition(|op| op.operator == "f")
        .unwrap();
    let text = operations
        .iter()
        .position(|op| op.operator == "Tj")
        .unwrap();
    assert!(background < text);
    let output = bundle.directory.join("out.svg");
    write_note_output(&note, &bundle.path, &output, OutputFormat::Svg).unwrap();
    let svg = fs::read_to_string(output).unwrap();
    let page = svg.split("id=\"page-2\"").nth(1).unwrap();
    assert!(page.find("fill=\"rgb(255,244,142)\"").unwrap() < page.find(">B</text>").unwrap());
}

#[test]
#[ignore = "requires Poppler's pdftoppm"]
fn embeds_pdf_backgrounds_in_svg_without_external_asset_links() {
    let bundle = Bundle::new(&fixture(), &[("assets/background.pdf", background_pdf(5))]);
    let note = load_ntb_document(&bundle.path).unwrap();
    let output = bundle.directory.join("out.svg");
    write_note_output(&note, &bundle.path, &output, OutputFormat::Svg).unwrap();
    let svg = fs::read_to_string(output).unwrap();
    assert_eq!(svg.matches("data:image/png;base64,iVBOR").count(), 5);
    assert!(svg.contains("text-clip-3-0"));
}

#[test]
fn note_export_skips_erased_records_and_keeps_groups() {
    let bundle = Bundle::new(&ink_fixture(), &[]);
    let export = load_ntb_export(&bundle.path).unwrap();
    let [InkObject::Group(members)] = export.ink.as_slice() else {
        panic!("expected one group, got {:?}", export.ink);
    };
    let [InkObject::Stroke(stroke), InkObject::Shape(shape)] = members.as_slice() else {
        panic!("unexpected group members {members:?}");
    };
    let inset = 679.0 / 38.4;
    let page_top = 891.187_56;
    let first = stroke.curves[0].points[0];
    assert!((first.0 - (100.0 - inset)).abs() < 1e-3 && (first.1 - (page_top + 50.0)).abs() < 1e-3);
    assert_eq!(stroke.curves[0].nodes.len(), 2);
    assert_eq!(stroke.curves[0].nodes[0].fractional_width, 0.5);
    let ShapeGeometry::Polygon(corners) = &shape.geometry else {
        panic!("expected a polygon");
    };
    assert_eq!(corners.len(), 3);

    let output = bundle.directory.join("out.note");
    write_note_bundle(&export, &output).unwrap();
    let note = load_note_document(&output).unwrap();
    assert_eq!(note.curves.len(), 2);
    let ink = note
        .curves
        .iter()
        .find(|curve| curve.style == 3 && curve.rgba[0] == 237)
        .unwrap();
    assert_eq!(ink.page_index, 1);
    assert!((ink.points[3].0 - (103.0 - inset as f32)).abs() < 1e-3);
    assert!((ink.points[3].1 - 50.0).abs() < 1e-3);
    let triangle = note
        .curves
        .iter()
        .find(|curve| curve.rgba == [0, 0, 0, 255])
        .unwrap();
    assert_eq!(triangle.points.len(), 4);
    assert!((triangle.points[2].1 - 100.0).abs() < 1e-3);
}

#[test]
fn note_export_places_text_boxes_on_pdf_page_slots() {
    let bundle = Bundle::new(&fixture(), &[("assets/background.pdf", background_pdf(5))]);
    let export = load_ntb_export(&bundle.path).unwrap();
    assert_eq!(export.pdf_pages.len(), 5);
    let output = bundle.directory.join("out.note");
    write_note_bundle(&export, &output).unwrap();
    let note = load_note_document(&output).unwrap();
    let ntb = load_ntb_document(&bundle.path).unwrap();
    // A 100 x 200 pt PDF page is 1358 units of the 679 wide strip.
    assert_eq!(note.page_height_doc, 1358.0);
    assert_eq!(note.pdf_pages.len(), 5);
    assert!(note
        .pdf_pages
        .iter()
        .enumerate()
        .all(|(index, page)| page.page_index == index && page.source_page_index == index));
    assert_eq!(note.text, "Main body");
    assert_eq!(note.text_blocks.len(), ntb.text_blocks.len());
    // Text boxes are not read back as sticky notes.
    assert!(note.sticky_notes.is_empty());
    for (a, b) in ntb.text_blocks.iter().zip(&note.text_blocks) {
        assert_eq!((a.page_index, &a.text), (b.page_index, &b.text));
        assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3);
        let colors = |block: &crate::converter::model::TextBlock| {
            block
                .text_spans
                .iter()
                .map(|span| (span.start, span.length, span.style.color))
                .collect::<Vec<_>>()
        };
        assert_eq!(colors(a), colors(b));
    }
    let mut zip = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
    let pdfs: Vec<String> = zip
        .file_names()
        .filter(|name| name.contains("/PDFs/") && name.ends_with(".pdf"))
        .map(str::to_owned)
        .collect();
    assert_eq!(pdfs.len(), 1);
    let pdf = crate::converter::note::read_zip_entry(&mut zip, &pdfs[0]).unwrap();
    assert_eq!(pdf, background_pdf(5));
}

#[test]
fn an_empty_bundle_converts_to_an_empty_note() {
    let bundle = Bundle::new(&events_bundle(&[(0, 1)], |b, _| b.table(1, &[])), &[]);
    let export = load_ntb_export(&bundle.path).unwrap();
    assert!(export.ink.is_empty() && export.media.is_empty() && export.text.text.is_empty());
    assert_eq!(export.title, "fixture");
    let output = bundle.directory.join("out.note");
    write_note_bundle(&export, &output).unwrap();
    let note = load_note_document(&output).unwrap();
    assert!(note.curves.is_empty() && note.text.is_empty());
    assert_eq!(note.page_width_doc, 679.0);
}

fn media_payload(
    b: &mut Buffer,
    position_xy: (f32, f32),
    size: (f32, f32),
    sort_key: u64,
) -> usize {
    b.table(
        16,
        &[
            (2, position(1, 1)),
            (3, pair(position_xy.0, position_xy.1)),
            (6, pair(size.0, size.1)),
            (9, sort_key.to_le_bytes().to_vec()),
        ],
    )
}

#[test]
fn text_boxes_follow_their_own_events_and_erased_boxes_take_their_text() {
    use crate::converter::note_writer::Media;
    // Two text boxes and a sticky note share one sort key; the first box is erased.
    let bytes = events_bundle(
        &[
            (0, 1),
            (100, 22),
            (101, 22),
            (102, 22),
            (110, 8),
            (111, 8),
            (120, 25),
        ],
        |b, index| match index {
            100 => media_payload(b, (40.0, 70.0), (200.0, 100.0), 5),
            101 => media_payload(b, (300.0, 400.0), (100.0, 50.0), 5),
            102 => {
                let sticky = b.table(
                    16,
                    &[
                        (2, position(1, 1)),
                        (3, pair(10.0, 10.0)),
                        (6, pair(50.0, 50.0)),
                        (9, 5u64.to_le_bytes().to_vec()),
                        (15, vec![0; 4]),
                    ],
                );
                let paper = b.table(5, &[(4, vec![255, 244, 142, 255])]);
                b.link(sticky, 15, paper);
                sticky
            }
            110 | 111 => {
                let text = b.table(2, &[(0, id(index - 10)), (1, vec![0; 4])]);
                let string = b.string(if index == 110 {
                    "deleted box"
                } else {
                    "live box"
                });
                b.link(text, 1, string);
                text
            }
            120 => {
                let erase = b.table(1, &[(0, vec![0; 4])]);
                id_list(b, erase, &[100]);
                erase
            }
            _ => b.table(1, &[]),
        },
    );
    let bundle = Bundle::new(&bytes, &[]);
    let export = load_ntb_export(&bundle.path).unwrap();
    let [Media::TextBox { frame, text }, Media::Sticky { .. }] = export.media.as_slice() else {
        panic!(
            "expected the live text box and the sticky note: {:?}",
            export.media
        );
    };
    assert_eq!(text.text, "live box");
    assert!((frame.origin.0 - (300.0 - 679.0 / 38.4)).abs() < 1e-3);
}

#[test]
fn a_record_stored_again_keeps_its_place_with_its_latest_content() {
    let mut versions = 0;
    let bytes = events_bundle(&[(0, 1), (10, 15), (11, 15), (10, 15)], |b, index| {
        if index == 0 {
            return b.table(1, &[]);
        }
        let x = match index {
            10 => {
                versions += 1;
                if versions == 1 {
                    100.0
                } else {
                    300.0
                }
            }
            _ => 200.0,
        };
        let ink = b.table(
            10,
            &[
                (0, position(1, 0)),
                (1, pair(x, 50.0)),
                (8, 1.0f32.to_le_bytes().to_vec()),
                (9, vec![0; 4]),
            ],
        );
        // A single dot.
        let path = [0u8, 1, 0, 3, 0, 0, 0, 0, 0x00, 0x3c, 0x00, 0x3c, 255, 0];
        let vector = b.vector(path.len(), &path);
        b.link(ink, 9, vector);
        ink
    });
    let bundle = Bundle::new(&bytes, &[]);
    let export = load_ntb_export(&bundle.path).unwrap();
    let starts: Vec<f32> = export
        .ink
        .iter()
        .map(|object| match object {
            InkObject::Stroke(stroke) => stroke.curves[0].points[0].0 + 679.0 / 38.4,
            other => panic!("expected strokes, got {other:?}"),
        })
        .collect();
    assert_eq!(starts.len(), 2);
    assert!((starts[0] - 300.0).abs() < 1e-3 && (starts[1] - 200.0).abs() < 1e-3);
}

#[test]
fn reports_media_it_cannot_convert() {
    let bytes = events_bundle(&[(0, 1), (30, 22)], |b, index| {
        if index == 0 {
            return b.table(1, &[]);
        }
        let media = b.table(
            16,
            &[
                (2, position(1, 0)),
                (3, pair(10.0, 10.0)),
                (6, pair(50.0, 50.0)),
                (10, vec![0; 4]),
            ],
        );
        let table = b.table(2, &[(0, vec![0; 4]), (1, pair(640.0, 480.0))]);
        b.link(media, 10, table);
        let asset = b.table(3, &[(0, vec![0xab; 64]), (2, vec![0; 4])]);
        b.link(table, 0, asset);
        let mime = b.string("video/mp4");
        b.link(asset, 2, mime);
        media
    });
    let bundle = Bundle::new(&bytes, &[]);
    let export = load_ntb_export(&bundle.path).unwrap();
    assert!(export.media.is_empty());
    assert_eq!(
        export.warnings,
        ["1 unsupported media object(s) (video/mp4) were left out"]
    );
}
