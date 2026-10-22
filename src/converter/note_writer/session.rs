//! `Session.plist`: the `NoteTakingSession` object graph.
use super::archive::{integer, null, ArchiveBuilder};
use super::ink::{empty_spatial_hash, rectangle_path, spatial_hash};
use super::text::{font_postscript_name, text_objects};
use super::{
    uuid_string, IdSource, ImageData, ImageKind, LinePattern, Media, MediaFrame, NoteExport,
    PaperLines, RichText,
};
use crate::Result;
use plist::Value;
use std::collections::BTreeMap;
use std::io::Cursor;

const BUNDLE_VERSION: &str = "16.1.5";
const SESSION_FORMAT_VERSION: i64 = 9;
const FORMATTED_STRING_VERSION: i64 = 5;
const MAX_STILL_SIDE: f32 = 4096.0;

pub(super) struct SessionFiles {
    pub(super) session: Vec<u8>,
    /// Package members the session refers to, relative to the package folder.
    pub(super) files: Vec<(String, Vec<u8>)>,
}

pub(super) fn build_session(
    note: &NoteExport,
    package_path: &str,
    ids: &mut IdSource,
) -> Result<SessionFiles> {
    SessionWriter {
        b: ArchiveBuilder::new("GLKeyedArchiver", "$0"),
        note,
        package_path,
        ids,
        files: Vec::new(),
        image_counts: BTreeMap::new(),
    }
    .build()
}

struct SessionWriter<'a> {
    b: ArchiveBuilder,
    note: &'a NoteExport,
    package_path: &'a str,
    ids: &'a mut IdSource,
    files: Vec<(String, Vec<u8>)>,
    image_counts: BTreeMap<&'static str, usize>,
}

impl SessionWriter<'_> {
    fn build(mut self) -> Result<SessionFiles> {
        let note = self.note;
        let root = self.b.reserve();
        let rich_text = self.b.reserve();
        let (pdf_files, layout) = self.page_layout();
        let media = self.media_objects()?;
        let media = self.b.array(media);
        let device = self.b.string("iPad");
        let reflow = self.b.object(
            "NBReflowStateLocked",
            vec![
                (
                    "pageWidthInDocumentCoordsKey",
                    Value::Real(f64::from(note.page_width)),
                ),
                ("nativeLayoutDeviceStringKey", device),
            ],
        );
        let hash = spatial_hash(&mut self.b, &note.ink, self.ids)?;
        let fields = self.formatted_string_fields(
            hash,
            reflow,
            &note.text,
            pdf_files,
            media,
            layout.clone(),
        );
        self.b.fill_object(&rich_text, "FormattedString", fields);

        let paper = self.document_paper();
        // Notability 15 and later read the page list from the paper layout model;
        // older versions read it from the rich text.
        let layout = self.b.mutable_array(layout);
        let paper_model = self.b.object(
            "Notability.NoteDocumentPaperLayoutModel",
            vec![
                ("documentPaperAttributes", paper),
                ("pageLayoutArray", layout),
            ],
        );
        let name = self.b.string(&note.title);
        let package_path = self.b.string(self.package_path);
        let creation_date = self
            .b
            .date(note.created_ms.or(note.modified_ms).unwrap_or_default());
        let subject = self.b.string("unsortedNotesKey");
        let tags = self.b.string("");
        let bundle_version = self.b.string(BUNDLE_VERSION);
        let language = self.b.string(&note.language);
        let events = self.b.object(
            "NBCPEventManager",
            vec![
                ("NBCPTimeManagerSOADurationsKey", Value::Data(Vec::new())),
                ("NBCPTimeManagerSOAEventUUIDsKey", Value::Data(Vec::new())),
                ("NBCPTimeManagerSOANumEventsKey", integer(0)),
                ("NBCPTimeManagerSOARecordingIDsKey", Value::Data(Vec::new())),
                ("NBCPTimeManagerSOATimestampsKey", Value::Data(Vec::new())),
            ],
        );
        let font = self.last_used_font();
        self.b.fill_object(
            &root,
            "NoteTakingSession",
            vec![
                ("subject", subject),
                ("NBNoteTakingSessionBundleVersionNumberKey", bundle_version),
                ("NBNoteTakingSessionHandwritingLanguageKey", language),
                ("contentPlaybackEventManager", events),
                ("NBPaperMinorVersionNumber", integer(0)),
                ("NBNoteTakingSessionLastUsedFontKey", font),
                ("NBNoteTakingSessionMinorVersionNumberKey", integer(0)),
                ("packagePath", package_path),
                ("tags", tags),
                (
                    "NBNoteTakingSessionIsHighlighterBehindTextKey",
                    Value::Boolean(true),
                ),
                ("sessionFormatVersion", integer(SESSION_FORMAT_VERSION)),
                (
                    "NBNoteTakingSessionDocumentPaperLayoutModelKey",
                    paper_model,
                ),
                (
                    "NBNoteTakingSessionAllowHighlighterConversionKey",
                    Value::Boolean(false),
                ),
                ("NBPaperMajorVersionNumber", integer(3)),
                ("richText", rich_text),
                ("isReadOnly", Value::Boolean(false)),
                ("creationDate", creation_date),
                ("name", name),
            ],
        );
        Ok(SessionFiles {
            session: self.b.finish(&root)?,
            files: self.files,
        })
    }

    /// The keys of a `FormattedString`: the note's rich text, or a text box's store.
    fn formatted_string_fields(
        &mut self,
        hash: Value,
        reflow: Value,
        text: &RichText,
        pdf_files: Value,
        media: Value,
        layout: Vec<Value>,
    ) -> Vec<(&'static str, Value)> {
        let text = text_objects(&mut self.b, text, self.ids);
        let overlay = self
            .b
            .object("HandwritingObject", vec![("SpatialHash", hash)]);
        let layout = self.b.mutable_array(layout);
        vec![
            ("formatVersion", integer(FORMATTED_STRING_VERSION)),
            ("formattedStringTextAlignmentKey", integer(0)),
            ("attributedString", text.attributed),
            ("NBAttributedBackingString", text.backing),
            ("Handwriting Overlay", overlay),
            ("reflowState", reflow),
            ("didBecomeReflowable", Value::Boolean(true)),
            ("pdfFiles", pdf_files),
            ("mediaObjects", media),
            ("recordingTimestampString", text.timestamps),
            ("pageLayoutArray", layout),
        ]
    }

    /// The PDF files, and one layout entry per PDF-backed page in page order. Plain
    /// paper notes have no entries: Notability derives their pages from the content,
    /// and pages below the last PDF page are plain paper.
    fn page_layout(&mut self) -> (Value, Vec<Value>) {
        let note = self.note;
        let mut files = Vec::new();
        for data in &note.pdfs {
            let name = format!("{}.pdf", uuid_string(&self.ids.uuid()));
            self.files.push((format!("PDFs/{name}"), data.clone()));
            let name_value = self.b.string(&name);
            let highlights = self.b.array(Vec::new());
            let object = self.b.object(
                "PDFFile",
                vec![
                    ("pdfFileName", name_value.clone()),
                    ("contentBoxVersion", integer(1)),
                    ("highlights", highlights),
                    ("version", integer(2)),
                    ("type", integer(0)),
                ],
            );
            files.push((name_value, object));
        }
        let mut layout = Vec::new();
        for (index, page) in note.pdf_pages.iter().enumerate() {
            let Some((name, file)) = files.get(page.file).cloned() else {
                continue;
            };
            let document_page = self.b.scalar(integer(index as i64 + 1));
            let source_page = self.b.scalar(integer(page.source_page as i64 + 1));
            let original = self.b.scalar(Value::Boolean(true));
            let bookmarked = self.b.scalar(Value::Boolean(false));
            layout.push(self.b.string_dictionary(
                false,
                vec![
                    ("kPageLayoutDocumentPageNumberKey", document_page),
                    ("kPageLayoutPDFFileNameKey", name),
                    ("kPageLayoutPDFFileKey", file),
                    ("kPageLayoutPDFPageNumberKey", source_page),
                    ("kPageLayoutPDFIsOriginalPageKey", original),
                    ("kPageLayoutPageIsBookmarkedKey", bookmarked),
                ],
            ));
        }
        let objects = files.into_iter().map(|(_, object)| object).collect();
        (self.b.array(objects), layout)
    }

    fn document_paper(&mut self) -> Value {
        let note = self.note;
        let (width, height) = (note.page_width, note.page_height);
        let (orientation, aspect) = if height >= width {
            ("portrait", height / width)
        } else {
            ("landscape", width / height)
        };
        self.paper_attributes(
            &note.paper.identifier,
            orientation,
            &paper_size_name(aspect),
            &format!("lockedWidth:{width:?}:iPad"),
            note.paper.lines,
            false,
        )
    }

    fn paper_attributes(
        &mut self,
        identifier: &str,
        orientation: &str,
        size: &str,
        sizing: &str,
        lines: Option<PaperLines>,
        sticky: bool,
    ) -> Value {
        let identifier = self.b.string(identifier);
        let orientation = self.b.string(orientation);
        let size = self.b.string(size);
        let sizing = self.b.string(sizing);
        let line_style = self.b.string(&line_style_name(lines, sticky));
        self.b.object(
            "GLModel.PaperAttributes",
            vec![
                ("paperIdentifier", identifier),
                ("drawRotatedForOrientation", Value::Boolean(true)),
                ("paperOrientation", orientation),
                ("paperSize", size),
                ("paperSizingBehavior", sizing),
                ("alignsTextToLines", Value::Boolean(true)),
                ("lineStyle2", line_style),
            ],
        )
    }

    fn last_used_font(&mut self) -> Value {
        let note = self.note;
        let family = self.b.string(&note.font_family);
        let name = self
            .b
            .string(&font_postscript_name(&note.font_family, false, false));
        let color = self.b.color([0, 0, 0, 255]);
        let offset = self.b.string("{0, 0}");
        self.b.object(
            "GLFontDescriptor",
            vec![
                ("GLFontDescriptorBold", Value::Boolean(false)),
                ("GLFontDescriptorColor", color),
                ("GLFontDescriptorCondensed", Value::Boolean(false)),
                ("GLFontDescriptorExpanded", Value::Boolean(false)),
                ("GLFontDescriptorFamily", family),
                ("GLFontDescriptorIndentDecorationStyle", integer(3)),
                ("GLFontDescriptorIndentLevel", integer(0)),
                ("GLFontDescriptorItalic", Value::Boolean(false)),
                ("GLFontDescriptorLineSpacing", Value::Real(1.0)),
                ("GLFontDescriptorName", name),
                ("GLFontDescriptorShadowColor", null()),
                ("GLFontDescriptorShadowOffset", offset),
                ("GLFontDescriptorShadowOpacity", Value::Real(0.0)),
                ("GLFontDescriptorShadowRadius", Value::Real(0.0)),
                (
                    "GLFontDescriptorSize",
                    Value::Real(f64::from(note.font_size)),
                ),
                ("GLFontDescriptorStrikethrough", Value::Boolean(false)),
                ("GLFontDescriptorSubscript", Value::Boolean(false)),
                ("GLFontDescriptorSuperscript", Value::Boolean(false)),
                ("GLFontDescriptorUnderlined", Value::Boolean(false)),
            ],
        )
    }

    fn media_objects(&mut self) -> Result<Vec<Value>> {
        let note = self.note;
        let mut objects = Vec::new();
        for media in &note.media {
            objects.push(match media {
                Media::Image {
                    frame,
                    image,
                    locked,
                } => self.image_object(frame, image, *locked)?,
                Media::Sticky {
                    frame,
                    color,
                    lines,
                } => {
                    let (fields, _) = self.canvas_fields(frame, Some(*color), *lines);
                    self.b.object("CanvasMediaObject", fields)
                }
                Media::TextBox { frame, text } => self.text_box_object(frame, text)?,
            });
        }
        Ok(objects)
    }

    /// Keys every `GenericMediaObject` has, and its asset id.
    fn media_fields(
        &mut self,
        frame: &MediaFrame,
        locked: bool,
    ) -> (Vec<(&'static str, Value)>, String) {
        let size = self.b.string(&point_string(frame.size));
        let origin = self.b.string(&point_string(frame.origin));
        let uuid = self.ids.uuid();
        let uuid = self.b.uuid(uuid);
        let assets_id = uuid_string(&self.ids.uuid());
        let assets_id_value = self.b.mutable_string(&assets_id);
        let font = self.b.string("HelveticaNeue");
        let black = self.b.string(&color_string([0, 0, 0, 255]));
        let font_color = self.b.color([0, 0, 0, 255]);
        let handwriting = self.b.mutable_array(Vec::new());
        let fields = vec![
            ("unscaledContentSize", size),
            ("locked", Value::Boolean(locked)),
            ("captionFontColorCrossPlatform", black),
            ("documentOrigin", origin.clone()),
            ("rotationDegrees", Value::Real(f64::from(frame.rotation))),
            ("captionIsUnderlined", Value::Boolean(false)),
            ("documentContentOrigin", origin),
            ("captionFieldText", null()),
            ("minDimension", Value::Real(8.0)),
            ("textWrapMode", integer(1)),
            ("cornerMode", integer(1)),
            ("UUID", uuid),
            ("captionFontName", font),
            ("assetsIdKey", assets_id_value),
            ("handwritingZIndecesKey", handwriting),
            ("captionFontSize", Value::Real(12.0)),
            ("zIndex", integer(frame.z_index)),
            ("captionFontColor", font_color),
            ("maxDimension", Value::Real(1024.0)),
        ];
        (fields, assets_id)
    }

    fn image_object(
        &mut self,
        frame: &MediaFrame,
        image: &ImageData,
        locked: bool,
    ) -> Result<Value> {
        // A GIF keeps its animation under Assets/ and shows a still of its first frame,
        // drawn at the logical size so that crop rectangles address its pixels.
        let (still, kind) = match image.kind {
            ImageKind::Gif => {
                let (width, height) = image.logical_size;
                // Crops are scaled to the still's pixels, so bounding its size is safe.
                let scale = (MAX_STILL_SIDE / width.max(height)).min(1.0);
                let first_frame = image::load_from_memory(&image.bytes)?.resize_exact(
                    ((width * scale).round() as u32).max(1),
                    ((height * scale).round() as u32).max(1),
                    image::imageops::FilterType::Triangle,
                );
                let mut png = Vec::new();
                first_frame.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)?;
                (png, ImageKind::Png)
            }
            kind => (image.bytes.clone(), kind),
        };
        let (pixel_width, pixel_height) = image::ImageReader::new(Cursor::new(&still))
            .with_guessed_format()?
            .into_dimensions()?;
        let pixels = (f64::from(pixel_width), f64::from(pixel_height));
        let relative_path = self.image_path(kind);
        self.files.push((relative_path.clone(), still));

        // Crops are stored in pixels of the stored image, bundles crop in logical units.
        let logical = (
            f64::from(image.logical_size.0),
            f64::from(image.logical_size.1),
        );
        let scale = |pixels: f64, logical: f64| if logical > 0.0 { pixels / logical } else { 1.0 };
        let (scale_x, scale_y) = (scale(pixels.0, logical.0), scale(pixels.1, logical.1));
        let (crop, crop_path) = match image.crop {
            Some([x, y, width, height]) => {
                let crop = [
                    f64::from(x) * scale_x,
                    f64::from(y) * scale_y,
                    f64::from(width) * scale_x,
                    f64::from(height) * scale_y,
                ];
                let path = rectangle_path(crop[0], crop[1], crop[2], crop[3]);
                (crop, self.b.add(Value::Data(path)))
            }
            None => ([0.0, 0.0, pixels.0, pixels.1], null()),
        };

        let path = self.b.string(&relative_path);
        let snapshot = self.b.object(
            "GLSnapshot",
            vec![
                ("saveAsJPEG", Value::Boolean(kind == ImageKind::Jpeg)),
                ("imageIsMissing", Value::Boolean(false)),
                ("relativePath", path),
            ],
        );
        let fill_string = self.b.string(&color_string([0, 0, 0, 0]));
        let fill = self.b.color([0, 0, 0, 0]);
        let stroke = self.b.color([0, 0, 0, 255]);
        let stroke_string = self.b.string(&color_string([0, 0, 0, 255]));
        let empty_rect = self.b.string(&rect_string([0.0; 4]));
        let background = self.b.object(
            "ImageObject",
            vec![
                ("kImageObjectSnapshotKey", snapshot),
                ("fillColorCrossPlatform", fill_string),
                ("fillColor", fill),
                ("strokeAlpha", Value::Real(1.0)),
                ("strokeWidth", Value::Real(1.0)),
                ("strokeColor", stroke),
                ("fillAlpha", Value::Real(1.0)),
                ("strokeColorCrossPlatform", stroke_string),
                ("rect", empty_rect),
            ],
        );
        let primitives = self.b.mutable_array(Vec::new());
        let canvas_size = self.b.string("{0, 0}");
        let crop_rect = self.b.string(&rect_string(crop));
        let figure = self.b.object(
            "Figure",
            vec![
                ("kFigurePrimitiveObjectsArrayKey", primitives),
                ("FigureObjectTypeKey", integer(1)),
                ("FigureCanvasSizeKey", canvas_size),
                ("FigureCropRectKey", crop_rect),
                ("$0", crop_path),
                ("FigureBackgroundObjectKey", background),
            ],
        );

        let (mut fields, assets_id) = self.media_fields(frame, locked);
        fields.extend([
            ("isFlippedHorizontal", Value::Boolean(false)),
            ("isFlippedVertical", Value::Boolean(false)),
            ("isCaptionEnabled", Value::Boolean(false)),
            ("indexable", Value::Boolean(false)),
            ("figure", figure),
        ]);
        if image.kind == ImageKind::Gif {
            fields.push(("isGif", Value::Boolean(true)));
            self.files
                .push((format!("Assets/{assets_id}/media.gif"), image.bytes.clone()));
        }
        Ok(self.b.object("ImageMediaObject", fields))
    }

    fn text_box_object(&mut self, frame: &MediaFrame, text: &RichText) -> Result<Value> {
        let (mut fields, _) = self.canvas_fields(frame, None, None);
        let hash = empty_spatial_hash(&mut self.b, self.ids)?;
        let reflow = self.b.object("NBReflowStateReflowable", Vec::new());
        let pdf_files = self.b.array(Vec::new());
        let media = self.b.array(Vec::new());
        let store = self.formatted_string_fields(hash, reflow, text, pdf_files, media, Vec::new());
        let store = self.b.object("FormattedString", store);
        fields.push(("textStore", store));
        Ok(self.b.object("TextBlockMediaObject", fields))
    }

    /// Keys of a `CanvasMediaObject`: a sticky note, or the frame of a text box. Paper
    /// attributes are written for sticky notes only, whose values are known.
    fn canvas_fields(
        &mut self,
        frame: &MediaFrame,
        color: Option<[u8; 4]>,
        lines: Option<PaperLines>,
    ) -> (Vec<(&'static str, Value)>, String) {
        let (mut fields, assets_id) = self.media_fields(frame, false);
        let paper_color = match color {
            Some(color) => self.b.color(color),
            None => null(),
        };
        let style = self.b.object(
            "GLModel.NBPaperStyle",
            vec![
                ("paperColor", paper_color),
                ("lineStyle", integer(legacy_line_style(lines))),
                ("paperImageIndex", integer(-1)),
            ],
        );
        if let Some(color) = color {
            let identifier = format!("Color:#{:02X}{:02X}{:02X}", color[0], color[1], color[2]);
            let attributes = self.paper_attributes(
                &identifier,
                "portrait",
                "letter",
                "deviceBasedWidth",
                lines,
                true,
            );
            fields.push(("kCanvasMediaObjectPaperAttributes", attributes));
        }
        fields.extend([
            ("paperIndex", integer(-1)),
            ("paperStyleObject", style),
            ("lineStyle", integer(legacy_line_style(lines))),
        ]);
        (fields, assets_id)
    }

    /// `Images/Image .jpg`, `Images/Image 1.jpg`, ... numbered per extension.
    fn image_path(&mut self, kind: ImageKind) -> String {
        let extension = match kind {
            ImageKind::Png | ImageKind::Gif => "png",
            ImageKind::Jpeg => "jpg",
        };
        let count = self.image_counts.entry(extension).or_default();
        let name = if *count == 0 {
            format!("Images/Image .{extension}")
        } else {
            format!("Images/Image {count}.{extension}")
        };
        *count += 1;
        name
    }
}

/// `lineStyle2`: `No Lines`, or `<Lines|Dots|Grid>:<flag>:false:<inches>` with the
/// spacing in inches on an 8.5 in wide page. The first flag is set on sticky notes.
fn line_style_name(lines: Option<PaperLines>, sticky: bool) -> String {
    let Some(lines) = lines else {
        return "No Lines".to_owned();
    };
    let pattern = match lines.pattern {
        LinePattern::Ruled => "Lines",
        LinePattern::Dots => "Dots",
        LinePattern::Grid => "Grid",
    };
    let inches = format!("{:.7}", lines.spacing_inches);
    let inches = inches.trim_end_matches('0').trim_end_matches('.');
    format!("{pattern}:{sticky}:false:{inches}")
}

/// The older integer line style: 0 blank, 1 ruled, 9 dots. The grid value is not
/// known, and `lineStyle2` takes precedence.
fn legacy_line_style(lines: Option<PaperLines>) -> i64 {
    match lines.map(|lines| lines.pattern) {
        Some(LinePattern::Ruled) => 1,
        Some(LinePattern::Dots) => 9,
        Some(LinePattern::Grid) | None => 0,
    }
}

/// Locked-width pages of the legacy 21:16 shape are `letter`; other shapes are named
/// by their width-to-height ratio.
fn paper_size_name(aspect: f32) -> String {
    if (aspect - 21.0 / 16.0).abs() < 2e-3 {
        "letter".to_owned()
    } else if (aspect - std::f32::consts::SQRT_2).abs() < 2e-3 {
        "a4".to_owned()
    } else {
        format!("custom:{}", 1.0 / aspect)
    }
}

pub(super) fn number_string(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

fn point_string((x, y): (f32, f32)) -> String {
    format!(
        "{{{}, {}}}",
        number_string(f64::from(x)),
        number_string(f64::from(y))
    )
}

fn rect_string([x, y, width, height]: [f64; 4]) -> String {
    format!(
        "{{{{{}, {}}}, {{{}, {}}}}}",
        number_string(x),
        number_string(y),
        number_string(width),
        number_string(height)
    )
}

/// The `...CrossPlatform` color form: `r,g,b,a` with 15 decimals.
pub(super) fn color_string(rgba: [u8; 4]) -> String {
    rgba.iter()
        .map(|value| format!("{:.15}", f64::from(*value) / 255.0))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::{color_string, line_style_name, paper_size_name, rect_string};
    use crate::converter::note_writer::{LinePattern, PaperLines};

    #[test]
    fn formats_geometry_and_colors_like_notability() {
        assert_eq!(
            rect_string([0.0, 797.5, 2250.0, 1550.25]),
            "{{0, 797.5}, {2250, 1550.25}}"
        );
        assert_eq!(
            color_string([18, 3, 119, 255]),
            "0.070588235294118,0.011764705882353,0.466666666666667,1.000000000000000"
        );
    }

    #[test]
    fn names_paper_like_the_sample() {
        let ruled = |spacing_inches| {
            Some(PaperLines {
                pattern: LinePattern::Ruled,
                spacing_inches,
            })
        };
        assert_eq!(
            line_style_name(ruled(0.25), false),
            "Lines:false:false:0.25"
        );
        assert_eq!(
            line_style_name(ruled(0.196_850_5), true),
            "Lines:true:false:0.1968505"
        );
        assert_eq!(line_style_name(None, true), "No Lines");
        assert_eq!(paper_size_name(891.187_5 / 679.0), "letter");
    }
}
