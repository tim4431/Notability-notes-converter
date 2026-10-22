//! Typed text: Notability's attributed string dictionaries.
//!
//! A `FormattedString` holds its text three times (`attributedString` and the two
//! copies of `NBAttributedBackingString`), each as `{stringKey, subRangesKey}` with
//! ranges in UTF-16 units, plus `recordingTimestampString` of the same length.
use super::archive::{integer, ArchiveBuilder};
use super::session::color_string;
use super::{IdSource, RichText};
use crate::converter::model::TextStyle;
use plist::Value;

pub(super) struct TextObjects {
    pub(super) attributed: Value,
    pub(super) backing: Value,
    pub(super) timestamps: Value,
}

pub(super) fn text_objects(
    b: &mut ArchiveBuilder,
    text: &RichText,
    ids: &mut IdSource,
) -> TextObjects {
    let runs = runs(text, ids);
    let plain = b.string(&text.text);
    let attributed = attributed_string(b, plain, &runs);
    let backing_string = b.mutable_string(&text.text);
    let backing_copy = attributed_string(b, backing_string, &runs);
    let layout_string = b.mutable_string(&text.text);
    let layout_copy = attributed_string(b, layout_string, &runs);
    let backing = b.object(
        "NBAttributedString",
        vec![
            ("NBAttributedBackingStringCodingKey", backing_copy),
            ("NBAttributedLayoutStringCodingKey", layout_copy),
        ],
    );
    let length = utf16_length(&text.text);
    let mut ranges = Vec::new();
    if length > 0 {
        let attributes = b.string_dictionary(true, Vec::new());
        let range = b.string(&range_string(0, length));
        ranges.push(b.string_dictionary(
            true,
            vec![
                ("subRangeOtherAttributesKey", attributes),
                ("subRangeRangeKey", range),
            ],
        ));
    }
    let timestamp_string = b.mutable_string(&text.text);
    let ranges = b.mutable_array(ranges);
    let timestamps = b.string_dictionary(
        true,
        vec![("stringKey", timestamp_string), ("subRangesKey", ranges)],
    );
    TextObjects {
        attributed,
        backing,
        timestamps,
    }
}

struct Run<'a> {
    location: usize,
    length: usize,
    style: &'a TextStyle,
    /// Identifies the list a paragraph belongs to.
    list_id: Option<i64>,
}

fn runs<'a>(text: &'a RichText, ids: &mut IdSource) -> Vec<Run<'a>> {
    let mut offsets = Vec::with_capacity(text.text.len() + 1);
    let mut units = 0;
    for ch in text.text.chars() {
        offsets.push(units);
        units += ch.len_utf16();
    }
    offsets.push(units);
    let offset = |index: usize| offsets[index.min(offsets.len() - 1)];

    let mut runs = Vec::new();
    let mut list: Option<(i64, i64)> = None;
    for span in &text.spans {
        let location = offset(span.start);
        let length = offset(span.start + span.length) - location;
        if length == 0 {
            continue;
        }
        // Consecutive list paragraphs of one decoration style form one list.
        let list_id = span.style.indent_decoration_style.map(|style| match list {
            Some((id, current)) if current == style => id,
            _ => {
                let id = i64::from(ids.next_u64() as u32);
                list = Some((id, style));
                id
            }
        });
        if list_id.is_none() {
            list = None;
        }
        runs.push(Run {
            location,
            length,
            style: &span.style,
            list_id,
        });
    }
    runs
}

fn attributed_string(b: &mut ArchiveBuilder, string: Value, runs: &[Run<'_>]) -> Value {
    let ranges = runs.iter().map(|run| sub_range(b, run)).collect();
    let ranges = b.mutable_array(ranges);
    b.string_dictionary(true, vec![("stringKey", string), ("subRangesKey", ranges)])
}

fn sub_range(b: &mut ArchiveBuilder, run: &Run<'_>) -> Value {
    let style = run.style;
    let color = b.string(&color_string(style.color));
    let range = b.string(&range_string(run.location, run.length));
    let size = b.scalar(Value::Real(f64::from(style.font_size)));
    let name = b.string(&font_postscript_name(
        &style.font_name,
        style.bold,
        style.italic,
    ));
    let font = b.string_dictionary(
        true,
        vec![("NSFontSizeAttribute", size), ("NSFontNameAttribute", name)],
    );
    let mut attributes = vec![
        (
            "NSStrikethrough",
            b.scalar(integer(i64::from(style.strikethrough))),
        ),
        (
            "line-spacing",
            b.scalar(Value::Real(f64::from(style.line_spacing_multiplier))),
        ),
        ("NSUnderline", b.scalar(integer(i64::from(style.underline)))),
        (
            "NSBaselineOffset",
            b.scalar(Value::Real(f64::from(style.baseline_offset))),
        ),
    ];
    if let Some(level) = style.indent_level {
        attributes.push(("indent-level", b.scalar(integer(level as i64))));
    }
    if let (Some(decoration), Some(list_id)) = (style.indent_decoration_style, run.list_id) {
        attributes.push(("indent-range-id", b.scalar(integer(list_id))));
        attributes.push(("indent-decoration-style", b.scalar(integer(decoration))));
        if style.indent_level.is_none() {
            attributes.push(("indent-level", b.scalar(integer(0))));
        }
        if let Some(number) = style.indent_decoration_number {
            attributes.push(("indent-decoration-number", b.scalar(integer(number))));
        }
        attributes.push((
            "checklist-checked",
            b.scalar(integer(i64::from(style.checklist_checked))),
        ));
    }
    let attributes = b.string_dictionary(true, attributes);
    let ui_color = b.color(style.color);
    b.string_dictionary(
        true,
        vec![
            ("subRangeColorCrossPlatformKey", color),
            ("subRangeRangeKey", range),
            ("subRangeFontKey", font),
            ("subRangeOtherAttributesKey", attributes),
            ("subRangeColorKey", ui_color),
        ],
    )
}

fn range_string(location: usize, length: usize) -> String {
    format!("{{{location}, {length}}}")
}

fn utf16_length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// PostScript names of the bundled iOS families: regular, bold, italic, bold italic.
/// A missing style falls back to the nearest one the family has.
const FONT_FAMILIES: &[(&str, [Option<&str>; 4])] = &[
    ("Al Nile", [Some("AlNile"), Some("AlNile-Bold"), None, None]),
    (
        "American Typewriter",
        [
            Some("AmericanTypewriter"),
            Some("AmericanTypewriter-Bold"),
            None,
            None,
        ],
    ),
    (
        "Arial",
        [
            Some("ArialMT"),
            Some("Arial-BoldMT"),
            Some("Arial-ItalicMT"),
            Some("Arial-BoldItalicMT"),
        ],
    ),
    (
        "Avenir",
        [
            Some("Avenir-Book"),
            Some("Avenir-Heavy"),
            Some("Avenir-BookOblique"),
            Some("Avenir-HeavyOblique"),
        ],
    ),
    (
        "Avenir Next",
        [
            Some("AvenirNext-Regular"),
            Some("AvenirNext-Bold"),
            Some("AvenirNext-Italic"),
            Some("AvenirNext-BoldItalic"),
        ],
    ),
    (
        "Baskerville",
        [
            Some("Baskerville"),
            Some("Baskerville-Bold"),
            Some("Baskerville-Italic"),
            Some("Baskerville-BoldItalic"),
        ],
    ),
    (
        "Chalkboard SE",
        [
            Some("ChalkboardSE-Regular"),
            Some("ChalkboardSE-Bold"),
            None,
            None,
        ],
    ),
    ("Chalkduster", [Some("Chalkduster"), None, None, None]),
    (
        "Charter",
        [
            Some("Charter-Roman"),
            Some("Charter-Bold"),
            Some("Charter-Italic"),
            Some("Charter-BoldItalic"),
        ],
    ),
    (
        "Cochin",
        [
            Some("Cochin"),
            Some("Cochin-Bold"),
            Some("Cochin-Italic"),
            Some("Cochin-BoldItalic"),
        ],
    ),
    (
        "Copperplate",
        [Some("Copperplate"), Some("Copperplate-Bold"), None, None],
    ),
    (
        "Courier",
        [
            Some("Courier"),
            Some("Courier-Bold"),
            Some("Courier-Oblique"),
            Some("Courier-BoldOblique"),
        ],
    ),
    (
        "Courier New",
        [
            Some("CourierNewPSMT"),
            Some("CourierNewPS-BoldMT"),
            Some("CourierNewPS-ItalicMT"),
            Some("CourierNewPS-BoldItalicMT"),
        ],
    ),
    (
        "Didot",
        [
            Some("Didot"),
            Some("Didot-Bold"),
            Some("Didot-Italic"),
            None,
        ],
    ),
    (
        "Futura",
        [
            Some("Futura-Medium"),
            Some("Futura-Bold"),
            Some("Futura-MediumItalic"),
            None,
        ],
    ),
    (
        "Georgia",
        [
            Some("Georgia"),
            Some("Georgia-Bold"),
            Some("Georgia-Italic"),
            Some("Georgia-BoldItalic"),
        ],
    ),
    (
        "Gill Sans",
        [
            Some("GillSans"),
            Some("GillSans-Bold"),
            Some("GillSans-Italic"),
            Some("GillSans-BoldItalic"),
        ],
    ),
    (
        "Helvetica",
        [
            Some("Helvetica"),
            Some("Helvetica-Bold"),
            Some("Helvetica-Oblique"),
            Some("Helvetica-BoldOblique"),
        ],
    ),
    (
        "Helvetica Neue",
        [
            Some("HelveticaNeue"),
            Some("HelveticaNeue-Bold"),
            Some("HelveticaNeue-Italic"),
            Some("HelveticaNeue-BoldItalic"),
        ],
    ),
    (
        "Hoefler Text",
        [
            Some("HoeflerText-Regular"),
            Some("HoeflerText-Black"),
            Some("HoeflerText-Italic"),
            Some("HoeflerText-BlackItalic"),
        ],
    ),
    ("Impact", [Some("Impact"), None, None, None]),
    (
        "Marker Felt",
        [Some("MarkerFelt-Thin"), Some("MarkerFelt-Wide"), None, None],
    ),
    (
        "Menlo",
        [
            Some("Menlo-Regular"),
            Some("Menlo-Bold"),
            Some("Menlo-Italic"),
            Some("Menlo-BoldItalic"),
        ],
    ),
    (
        "Noteworthy",
        [
            Some("Noteworthy-Light"),
            Some("Noteworthy-Bold"),
            None,
            None,
        ],
    ),
    (
        "Optima",
        [
            Some("Optima-Regular"),
            Some("Optima-Bold"),
            Some("Optima-Italic"),
            Some("Optima-BoldItalic"),
        ],
    ),
    (
        "Palatino",
        [
            Some("Palatino-Roman"),
            Some("Palatino-Bold"),
            Some("Palatino-Italic"),
            Some("Palatino-BoldItalic"),
        ],
    ),
    ("Papyrus", [Some("Papyrus"), None, None, None]),
    (
        "Snell Roundhand",
        [
            Some("SnellRoundhand"),
            Some("SnellRoundhand-Bold"),
            None,
            None,
        ],
    ),
    (
        "Times New Roman",
        [
            Some("TimesNewRomanPSMT"),
            Some("TimesNewRomanPS-BoldMT"),
            Some("TimesNewRomanPS-ItalicMT"),
            Some("TimesNewRomanPS-BoldItalicMT"),
        ],
    ),
    (
        "Trebuchet MS",
        [
            Some("TrebuchetMS"),
            Some("TrebuchetMS-Bold"),
            Some("TrebuchetMS-Italic"),
            Some("Trebuchet-BoldItalic"),
        ],
    ),
    (
        "Verdana",
        [
            Some("Verdana"),
            Some("Verdana-Bold"),
            Some("Verdana-Italic"),
            Some("Verdana-BoldItalic"),
        ],
    ),
];

/// `.note` text names a font by its PostScript name; `.ntb` text stores the family
/// with separate bold and italic flags.
pub(super) fn font_postscript_name(family: &str, bold: bool, italic: bool) -> String {
    let wanted = usize::from(bold) + 2 * usize::from(italic);
    if let Some((_, names)) = FONT_FAMILIES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(family.trim()))
    {
        // Nearest style first: the exact one, then dropping italic, then bold.
        let order = [wanted, wanted & 1, wanted & 2, 0];
        if let Some(name) = order.iter().find_map(|&index| names[index]) {
            return name.to_owned();
        }
    }
    // Unknown families: the usual `Family-Style` naming, or an existing PostScript
    // name, which has no spaces.
    let base: String = family.chars().filter(|ch| !ch.is_whitespace()).collect();
    let base = if base.is_empty() {
        "HelveticaNeue".to_owned()
    } else {
        base
    };
    if base.contains('-') {
        return base;
    }
    match (bold, italic) {
        (true, true) => format!("{base}-BoldItalic"),
        (true, false) => format!("{base}-Bold"),
        (false, true) => format!("{base}-Italic"),
        (false, false) => base,
    }
}

#[cfg(test)]
mod tests {
    use super::font_postscript_name;

    #[test]
    fn maps_families_to_the_names_in_the_sample_note() {
        for (family, bold, italic, expected) in [
            ("Helvetica Neue", false, false, "HelveticaNeue"),
            ("Helvetica Neue", false, true, "HelveticaNeue-Italic"),
            ("Helvetica Neue", true, false, "HelveticaNeue-Bold"),
            (
                "American Typewriter",
                true,
                false,
                "AmericanTypewriter-Bold",
            ),
            ("Baskerville", true, false, "Baskerville-Bold"),
            ("Impact", true, false, "Impact"),
            ("Al Nile", false, false, "AlNile"),
            ("Chalkduster", false, false, "Chalkduster"),
            ("Some Family", true, true, "SomeFamily-BoldItalic"),
        ] {
            assert_eq!(font_postscript_name(family, bold, italic), expected);
        }
    }
}
