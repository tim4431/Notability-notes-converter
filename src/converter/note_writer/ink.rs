//! Handwriting: the `InkedSpatialHash` arrays, shapes and ink groups.
//!
//! Curves are concatenated into parallel little-endian arrays. A stroke the eraser
//! split is several consecutive curves sharing one UUID; `curvesstyles`, `options`
//! and the `dashStyles` keys count strokes, every other array counts curves or
//! nodes. Strokes, shapes and groups share one z-order: `shapes.indices` and each
//! group's `index` are positions in it, and strokes fill the remaining positions.
use super::archive::{binary_plist, integer, ArchiveBuilder};
use super::{Curve, IdSource, InkObject, Node, Point, Shape, ShapeGeometry, Stroke};
use crate::Result;
use plist::{Dictionary, Value};

/// Path data in the CoreGraphics archive layout: a magic number, the element
/// count, one byte per element (0 move, 1 line) and float64 point pairs.
const PATH_MAGIC: [u8; 4] = [0x25, 0xb3, 0xe5, 0x48];

#[derive(Default)]
struct InkArrays {
    points: Vec<u8>,
    point_counts: Vec<u8>,
    widths: Vec<u8>,
    fractional_widths: Vec<u8>,
    forces: Vec<u8>,
    altitudes: Vec<u8>,
    azimuths: Vec<u8>,
    colors: Vec<u8>,
    uuids: Vec<u8>,
    styles: Vec<u8>,
    options: Vec<u8>,
    curve_count: usize,
    point_count: usize,
    node_count: usize,
    /// Stroke number and dash pattern of each dashed stroke.
    dashes: Vec<(usize, u8)>,
    stroke_count: usize,
}

impl InkArrays {
    fn push(&mut self, stroke: &Stroke, uuid: [u8; 16]) {
        for curve in stroke
            .curves
            .iter()
            .filter(|curve| !curve.points.is_empty())
        {
            push_curve(self, curve, stroke, uuid);
        }
        self.styles.push(stroke.style);
        self.options.extend(0u64.to_le_bytes());
        if let Some(dash) = stroke.dash {
            self.dashes.push((self.stroke_count, dash));
        }
        self.stroke_count += 1;
    }

    /// The keys an `InkedSpatialHash` and a group's `InkObjectEncoder` share.
    fn fields(self, b: &mut ArchiveBuilder) -> Result<Vec<(&'static str, Value)>> {
        let mut fields = vec![
            ("numpoints", b.add(integer(self.point_count as i64))),
            ("options", Value::Data(self.options)),
            (
                "numfractionalwidths",
                b.add(integer(self.node_count as i64)),
            ),
            ("curvesaltitudeangles", Value::Data(self.altitudes)),
            ("curvespoints", Value::Data(self.points)),
            ("curveUUIDs", Value::Data(self.uuids)),
            ("curvescolors", Value::Data(self.colors)),
            (
                "curvesfractionalwidths",
                Value::Data(self.fractional_widths),
            ),
            ("curvesnumpoints", Value::Data(self.point_counts)),
            ("curvesforces", Value::Data(self.forces)),
            ("curvesazimuthunitvector", Value::Data(self.azimuths)),
            ("curveswidth", Value::Data(self.widths)),
            ("curvesstyles", Value::Data(self.styles)),
            ("numcurves", b.add(integer(self.curve_count as i64))),
        ];
        if !self.dashes.is_empty() {
            let dashes = b.add(Value::Data(dash_styles(&self.dashes)?));
            fields.push(("dashStyles", dashes));
        }
        // A render cache keyed by curve number; Notability fills it on demand.
        fields.push(("bezierPathsDataDictionary", b.dictionary(true, Vec::new())));
        Ok(fields)
    }
}

fn push_curve(arrays: &mut InkArrays, curve: &Curve, stroke: &Stroke, uuid: [u8; 16]) {
    debug_assert_eq!(
        curve.points.len() % 3,
        1,
        "a curve is an anchor and cubic segments"
    );
    // One node per anchor, whatever the source held.
    let anchors = (curve.points.len() + 2) / 3;
    let last = curve.nodes.last().copied().unwrap_or(Node::UPRIGHT);
    let nodes = curve
        .nodes
        .iter()
        .copied()
        .chain(std::iter::repeat(last))
        .take(anchors);
    for (x, y) in &curve.points {
        arrays.points.extend(x.to_le_bytes());
        arrays.points.extend(y.to_le_bytes());
    }
    arrays
        .point_counts
        .extend((curve.points.len() as i32).to_le_bytes());
    for node in nodes {
        arrays
            .fractional_widths
            .extend(node.fractional_width.to_le_bytes());
        arrays.forces.extend(node.force.to_le_bytes());
        arrays.altitudes.extend(node.altitude.to_le_bytes());
        arrays.azimuths.extend(node.azimuth.0.to_le_bytes());
        arrays.azimuths.extend(node.azimuth.1.to_le_bytes());
    }
    arrays.widths.extend(stroke.width.to_le_bytes());
    arrays.colors.extend(stroke.rgba);
    arrays.uuids.extend(uuid);
    arrays.curve_count += 1;
    arrays.point_count += curve.points.len();
    arrays.node_count += anchors;
}

/// `richText['Handwriting Overlay'].SpatialHash`.
pub(super) fn spatial_hash(
    b: &mut ArchiveBuilder,
    objects: &[InkObject],
    ids: &mut IdSource,
) -> Result<Value> {
    let mut arrays = InkArrays::default();
    let mut shapes = ShapeList::default();
    let mut groups = Vec::new();
    // Empty objects would take a z-order position without writing anything.
    for (index, object) in objects
        .iter()
        .filter(|object| !is_empty(object))
        .enumerate()
    {
        match object {
            InkObject::Stroke(stroke) => arrays.push(stroke, ids.uuid()),
            InkObject::Shape(shape) => shapes.push(shape, index as i64, ids),
            InkObject::Group(members) => {
                let group = group_plist(members, index as i64, ids)?;
                groups.push(b.add(Value::Data(group)));
            }
        }
    }
    let has_dashes = !arrays.dashes.is_empty();
    let mut fields = arrays.fields(b)?;
    if !has_dashes {
        let dashes = b.add(Value::Data(dash_styles(&[])?));
        fields.push(("dashStyles", dashes));
    }
    let groups = b.mutable_array(groups);
    let shapes = b.add(Value::Data(binary_plist(shapes.into_value())?));
    fields.push(("groupsArrays", groups));
    fields.push(("shapes", shapes));
    Ok(b.object("InkedSpatialHash", fields))
}

fn is_empty(object: &InkObject) -> bool {
    match object {
        InkObject::Stroke(stroke) => stroke.curves.iter().all(|curve| curve.points.is_empty()),
        InkObject::Shape(_) => false,
        InkObject::Group(members) => members.iter().all(is_empty),
    }
}

/// The empty hash of a text box's own text store.
pub(super) fn empty_spatial_hash(b: &mut ArchiveBuilder, ids: &mut IdSource) -> Result<Value> {
    spatial_hash(b, &[], ids)
}

fn dash_styles(dashes: &[(usize, u8)]) -> Result<Vec<u8>> {
    let mut patterns = Dictionary::new();
    for (stroke, pattern) in dashes {
        let mut style = Dictionary::new();
        style.insert("pattern".to_owned(), integer(i64::from(*pattern)));
        patterns.insert(stroke.to_string(), Value::Dictionary(style));
    }
    let mut root = Dictionary::new();
    root.insert("objectPatterns".to_owned(), Value::Dictionary(patterns));
    binary_plist(Value::Dictionary(root))
}

#[derive(Default)]
struct ShapeList {
    shapes: Vec<Value>,
    indices: Vec<Value>,
    kinds: Vec<Value>,
}

impl ShapeList {
    fn push(&mut self, shape: &Shape, index: i64, ids: &mut IdSource) {
        let (kind, dictionary) = shape_dictionary(shape, ids.uuid());
        self.shapes.push(Value::Dictionary(dictionary));
        self.indices.push(integer(index));
        self.kinds.push(kind.into());
    }

    fn into_value(self) -> Value {
        let mut root = Dictionary::new();
        root.insert("shapes".to_owned(), Value::Array(self.shapes));
        root.insert("indices".to_owned(), Value::Array(self.indices));
        root.insert("kinds".to_owned(), Value::Array(self.kinds));
        Value::Dictionary(root)
    }
}

fn shape_dictionary(shape: &Shape, uuid: [u8; 16]) -> (&'static str, Dictionary) {
    let mut dictionary = Dictionary::new();
    let (kind, bounds) = match &shape.geometry {
        ShapeGeometry::Line {
            start,
            end,
            control,
        } => {
            dictionary.insert("startPt".to_owned(), point_value(*start));
            dictionary.insert("endPt".to_owned(), point_value(*end));
            let mut bounds = Bounds::of(&[*start, *end]);
            if let Some(control) = control {
                dictionary.insert("controlPoint1".to_owned(), point_value(*control));
                bounds.add_quadratic_extrema(*start, *control, *end);
            }
            ("line", bounds)
        }
        ShapeGeometry::Polygon(points) => {
            dictionary.insert(
                "points".to_owned(),
                Value::Array(points.iter().copied().map(point_value).collect()),
            );
            dictionary.insert("isClosed".to_owned(), Value::Boolean(true));
            (polygon_kind(points), Bounds::of(points))
        }
        ShapeGeometry::Ellipse(corners) => {
            let [top_left, top_right, bottom_right, bottom_left] = *corners;
            let mut rotated = Dictionary::new();
            rotated.insert(
                "corners".to_owned(),
                Value::Array(
                    [bottom_left, bottom_right, top_right, top_left]
                        .into_iter()
                        .map(point_value)
                        .collect(),
                ),
            );
            dictionary.insert("rotatedRect".to_owned(), Value::Dictionary(rotated));
            let width = distance(top_left, top_right);
            let height = distance(top_left, bottom_left);
            let kind = if (width - height).abs() <= 1e-3 * width.max(height) {
                "circle"
            } else {
                "ellipse"
            };
            (kind, Bounds::of_ellipse(corners))
        }
    };
    let pad = shape.width * 0.5;
    dictionary.insert(
        "rect".to_owned(),
        Value::Array(vec![
            point_value((bounds.min.0 - pad, bounds.min.1 - pad)),
            point_value((
                bounds.max.0 - bounds.min.0 + shape.width,
                bounds.max.1 - bounds.min.1 + shape.width,
            )),
        ]),
    );
    dictionary.insert(
        "appearance".to_owned(),
        Value::Dictionary(appearance(shape)),
    );
    dictionary.insert("options".to_owned(), integer(0));
    dictionary.insert("uuid".to_owned(), Value::Data(uuid.to_vec()));
    (kind, dictionary)
}

fn appearance(shape: &Shape) -> Dictionary {
    let mut color = Dictionary::new();
    color.insert(
        "rgba".to_owned(),
        Value::Array(
            shape
                .rgba
                .iter()
                .map(|value| Value::Real(f64::from(*value) / 255.0))
                .collect(),
        ),
    );
    let mut appearance = Dictionary::new();
    appearance.insert("style".to_owned(), integer(i64::from(shape.style)));
    appearance.insert("strokeColor".to_owned(), Value::Dictionary(color));
    appearance.insert("strokeWidth".to_owned(), Value::Real(shape.width));
    if let Some(pattern) = shape.dash {
        let mut dash = Dictionary::new();
        dash.insert("pattern".to_owned(), integer(i64::from(pattern)));
        appearance.insert("dashStyle".to_owned(), Value::Dictionary(dash));
    }
    if let Some(force) = shape.force {
        appearance.insert("force".to_owned(), Value::Real(force));
    }
    appearance
}

/// Notability names polygons by shape: three corners make a triangle, four right
/// angles a rectangle, or a square when the sides are equal too.
fn polygon_kind(points: &[Point]) -> &'static str {
    match points.len() {
        3 => "triangle",
        4 => {
            let right_angles = (0..4).all(|index| {
                let previous = points[(index + 3) % 4];
                let corner = points[index];
                let next = points[(index + 1) % 4];
                let a = (previous.0 - corner.0, previous.1 - corner.1);
                let b = (next.0 - corner.0, next.1 - corner.1);
                let lengths = a.0.hypot(a.1) * b.0.hypot(b.1);
                lengths > 0.0 && (a.0 * b.0 + a.1 * b.1).abs() < 1e-3 * lengths
            });
            if !right_angles {
                return "polygon";
            }
            let sides: Vec<f64> = (0..4)
                .map(|index| distance(points[index], points[(index + 1) % 4]))
                .collect();
            let longest = sides.iter().copied().fold(0.0, f64::max);
            let shortest = sides.iter().copied().fold(f64::INFINITY, f64::min);
            if longest - shortest <= 1e-3 * longest {
                "square"
            } else {
                "rectangle"
            }
        }
        _ => "polygon",
    }
}

/// One element of `groupsArrays`: a binary plist holding the group's members and its
/// z-order position. Members are written in document coordinates under an identity
/// transform.
fn group_plist(members: &[InkObject], index: i64, ids: &mut IdSource) -> Result<Vec<u8>> {
    let mut objects = Vec::new();
    for member in members.iter().filter(|member| !is_empty(member)) {
        let mut entry = Dictionary::new();
        match member {
            InkObject::Stroke(stroke) => {
                entry.insert("type".to_owned(), integer(1));
                entry.insert(
                    "object".to_owned(),
                    Value::Data(member_archive(stroke, ids)?),
                );
            }
            InkObject::Shape(shape) => {
                let mut list = ShapeList::default();
                list.push(shape, 0, ids);
                entry.insert("type".to_owned(), integer(2));
                entry.insert("object".to_owned(), list.into_value());
            }
            InkObject::Group(_) => continue,
        }
        objects.push(Value::Dictionary(entry));
    }
    let mut color = Dictionary::new();
    color.insert(
        "rgba".to_owned(),
        Value::Array(vec![0.0.into(), 0.0.into(), 0.0.into(), 1.0.into()]),
    );
    let mut appearance = Dictionary::new();
    appearance.insert("style".to_owned(), integer(3));
    appearance.insert("strokeColor".to_owned(), Value::Dictionary(color));
    appearance.insert("strokeWidth".to_owned(), Value::Real(0.0));
    let mut group = Dictionary::new();
    group.insert("inkGroupObjects".to_owned(), Value::Array(objects));
    group.insert(
        "transform".to_owned(),
        Value::Array([1.0, 0.0, 0.0, 1.0, 0.0, 0.0].map(Value::Real).to_vec()),
    );
    group.insert("isCustomSticker".to_owned(), Value::Boolean(false));
    group.insert("appearance".to_owned(), Value::Dictionary(appearance));
    group.insert("options".to_owned(), integer(0));
    group.insert("uuid".to_owned(), Value::Data(ids.uuid().to_vec()));
    let mut root = Dictionary::new();
    root.insert("inkGroup".to_owned(), Value::Dictionary(group));
    root.insert("index".to_owned(), integer(index));
    binary_plist(Value::Dictionary(root))
}

/// A group member stroke: a complete `NSKeyedArchiver` archive of one stroke.
fn member_archive(stroke: &Stroke, ids: &mut IdSource) -> Result<Vec<u8>> {
    let mut b = ArchiveBuilder::new("NSKeyedArchiver", "root");
    let root = b.reserve();
    let mut arrays = InkArrays::default();
    arrays.push(stroke, ids.uuid());
    let mut fields = arrays.fields(&mut b)?;
    let groups = b.mutable_array(Vec::new());
    fields.push(("groupsArrays", groups));
    b.fill_object(&root, "InkObjectEncoder", fields);
    b.finish(&root)
}

/// The closed rectangle path stored with a cropped image (`Figure.$0`).
pub(super) fn rectangle_path(x: f64, y: f64, width: f64, height: f64) -> Vec<u8> {
    let corners = [
        (x, y),
        (x + width, y),
        (x + width, y + height),
        (x, y + height),
        (x, y),
    ];
    let mut data = PATH_MAGIC.to_vec();
    data.extend((corners.len() as u32).to_le_bytes());
    data.extend([0, 1, 1, 1, 1]);
    for (px, py) in corners {
        data.extend(px.to_le_bytes());
        data.extend(py.to_le_bytes());
    }
    data
}

fn point_value((x, y): Point) -> Value {
    Value::Array(vec![Value::Real(x), Value::Real(y)])
}

fn distance(a: Point, b: Point) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

struct Bounds {
    min: Point,
    max: Point,
}

impl Bounds {
    fn of(points: &[Point]) -> Self {
        let mut bounds = Self {
            min: (f64::INFINITY, f64::INFINITY),
            max: (f64::NEG_INFINITY, f64::NEG_INFINITY),
        };
        for point in points {
            bounds.add(*point);
        }
        if points.is_empty() {
            bounds.min = (0.0, 0.0);
            bounds.max = (0.0, 0.0);
        }
        bounds
    }

    fn add(&mut self, (x, y): Point) {
        self.min = (self.min.0.min(x), self.min.1.min(y));
        self.max = (self.max.0.max(x), self.max.1.max(y));
    }

    /// A quadratic Bezier reaches beyond its end points where its derivative is zero.
    fn add_quadratic_extrema(&mut self, start: Point, control: Point, end: Point) {
        let axis = |a: f64, b: f64, c: f64| -> Option<f64> {
            let denominator = a - 2.0 * b + c;
            if denominator.abs() < f64::EPSILON {
                return None;
            }
            let t = (a - b) / denominator;
            (t > 0.0 && t < 1.0).then_some(t)
        };
        let at = |t: f64| {
            let u = 1.0 - t;
            (
                u * u * start.0 + 2.0 * u * t * control.0 + t * t * end.0,
                u * u * start.1 + 2.0 * u * t * control.1 + t * t * end.1,
            )
        };
        for t in [
            axis(start.0, control.0, end.0),
            axis(start.1, control.1, end.1),
        ]
        .into_iter()
        .flatten()
        {
            self.add(at(t));
        }
    }

    fn of_ellipse([top_left, top_right, _, bottom_left]: &[Point; 4]) -> Self {
        let u = (
            (top_right.0 - top_left.0) * 0.5,
            (top_right.1 - top_left.1) * 0.5,
        );
        let v = (
            (bottom_left.0 - top_left.0) * 0.5,
            (bottom_left.1 - top_left.1) * 0.5,
        );
        let center = (top_left.0 + u.0 + v.0, top_left.1 + u.1 + v.1);
        let extent = (u.0.hypot(v.0), u.1.hypot(v.1));
        Self {
            min: (center.0 - extent.0, center.1 - extent.1),
            max: (center.0 + extent.0, center.1 + extent.1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{polygon_kind, rectangle_path, Bounds};

    #[test]
    fn names_polygons_by_their_geometry() {
        let square = [(0.0, 0.0), (1.0, 1.0), (2.0, 0.0), (1.0, -1.0)];
        let rectangle = [(0.0, 0.0), (0.0, 2.0), (1.0, 2.0), (1.0, 0.0)];
        let kite = [(0.0, 0.0), (1.0, 2.0), (2.0, 0.0), (1.0, -1.0)];
        assert_eq!(polygon_kind(&square), "square");
        assert_eq!(polygon_kind(&rectangle), "rectangle");
        assert_eq!(polygon_kind(&kite), "polygon");
        assert_eq!(polygon_kind(&square[..3]), "triangle");
    }

    #[test]
    fn bounds_follow_rotated_ellipses_and_curved_lines() {
        // A circle of radius 1 around (1, 1), its frame rotated by 45 degrees.
        let r = std::f64::consts::FRAC_1_SQRT_2;
        let top_left = (1.0 - 2.0 * r, 1.0);
        let corners = [
            top_left,
            (1.0, 1.0 - 2.0 * r),
            (1.0 + 2.0 * r, 1.0),
            (1.0, 1.0 + 2.0 * r),
        ];
        let bounds = Bounds::of_ellipse(&corners);
        assert!((bounds.min.0 - 0.0).abs() < 1e-9 && (bounds.max.1 - 2.0).abs() < 1e-9);
        let mut line = Bounds::of(&[(0.0, 0.0), (2.0, 0.0)]);
        line.add_quadratic_extrema((0.0, 0.0), (1.0, 2.0), (2.0, 0.0));
        assert!((line.max.1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn crop_path_matches_notabilitys_layout() {
        let path = rectangle_path(0.0, 797.5, 2250.0, 1550.5);
        assert_eq!(path.len(), 93);
        assert_eq!(&path[..9], &[0x25, 0xb3, 0xe5, 0x48, 5, 0, 0, 0, 0]);
    }
}
