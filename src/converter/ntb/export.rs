//! Rebuild the `.note` object model from a native `.ntb` event collection.
//!
//! The renderer's `NoteDocument` flattens strokes into page-local paths. A `.note`
//! keeps more: the pieces an eraser left of each stroke, pressure and tilt per node,
//! editable shapes, ink groups and the z-order across all of them. This module reads
//! those straight from the events and places everything in `.note` document
//! coordinates: x less the content inset, y down a continuous strip of pages.
use super::documents::{self, event_id, event_type, EventId};
use super::{
    default_text_style, hex, page_index, read_half, rotate_points, tool_style, unrotate_top_left,
    FlatBuffer, EVENT_DOCUMENT, EVENT_INK, EVENT_MEDIA, EVENT_SHAPE, EVENT_TEXT,
};
use crate::converter::model::{TextBlock, TextSpan, TextStyle, DEFAULT_CONTENT_INSET_RATIO};
use crate::converter::note::read_zip_entry;
use crate::converter::note_writer::{
    content_seed, Curve, ImageData, ImageKind, InkObject, LinePattern, Media, MediaFrame, Node,
    NoteExport, Paper, PaperLines, PdfPage, Point, RichText, Shape, ShapeGeometry, Stroke,
};
use crate::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::f32::consts::FRAC_PI_2;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use zip::ZipArchive;

const EVENT_GROUP: u8 = 20;
const EVENT_ERASE: u8 = 25;
const DEFAULT_PAGE_WIDTH: f32 = 679.0;
const DEFAULT_PAGE_HEIGHT: f32 = 891.187_56;
const DEFAULT_STICKY_COLOR: [u8; 4] = [255, 244, 142, 255];
const DEFAULT_LINE_SPACING_INCHES: f32 = 0.25;
/// Sticky note paper is laid out at a fixed device width: the sample's 5 mm lines
/// are 13.52 units apart in the bundle.
const STICKY_PAPER_WIDTH: f32 = 583.8;
/// Content on a page beyond this one is taken for a corrupt page number.
const MAX_PAGE_INDEX: usize = 10_000;

pub(crate) fn load_ntb_export(path: &Path) -> Result<NoteExport> {
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    read_ntb_export(ZipArchive::new(File::open(path)?)?, &name)
}

/// `name` titles the note when the bundle has no title of its own.
pub(crate) fn read_ntb_export<R: Read + Seek>(
    mut bundle: ZipArchive<R>,
    name: &str,
) -> Result<NoteExport> {
    let bytes = read_zip_entry(&mut bundle, "noteBundle")?;
    let flat = FlatBuffer::new(&bytes);
    let root = flat.root_table().ok_or("Invalid .ntb noteBundle root")?;
    let tables = flat
        .table_vector(root, 6)
        .ok_or("Invalid .ntb event collection")?;
    let records: Vec<Record> = tables
        .iter()
        .enumerate()
        .filter_map(|(index, &table)| Record::read(&flat, index, table))
        .collect();
    let live = live_records(&flat, &records);
    // The last document record wins.
    let document = live
        .iter()
        .filter(|record| record.kind == EVENT_DOCUMENT)
        .last()
        .map(|record| DocumentInfo::read(&flat, record.payload))
        .unwrap_or_default();
    let page_width = document.page_size.map_or(DEFAULT_PAGE_WIDTH, |size| size.0);
    let page_height = document
        .page_size
        .map_or(DEFAULT_PAGE_HEIGHT, |size| size.1);
    let inset = page_width * DEFAULT_CONTENT_INSET_RATIO;
    let mut warnings = Warnings::default();

    // A PDF page occupies ceil(W * h / w) units of the .note strip; its ink keeps the
    // page-local coordinates from the top of that slot.
    let mut pdfs = Vec::new();
    let mut pdf_pages = Vec::new();
    let mut slot_heights = Vec::new();
    let backgrounds = documents::pdf_backgrounds(&mut bundle)?;
    if let Some(first) = backgrounds.first() {
        let data = read_zip_entry(&mut bundle, &first.relative_path)?;
        let pdf = lopdf::Document::load_mem(&data)?;
        for page in &backgrounds {
            let (width, height) = crate::pdf::page_size(&pdf, page.source_page_index)?;
            slot_heights
                .push((f64::from(page_width) * f64::from(height) / f64::from(width)).ceil());
            pdf_pages.push(PdfPage {
                file: 0,
                source_page: page.source_page_index,
            });
        }
        pdfs.push(data);
    }
    let layout = PageLayout::new(inset, page_height, &slot_heights);

    let mut group_of = BTreeMap::new();
    let mut groups: Vec<Vec<InkObject>> = Vec::new();
    for record in live.iter().filter(|record| record.kind == EVENT_GROUP) {
        let members = id_vector(&flat, record.payload, 0).unwrap_or_default();
        for member in members {
            group_of.entry(member).or_insert(groups.len());
        }
        groups.push(Vec::new());
    }

    // Ink keeps the order of its events; a group takes the place of its first member.
    enum Slot {
        Object(InkObject),
        Group(usize),
    }
    let mut slots = Vec::new();
    let mut placed = vec![false; groups.len()];
    for record in &live {
        if !matches!(record.kind, EVENT_INK | EVENT_SHAPE) {
            continue;
        }
        if page_index(&flat, record.payload, 0) > MAX_PAGE_INDEX {
            warnings.far_pages += 1;
            continue;
        }
        let object = if record.kind == EVENT_INK {
            match read_stroke(&flat, record.payload, &layout) {
                Some(stroke) => InkObject::Stroke(stroke),
                None => {
                    warnings.strokes += 1;
                    continue;
                }
            }
        } else {
            match read_shape(&flat, record.payload, &layout) {
                Some(shape) => InkObject::Shape(shape),
                None => {
                    warnings.shapes += 1;
                    continue;
                }
            }
        };
        match record.id.and_then(|id| group_of.get(&id)) {
            Some(&group) => {
                groups[group].push(object);
                if !placed[group] {
                    placed[group] = true;
                    slots.push(Slot::Group(group));
                }
            }
            None => slots.push(Slot::Object(object)),
        }
    }
    let ink = slots
        .into_iter()
        .filter_map(|slot| match slot {
            Slot::Object(object) => Some(object),
            Slot::Group(group) => {
                let members = std::mem::take(&mut groups[group]);
                (!members.is_empty()).then_some(InkObject::Group(members))
            }
        })
        .collect();

    let mut default_style = default_text_style();
    if let Some(family) = &document.font_family {
        default_style.font_name = family.clone();
    }
    if let Some(size) = document.font_size {
        default_style.font_size = size;
    }
    let live_tables: Vec<usize> = live.iter().map(|record| record.table).collect();
    let (text, spans, boxes) =
        documents::reconstruct_text_boxes(&flat, &live_tables, default_style.clone(), inset)?;
    let text_boxes = boxes
        .into_iter()
        .map(|(index, block)| (live_tables[index], block))
        .collect();
    let media = read_media(
        &flat,
        live.iter()
            .copied()
            .filter(|record| record.kind == EVENT_MEDIA),
        &mut bundle,
        &layout,
        text_boxes,
        &mut warnings,
    )?;

    let title = document
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| name.to_owned());
    let paper = document
        .paper
        .map(|style| document_paper(&style, page_width))
        .unwrap_or_else(|| Paper {
            identifier: "Legacy:13".to_owned(),
            lines: None,
        });
    let created_ms = flat
        .field(root, 4)
        .and_then(|field| flat.u64(field))
        .and_then(|value| i64::try_from(value).ok());
    let modified_ms = records.iter().filter_map(|record| record.time_ms).max();
    let spans = number_list_paragraphs(&text, spans);
    Ok(NoteExport {
        title,
        language: document.language.unwrap_or_else(|| "en_US".to_owned()),
        font_family: default_style.font_name,
        font_size: default_style.font_size,
        created_ms,
        modified_ms,
        page_width,
        page_height,
        paper,
        pdfs,
        pdf_pages,
        ink,
        media,
        text: RichText { text, spans },
        thumbnail: read_zip_entry(&mut bundle, "thumbnail.png").ok(),
        seed: content_seed(&bytes),
        warnings: warnings.into_messages(),
    })
}

/// Content left out of the conversion, counted for the report.
#[derive(Default)]
struct Warnings {
    strokes: usize,
    shapes: usize,
    far_pages: usize,
    frames: usize,
    unreadable_images: Vec<String>,
    unsupported_media: Vec<String>,
    unknown_media: usize,
}

impl Warnings {
    fn into_messages(self) -> Vec<String> {
        let mut counted = vec![
            (self.strokes, "stroke(s) with unknown geometry".to_owned()),
            (self.shapes, "shape(s) with unknown geometry".to_owned()),
            (
                self.far_pages,
                format!("object(s) beyond page {MAX_PAGE_INDEX}"),
            ),
            (
                self.frames,
                "media object(s) without a valid frame".to_owned(),
            ),
            (
                self.unknown_media,
                "media object(s) of an unknown kind".to_owned(),
            ),
        ];
        for (kinds, what) in [
            (self.unreadable_images, "unreadable image(s)"),
            (self.unsupported_media, "unsupported media object(s)"),
        ] {
            let mut counts = BTreeMap::new();
            for kind in kinds {
                *counts.entry(kind).or_insert(0usize) += 1;
            }
            counted.extend(
                counts
                    .into_iter()
                    .map(|(kind, count)| (count, format!("{what} ({kind})"))),
            );
        }
        counted
            .into_iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, what)| format!("{count} {what} were left out"))
            .collect()
    }
}

/// Records that are still part of the note. An edit log removes records with erase
/// records and can hold several versions of one record; the last version is current
/// and takes the place of the first.
fn live_records<'a>(flat: &FlatBuffer<'_>, records: &'a [Record]) -> Vec<&'a Record> {
    let mut erased = erased_records(flat, records);
    // The text of an erased text box goes with it.
    for record in records {
        let owner = event_id(flat, record.payload, 0);
        if record.kind == EVENT_TEXT && owner.is_some_and(|owner| erased.contains(&owner)) {
            erased.extend(record.id);
        }
    }
    let mut latest = BTreeMap::new();
    for record in records {
        if let Some(id) = record.id {
            latest.insert(id, record);
        }
    }
    let mut seen = BTreeSet::new();
    records
        .iter()
        .filter(|record| record.id.is_none_or(|id| !erased.contains(&id)))
        .filter_map(|record| match record.id {
            Some(id) if !seen.insert(id) => None,
            Some(id) => latest.get(&id).copied(),
            None => Some(record),
        })
        .collect()
}

/// Numbers every paragraph of a list, per indent level, as Notability does for
/// bullets and numbered lists alike. A list is a run of paragraphs with the same
/// decoration; a new list starts again at 1.
fn number_list_paragraphs(text: &str, spans: Vec<TextSpan>) -> Vec<TextSpan> {
    let mut styles: Vec<TextStyle> = Vec::with_capacity(text.chars().count());
    for span in &spans {
        styles.extend(std::iter::repeat_n(span.style.clone(), span.length));
    }
    let mut counters = [0i64; 16];
    let mut list = None;
    let mut paragraph_start = 0;
    let characters: Vec<char> = text.chars().collect();
    while paragraph_start < styles.len() {
        let paragraph_end = (paragraph_start..styles.len())
            .find(|&index| characters.get(index) == Some(&'\n'))
            .map_or(styles.len(), |newline| newline + 1);
        let style = &styles[paragraph_start];
        let number = match style.indent_decoration_style {
            Some(decoration) => {
                if list != Some(decoration) {
                    list = Some(decoration);
                    counters = [0; 16];
                }
                let level = style.indent_level.unwrap_or(0).min(counters.len() - 1);
                counters[level] += 1;
                counters[level + 1..].fill(0);
                Some(counters[level])
            }
            None => {
                list = None;
                None
            }
        };
        for style in &mut styles[paragraph_start..paragraph_end] {
            style.indent_decoration_number = number;
        }
        paragraph_start = paragraph_end;
    }
    let mut numbered = Vec::new();
    let mut start = 0;
    while start < styles.len() {
        let mut end = start + 1;
        while end < styles.len() && styles[end] == styles[start] {
            end += 1;
        }
        numbered.push(TextSpan {
            start,
            length: end - start,
            style: styles[start].clone(),
        });
        start = end;
    }
    if numbered.is_empty() {
        return spans;
    }
    numbered
}

struct Record {
    index: usize,
    table: usize,
    id: Option<EventId>,
    kind: u8,
    payload: usize,
    time_ms: Option<i64>,
}

impl Record {
    fn read(flat: &FlatBuffer<'_>, index: usize, table: usize) -> Option<Self> {
        Some(Self {
            index,
            table,
            id: event_id(flat, table, 0),
            kind: event_type(flat, table)?,
            payload: flat.offset_field(table, 5)?,
            time_ms: flat
                .field(table, 1)
                .and_then(|field| flat.u64(field))
                .and_then(|value| i64::try_from(value).ok()),
        })
    }
}

/// Bundles that are an edit log rather than a compacted note list removed records
/// in erase records.
fn erased_records(flat: &FlatBuffer<'_>, records: &[Record]) -> BTreeSet<EventId> {
    records
        .iter()
        .filter(|record| record.kind == EVENT_ERASE)
        .filter_map(|record| id_vector(flat, record.payload, 0))
        .flatten()
        .collect()
}

fn id_vector(flat: &FlatBuffer<'_>, table: usize, index: usize) -> Option<Vec<EventId>> {
    let target = flat.offset_field(table, index)?;
    let length = flat.u32(target)? as usize;
    (0..length)
        .map(|item| {
            let position = target + 4 + item * 8;
            Some([flat.u32(position)?, flat.u32(position + 4)?])
        })
        .collect()
}

#[derive(Default)]
struct DocumentInfo {
    title: Option<String>,
    language: Option<String>,
    font_family: Option<String>,
    font_size: Option<f32>,
    page_size: Option<(f32, f32)>,
    paper: Option<PaperStyle>,
}

impl DocumentInfo {
    fn read(flat: &FlatBuffer<'_>, payload: usize) -> Self {
        let nested_string = |slot| {
            flat.offset_field(payload, slot)
                .and_then(|table| flat.string_field(table, 0))
                .map(str::to_owned)
        };
        let layout = flat
            .offset_field(payload, 1)
            .and_then(|value| flat.offset_field(value, 0));
        Self {
            title: nested_string(0),
            language: nested_string(2),
            font_family: flat.string_field(payload, 4).map(str::to_owned),
            font_size: flat
                .field(payload, 5)
                .and_then(|field| flat.f32(field))
                .filter(|size| size.is_finite() && *size > 0.0),
            page_size: layout.and_then(|layout| flat.pair_field(layout, 3)).filter(
                |(width, height)| {
                    width.is_finite() && height.is_finite() && *width > 0.0 && *height > 0.0
                },
            ),
            paper: layout
                .and_then(|layout| flat.offset_field(layout, 0))
                .map(|style| PaperStyle::read(flat, style)),
        }
    }
}

/// Paper of the document or of a sticky note.
struct PaperStyle {
    /// 0 ruled, 1 dots, 2 grid; blank when absent.
    pattern: Option<u8>,
    spacing: Option<f32>,
    color: Option<[u8; 4]>,
    legacy: Option<u8>,
}

impl PaperStyle {
    fn read(flat: &FlatBuffer<'_>, table: usize) -> Self {
        Self {
            pattern: flat.field(table, 0).and_then(|field| flat.u8(field)),
            spacing: flat
                .field(table, 1)
                .and_then(|field| flat.f32(field))
                .filter(|spacing| spacing.is_finite() && *spacing > 0.0),
            color: flat
                .field(table, 4)
                .and_then(|field| flat.bytes::<4>(field)),
            legacy: flat.field(table, 5).and_then(|field| flat.u8(field)),
        }
    }

    /// Lines with their spacing converted from document units to inches.
    fn lines(&self, units_per_inch: f32) -> Option<PaperLines> {
        let pattern = match self.pattern? {
            0 => LinePattern::Ruled,
            1 => LinePattern::Dots,
            2 => LinePattern::Grid,
            _ => return None,
        };
        Some(PaperLines {
            pattern,
            spacing_inches: self.spacing.map_or(DEFAULT_LINE_SPACING_INCHES, |spacing| {
                spacing / units_per_inch
            }),
        })
    }
}

/// Notability measures line spacing in inches of an 8.5 in wide page.
fn document_paper(style: &PaperStyle, page_width: f32) -> Paper {
    let identifier = match (style.legacy, style.color) {
        (Some(index), _) => format!("Legacy:{index}"),
        (None, Some([r, g, b, _])) => format!("Color:#{r:02X}{g:02X}{b:02X}"),
        (None, None) => "Legacy:13".to_owned(),
    };
    Paper {
        identifier,
        lines: style.lines(page_width / 8.5),
    }
}

fn sticky_lines(style: &PaperStyle) -> Option<PaperLines> {
    style.lines(STICKY_PAPER_WIDTH / 8.5)
}

/// Where pages begin in the `.note` strip: PDF pages first, then plain paper.
struct PageLayout {
    inset: f64,
    plain_height: f64,
    tops: Vec<f64>,
    end: f64,
}

impl PageLayout {
    fn new(inset: f32, plain_height: f32, slot_heights: &[f64]) -> Self {
        let mut tops = Vec::with_capacity(slot_heights.len());
        let mut end = 0.0;
        for height in slot_heights {
            tops.push(end);
            end += height;
        }
        Self {
            inset: f64::from(inset),
            plain_height: f64::from(plain_height),
            tops,
            end,
        }
    }

    fn top(&self, page: usize) -> f64 {
        self.tops
            .get(page)
            .copied()
            .unwrap_or_else(|| self.end + (page - self.tops.len()) as f64 * self.plain_height)
    }

    fn place(&self, page: usize, (x, y): Point) -> Point {
        (x - self.inset, self.top(page) + y)
    }

    fn place_f32(&self, page: usize, (x, y): (f32, f32)) -> (f32, f32) {
        let (x, y) = self.place(page, (f64::from(x), f64::from(y)));
        (x as f32, y as f32)
    }
}

fn read_stroke(flat: &FlatBuffer<'_>, payload: usize, layout: &PageLayout) -> Option<Stroke> {
    let origin = flat.pair_field(payload, 1)?;
    let scale = flat
        .pair_field(payload, 3)
        .filter(|(x_scale, y_scale)| {
            x_scale.is_finite()
                && y_scale.is_finite()
                && x_scale.abs() > f32::EPSILON
                && y_scale.abs() > f32::EPSILON
        })
        .unwrap_or((1.0, 1.0));
    let mut curves = decode_curves(flat.byte_vector(payload, 9)?, origin, scale)?;
    let rotation = flat
        .field(payload, 2)
        .and_then(|field| flat.f32(field))
        .filter(|rotation| rotation.is_finite())
        .unwrap_or(0.0);
    let page = page_index(flat, payload, 0);
    for curve in &mut curves {
        if rotation.abs() > f32::EPSILON {
            rotate_points(&mut curve.points, origin, rotation);
        }
        for point in &mut curve.points {
            *point = layout.place_f32(page, *point);
        }
    }
    let tool = flat
        .field(payload, 4)
        .and_then(|field| flat.u8(field))
        .unwrap_or(0);
    let line_style = flat
        .field(payload, 5)
        .and_then(|field| flat.u8(field))
        .unwrap_or(1);
    // The scale of a transformed group applies to the width as well.
    let width = flat.field(payload, 8).and_then(|field| flat.f32(field))?
        * (scale.0.abs() + scale.1.abs())
        * 0.5;
    Some(Stroke {
        curves,
        width,
        rgba: flat
            .field(payload, 7)
            .and_then(|field| flat.bytes::<4>(field))
            .unwrap_or([0, 0, 0, 255]),
        style: tool_style(tool),
        dash: (line_style > 1).then(|| line_style - 1),
    })
}

/// Decode an ink path into curves in page coordinates.
///
/// Cubic paths (kind 3) are a node, then per segment a command, its points as
/// offsets from the origin and the next node. Command 0 continues with a cubic
/// segment; command 3 jumps to a point that starts the next piece. A node is the
/// width multiplier and force as half floats, then altitude and azimuth bytes.
/// Linear paths (kind 7) have no nodes: command 6 draws a line, 7 moves.
fn decode_curves(data: &[u8], origin: (f32, f32), scale: (f32, f32)) -> Option<Vec<Curve>> {
    if data.len() < 8 {
        return None;
    }
    let precision = data[0];
    let sample_count = u16::from_le_bytes(data.get(1..3)?.try_into().ok()?) as usize;
    let path_kind = data[3];
    if sample_count == 0 || !matches!(precision, 0 | 1) {
        return None;
    }
    let scalar_size = if precision == 0 { 2 } else { 4 };
    let read_scalar = |offset: usize| -> Option<f32> {
        if precision == 0 {
            read_half(data, offset)
        } else {
            Some(f32::from_le_bytes(
                data.get(offset..offset + 4)?.try_into().ok()?,
            ))
        }
    };
    let point = |x: f32, y: f32| (origin.0 + x * scale.0, origin.1 + y * scale.1);
    let mut cursor = if precision == 0 { 8 } else { 12 };
    let mut curves = Vec::new();
    match path_kind {
        7 => {
            let mut vertices = vec![origin];
            for _ in 1..sample_count {
                let command = *data.get(cursor)?;
                let next = point(
                    read_scalar(cursor + 1)?,
                    read_scalar(cursor + 1 + scalar_size)?,
                );
                match command {
                    6 => vertices.push(next),
                    7 => curves.push(polyline_curve(std::mem::replace(&mut vertices, vec![next]))),
                    _ => return None,
                }
                cursor += 1 + scalar_size * 2;
            }
            curves.push(polyline_curve(vertices));
        }
        3 => {
            let mut points = vec![origin];
            let mut nodes = Vec::new();
            for _ in 1..sample_count {
                nodes.push(read_node(data, cursor));
                let command = *data.get(cursor + 6)?;
                let coordinate_count = match command {
                    0 => 6,
                    3 => 2,
                    _ => return None,
                };
                let mut values = [0.0; 6];
                for (index, value) in values.iter_mut().take(coordinate_count).enumerate() {
                    *value = read_scalar(cursor + 7 + index * scalar_size)?;
                }
                if command == 0 {
                    points.extend([
                        point(values[0], values[1]),
                        point(values[2], values[3]),
                        point(values[4], values[5]),
                    ]);
                } else {
                    curves.push(Curve {
                        points: std::mem::replace(&mut points, vec![point(values[0], values[1])]),
                        nodes: std::mem::take(&mut nodes),
                    });
                }
                cursor += 7 + coordinate_count * scalar_size;
            }
            nodes.push(read_node(data, cursor));
            curves.push(Curve { points, nodes });
        }
        _ => return None,
    }
    let finite = |curve: &Curve| {
        curve
            .points
            .iter()
            .all(|(x, y)| x.is_finite() && y.is_finite())
    };
    curves.retain(finite);
    (!curves.is_empty()).then_some(curves)
}

/// A node of a cubic path; tilt is quantized to a byte for pi/2 and a full turn.
fn read_node(data: &[u8], offset: usize) -> Node {
    let finite_or =
        |value: Option<f32>, default: f32| value.filter(|v| v.is_finite()).unwrap_or(default);
    let altitude = data
        .get(offset + 4)
        .map(|byte| f32::from(*byte) / 255.0 * FRAC_PI_2);
    // In double precision, so that every platform's math library rounds to the same
    // single-precision vector.
    let azimuth = data
        .get(offset + 5)
        .map(|byte| f64::from(*byte) * std::f64::consts::TAU / 256.0);
    Node {
        fractional_width: finite_or(read_half(data, offset), 1.0),
        force: finite_or(read_half(data, offset + 2), 1.0),
        altitude: altitude.unwrap_or(Node::UPRIGHT.altitude),
        azimuth: azimuth.map_or(Node::UPRIGHT.azimuth, |angle| {
            (angle.cos() as f32, angle.sin() as f32)
        }),
    }
}

/// Straight segments as cubic segments with their controls on the line.
fn polyline_curve(vertices: Vec<(f32, f32)>) -> Curve {
    let mut points = vec![vertices[0]];
    for pair in vertices.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        let (dx, dy) = (x1 - x0, y1 - y0);
        points.extend([
            (x0 + dx / 3.0, y0 + dy / 3.0),
            (x0 + dx * 2.0 / 3.0, y0 + dy * 2.0 / 3.0),
            (x1, y1),
        ]);
    }
    Curve {
        nodes: vec![Node::UPRIGHT; vertices.len()],
        points,
    }
}

fn read_shape(flat: &FlatBuffer<'_>, payload: usize, layout: &PageLayout) -> Option<Shape> {
    let (origin_x, origin_y) = flat.pair_field(payload, 1)?;
    let origin = (f64::from(origin_x), f64::from(origin_y));
    let table = flat.offset_field(payload, 5)?;
    let relative = |(x, y): (f32, f32)| (origin.0 + f64::from(x), origin.1 + f64::from(y));
    let mut geometry = match flat.field(payload, 4).and_then(|field| flat.u8(field))? {
        1 => ShapeGeometry::Line {
            start: relative(flat.pair_field(table, 0).unwrap_or((0.0, 0.0))),
            end: relative(flat.pair_field(table, 3)?),
            control: flat.pair_field(table, 1).map(relative),
        },
        2 => {
            let mut points: Vec<Point> = flat
                .point_vector(table, 0)?
                .into_iter()
                .map(relative)
                .collect();
            if points.len() > 3 && points.first() == points.last() {
                points.pop();
            }
            if points.len() < 2 {
                return None;
            }
            ShapeGeometry::Polygon(points)
        }
        3 => {
            let (width, height) = flat.pair_field(table, 1)?;
            let (width, height) = (f64::from(width), f64::from(height));
            ShapeGeometry::Ellipse([
                origin,
                (origin.0 + width, origin.1),
                (origin.0 + width, origin.1 + height),
                (origin.0, origin.1 + height),
            ])
        }
        _ => return None,
    };
    let rotation = f64::from(
        flat.field(payload, 2)
            .and_then(|field| flat.f32(field))
            .filter(|rotation| rotation.is_finite())
            .unwrap_or(0.0),
    );
    let page = page_index(flat, payload, 0);
    let (sin, cos) = rotation.sin_cos();
    let transform = |(x, y): Point| {
        let (dx, dy) = (x - origin.0, y - origin.1);
        layout.place(
            page,
            (
                origin.0 + dx * cos - dy * sin,
                origin.1 + dx * sin + dy * cos,
            ),
        )
    };
    match &mut geometry {
        ShapeGeometry::Line {
            start,
            end,
            control,
        } => {
            *start = transform(*start);
            *end = transform(*end);
            if let Some(control) = control {
                *control = transform(*control);
            }
        }
        ShapeGeometry::Polygon(points) => points
            .iter_mut()
            .for_each(|point| *point = transform(*point)),
        ShapeGeometry::Ellipse(corners) => corners
            .iter_mut()
            .for_each(|point| *point = transform(*point)),
    }
    if !shape_points(&geometry).all(|(x, y)| x.is_finite() && y.is_finite()) {
        return None;
    }
    let tool = flat
        .field(payload, 6)
        .and_then(|field| flat.u8(field))
        .unwrap_or(0);
    let dash = flat
        .field(payload, 7)
        .and_then(|field| flat.u8(field))
        .unwrap_or(1);
    let style = tool_style(tool);
    Some(Shape {
        geometry,
        width: f64::from(flat.field(payload, 10).and_then(|field| flat.f32(field))?),
        rgba: flat
            .field(payload, 9)
            .and_then(|field| flat.bytes::<4>(field))
            .unwrap_or([0, 0, 0, 255]),
        style,
        dash: (dash > 1).then(|| dash - 1),
        // Pencil shapes store their force with the width they were drawn at.
        force: (tool == 1)
            .then(|| flat.field(payload, 14).and_then(|field| flat.f32(field)))
            .flatten()
            .filter(|force| force.is_finite())
            .map(f64::from),
    })
}

fn shape_points(geometry: &ShapeGeometry) -> Box<dyn Iterator<Item = Point> + '_> {
    match geometry {
        ShapeGeometry::Line {
            start,
            end,
            control,
        } => Box::new([*start, *end].into_iter().chain(*control)),
        ShapeGeometry::Polygon(points) => Box::new(points.iter().copied()),
        ShapeGeometry::Ellipse(corners) => Box::new(corners.iter().copied()),
    }
}

/// Images, sticky notes and text boxes, in event order, with their z-order on the
/// page from the events' sort keys.
fn read_media<'a, R: Read + Seek>(
    flat: &FlatBuffer<'_>,
    records: impl Iterator<Item = &'a Record>,
    bundle: &mut ZipArchive<R>,
    layout: &PageLayout,
    mut text_boxes: BTreeMap<usize, TextBlock>,
    warnings: &mut Warnings,
) -> Result<Vec<Media>> {
    let names: Vec<String> = bundle.file_names().map(str::to_owned).collect();
    let mut media = Vec::new();
    let mut order = Vec::new();
    for record in records {
        let payload = record.payload;
        let sort_key = flat
            .field(payload, 9)
            .and_then(|field| flat.u64(field))
            .and_then(|value| i64::try_from(value).ok())
            .unwrap_or(record.index as i64);
        let page = page_index(flat, payload, 2);
        if page > MAX_PAGE_INDEX {
            warnings.far_pages += 1;
            continue;
        }
        let item = if let Some(block) = text_boxes.remove(&record.table) {
            // Text boxes come with the inset already taken off.
            let y = layout.top(page) + f64::from(block.y);
            Media::TextBox {
                frame: MediaFrame {
                    origin: (block.x, y as f32),
                    size: (block.width, block.height),
                    rotation: block.rotation_degrees.to_radians(),
                    z_index: 0,
                },
                text: RichText {
                    spans: number_list_paragraphs(&block.text, block.text_spans),
                    text: block.text,
                },
            }
        } else if let Some(asset) = media_asset(flat, payload) {
            if !asset.mime.starts_with("image/") {
                warnings.unsupported_media.push(asset.mime);
                continue;
            }
            let Some(frame) = media_frame(flat, payload, page, layout) else {
                warnings.frames += 1;
                continue;
            };
            let Some(image) = read_image(bundle, &names, &asset)? else {
                warnings.unreadable_images.push(asset.mime);
                continue;
            };
            Media::Image {
                frame,
                image,
                locked: flat
                    .field(payload, 20)
                    .and_then(|field| flat.u8(field))
                    .is_some_and(|value| value != 0),
            }
        } else if let Some(paper) = flat.offset_field(payload, 15) {
            let Some(frame) = media_frame(flat, payload, page, layout) else {
                warnings.frames += 1;
                continue;
            };
            let style = PaperStyle::read(flat, paper);
            Media::Sticky {
                frame,
                color: style.color.unwrap_or(DEFAULT_STICKY_COLOR),
                lines: sticky_lines(&style),
            }
        } else {
            warnings.unknown_media += 1;
            continue;
        };
        order.push((page, sort_key, record.index, media.len()));
        media.push(item);
    }
    order.sort_unstable();
    let mut previous_page = None;
    let mut z_index = 0;
    for (page, _, _, index) in order {
        if previous_page != Some(page) {
            previous_page = Some(page);
            z_index = 0;
        }
        match &mut media[index] {
            Media::Image { frame, .. }
            | Media::Sticky { frame, .. }
            | Media::TextBox { frame, .. } => {
                frame.z_index = z_index;
            }
        }
        z_index += 1;
    }
    Ok(media)
}

/// The unrotated frame in document coordinates. Bundles store the top-left corner
/// after rotation about the center.
fn media_frame(
    flat: &FlatBuffer<'_>,
    payload: usize,
    page: usize,
    layout: &PageLayout,
) -> Option<MediaFrame> {
    let position = flat.pair_field(payload, 3)?;
    let size = flat.pair_field(payload, 6)?;
    if ![position.0, position.1, size.0, size.1]
        .iter()
        .all(|v| v.is_finite())
        || size.0 <= 0.0
        || size.1 <= 0.0
    {
        return None;
    }
    let rotation = flat
        .field(payload, 4)
        .and_then(|field| flat.f32(field))
        .filter(|rotation| rotation.is_finite())
        .unwrap_or(0.0);
    let origin = unrotate_top_left(position, size, rotation);
    Some(MediaFrame {
        origin: layout.place_f32(page, origin),
        size,
        rotation,
        z_index: 0,
    })
}

struct MediaAsset {
    hash: String,
    mime: String,
    logical_size: Option<(f32, f32)>,
    crop: Option<[f32; 4]>,
}

/// The asset of a media event, whatever its kind.
fn media_asset(flat: &FlatBuffer<'_>, payload: usize) -> Option<MediaAsset> {
    let media = flat.offset_field(payload, 10)?;
    let asset = flat.offset_field(media, 0)?;
    let mime = flat.string_field(asset, 2).unwrap_or("image/jpeg");
    let hash = flat.slice(flat.field(asset, 0)?, 64)?;
    let logical_size = flat.pair_field(media, 1).filter(|(width, height)| {
        width.is_finite() && height.is_finite() && *width > 0.0 && *height > 0.0
    });
    let crop = flat.field(payload, 11).and_then(|field| {
        let crop = flat.float_array::<4>(field)?;
        if !crop.iter().all(|v| v.is_finite()) || crop[2] <= 0.0 || crop[3] <= 0.0 {
            return None;
        }
        // A crop covering the whole asset is no crop.
        let covers = logical_size.is_some_and(|(width, height)| {
            crop[0] <= 0.5
                && crop[1] <= 0.5
                && crop[0] + crop[2] >= width - 0.5
                && crop[1] + crop[3] >= height - 0.5
        });
        (!covers).then_some(crop)
    });
    Some(MediaAsset {
        hash: hex(hash),
        mime: mime.to_owned(),
        logical_size,
        crop,
    })
}

/// Assets are named by their content hash; the extension is not relied on. Images
/// whose header cannot be read, or GIFs whose first frame cannot be decoded for the
/// still image, are left out.
fn read_image<R: Read + Seek>(
    bundle: &mut ZipArchive<R>,
    names: &[String],
    asset: &MediaAsset,
) -> Result<Option<ImageData>> {
    let short_hash = &asset.hash[..64];
    let Some(name) = names.iter().find(|name| {
        let stem = Path::new(name.as_str())
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        stem.eq_ignore_ascii_case(&asset.hash) || stem.eq_ignore_ascii_case(short_hash)
    }) else {
        return Ok(None);
    };
    let bytes = read_zip_entry(bundle, name)?;
    let Some(kind) = ImageKind::sniff(&bytes) else {
        return Ok(None);
    };
    let Ok((width, height)) = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(image::ImageError::IoError)
        .and_then(|reader| reader.into_dimensions())
    else {
        return Ok(None);
    };
    if kind == ImageKind::Gif && image::load_from_memory(&bytes).is_err() {
        return Ok(None);
    }
    let logical_size = asset.logical_size.unwrap_or((width as f32, height as f32));
    Ok(Some(ImageData {
        bytes,
        kind,
        logical_size,
        crop: asset.crop,
    }))
}

#[cfg(test)]
mod tests {
    use super::{decode_curves, number_list_paragraphs, sticky_lines, PaperStyle};
    use crate::converter::model::TextSpan;
    use crate::converter::note_writer::Node;

    #[test]
    fn numbers_each_list_paragraph_per_level_and_restarts_with_a_new_list() {
        let text = "a\nb\nc\nd\ne\nf\ng\n";
        let paragraphs = [
            (Some(1), 0), // bullets
            (Some(1), 1),
            (Some(1), 0),
            (Some(1), 1),
            (Some(2), 0), // a numbered list starts again at 1
            (Some(2), 0),
            (None, 0),
        ];
        let mut style = super::default_text_style();
        let spans: Vec<TextSpan> = paragraphs
            .iter()
            .enumerate()
            .map(|(index, (decoration, level))| {
                style.indent_decoration_style = *decoration;
                style.indent_level = decoration.map(|_| *level);
                TextSpan {
                    start: index * 2,
                    length: 2,
                    style: style.clone(),
                }
            })
            .collect();
        let numbered = number_list_paragraphs(text, spans);
        let number_at = |index: usize| {
            numbered
                .iter()
                .find(|span| span.start <= index && index < span.start + span.length)
                .unwrap()
                .style
                .indent_decoration_number
        };
        let expected = [Some(1), Some(1), Some(2), Some(1), Some(1), Some(2), None];
        for (paragraph, number) in expected.iter().enumerate() {
            // The paragraph's newline is numbered too.
            assert_eq!(number_at(paragraph * 2), *number);
            assert_eq!(number_at(paragraph * 2 + 1), *number);
        }
    }

    #[test]
    fn keeps_node_attributes_with_each_erased_piece() {
        // Half precision, three nodes: a cubic segment, a jump, then the last node.
        let mut encoded = vec![0, 3, 0, 3, 0, 0, 0, 0];
        encoded.extend([0x00, 0x38, 0x00, 0x3c, 120, 39, 0]); // node 0.5, 1.0, then cubic
        for value in [0x3c00u16, 0, 0x4000, 0, 0x4200, 0] {
            encoded.extend(value.to_le_bytes());
        }
        encoded.extend([0x00, 0x3c, 0x00, 0x3a, 255, 0, 3]); // node 1.0, 0.75, then jump
        for value in [0x4900u16, 0] {
            encoded.extend(value.to_le_bytes());
        }
        encoded.extend([0x00, 0x40, 0x00, 0x3c, 255, 0]); // node 2.0 at the jump target
        let curves = decode_curves(&encoded, (0.0, 0.0), (1.0, 1.0)).unwrap();
        assert_eq!(curves.len(), 2);
        assert_eq!(
            curves[0].points,
            vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (3.0, 0.0)]
        );
        assert_eq!(curves[1].points, vec![(10.0, 0.0)]);
        assert_eq!(curves[0].nodes.len(), 2);
        assert_eq!(
            (
                curves[0].nodes[0].fractional_width,
                curves[0].nodes[0].force
            ),
            (0.5, 1.0)
        );
        assert!((curves[0].nodes[0].altitude - 0.7392).abs() < 1e-3);
        assert_eq!(
            curves[0].nodes[1],
            Node {
                force: 0.75,
                ..Node::UPRIGHT
            }
        );
        assert_eq!(curves[1].nodes[0].fractional_width, 2.0);
    }

    #[test]
    fn linear_paths_become_straight_cubic_segments() {
        let encoded = [
            0, 3, 0, 7, 0, 0, 0, 0, 6, 0x00, 0x42, 0, 0, 7, 0x00, 0x49, 0, 0,
        ];
        let curves = decode_curves(&encoded, (0.0, 0.0), (1.0, 1.0)).unwrap();
        assert_eq!(curves.len(), 2);
        assert_eq!(
            curves[0].points,
            vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (3.0, 0.0)]
        );
        assert_eq!(curves[0].nodes, vec![Node::UPRIGHT; 2]);
        assert_eq!(curves[1].points, vec![(10.0, 0.0)]);
    }

    #[test]
    fn sticky_line_spacing_matches_the_sample() {
        let style = PaperStyle {
            pattern: Some(0),
            spacing: Some(13.520_157),
            color: None,
            legacy: None,
        };
        let lines = sticky_lines(&style).unwrap();
        assert!((lines.spacing_inches - 0.196_850_5).abs() < 1e-5);
    }
}
