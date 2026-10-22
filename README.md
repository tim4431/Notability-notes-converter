# Notability Notes Converter

Converts `.note` and `.ntb` files to `.pdf` by default, or optionally to `.svg`. It also converts `.ntb` files to `.note` packages.

**Online converter:** [tim4431.github.io/Notability-notes-converter](https://tim4431.github.io/Notability-notes-converter/) runs the same converter in your browser, compiled to WebAssembly. Files are converted on your device and never uploaded.

I originally made this for personal use because I couldn't find a converter online. I hope it is useful to others as well.

Note: output is not perfectly identical to Notability’s official export. Some details, such as Apple-specific fonts and exact text layout are approximated, but for most notes this converter works pretty well.

## Where is this useful?

- Convert `.note` and `.ntb` files to other formats in bulk (high speed is a plus)
- View `.note` and `.ntb` files on a machine that does not have Notability installed
- Switch platforms by converting notes to `.svg` (SVGs can be imported into other note-taking apps as editable files)
- Turn `.ntb` bundles into `.note` packages, with strokes, shapes and text kept editable

## How to use

Requirement: [Rust](https://rust-lang.org/tools/install/)

SVG output with imported PDF backgrounds also needs Poppler's `pdftoppm` in `PATH`. PDF output does not need Poppler.

Put your exported `.note` and `.ntb` files in `input/`, then run this from the repository root:

```bash
cargo run
```

This converts everything under `input/` into `output/`.
Nested folder structures are preserved.

Input and output folders must be separate and must not contain each other. Files are converted in a temporary folder first. A collision or conversion failure leaves existing output unchanged. When output is replaced successfully, the previous folder is kept in a sibling backup folder. Its location is printed after conversion.

Example:

```text
.
├── input/
│   ├── example1.note
│   └── example_folder/
│       └── example2.ntb
└── output/
    ├── example1.pdf
    └── example_folder/
        └── example2.pdf
```

## Optional flags: format and input/output folders

```bash
cargo run -- --format svg --input path/to/input --output path/to/output
```

`--format` is `pdf` (the default), `svg` or `note`. With `note`, `.ntb` files are converted to `.note` packages and `.note` files are copied unchanged:

```bash
cargo run -- --format note
```

## Building the online converter

The browser version lives in `web/`: `web/src` exposes the converter to JavaScript, and `web/site` is the page, which converts files in a web worker. It needs Rust's WebAssembly target and [wasm-bindgen-cli](https://github.com/wasm-bindgen/wasm-bindgen) at the version of the `wasm-bindgen` crate (0.2.118):

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.118
web/build.sh
```

This writes static files to `web/dist`; serve that folder over HTTP to try it, for example with `python3 -m http.server --directory web/dist`. The GitHub Actions workflow in `.github/workflows/pages.yml` runs the tests, builds the site and publishes it to GitHub Pages on every push to `master`.

## Analysis of the `.note` file format

Thanks to [Julia Evans' blog](https://jvns.ca/blog/2018/03/31/reverse-engineering-notability-format/), I was able to quickly understand how the file format works.

A `.note` file is a ZIP archive with a document folder inside it. The folder name does not have to match the file name, so locate `Session.plist` first and treat its parent folder as the bundle root. Typical contents are:

- `Session.plist`: the main document object graph
- `Images/`: embedded image files referenced by figure objects
- `PDFs/`: imported PDF files used as page backgrounds
- `thumb*.png`: thumbnails, useful as a fallback for page aspect ratio
- `metadata.plist`, `Recordings/`, and similar side files

`Session.plist` is an Apple keyed archive, not a plain property-list tree. The top-level plist contains `$objects` and `$top`. Values are often `UID` references into `$objects`, so a reader needs to resolve UIDs before interpreting fields. Archived arrays and dictionaries often appear as Objective-C shapes such as `NS.objects`, `NS.keys`, and `NS.bytes`.

Most useful page and drawing data is under `richText`.

Geometry fields define how document coordinates become pages:

- `richText -> reflowState -> pageWidthInDocumentCoordsKey`: document-space page width
- paper attributes: paper size, line style, and page sizing hints
- `pageLayoutArray`: imported PDF pages, whose page boxes can define the output ratio
- `thumb*.png`: fallback aspect ratio when no source PDF is available

Notability stores content in one continuous vertical document coordinate space. To convert it to pages, choose a page height from the geometry above, then split any item by:

```text
page_index = floor(document_y / page_height)
local_y = document_y - page_index * page_height
```

A stroke can cross page boundaries. Keep its full path and width data on every page it reaches, then clip to each page. For cubic curves, use the curve's extrema rather than its control points to find the page range.

Text appears in two forms:

- `richText -> attributedString`: the main text stream
- `TextBlockMediaObject` entries in `mediaObjects`: positioned text boxes

The attributed string stores plain text in `stringKey` and style runs in `subRangesKey`. Useful style fields include:

- `NSFontSizeAttribute` and `NSFontNameAttribute`
- `subRangeColorCrossPlatformKey` or `subRangeColorKey`
- underline, strikethrough, and baseline offset in `subRangeOtherAttributesKey`
- list/checklist metadata such as `indent-level`, `indent-decoration-style`, `indent-decoration-number`, and `checklist-checked`

To convert text, reconstruct a per-character style map from the ranges, then lay out the text in document coordinates. This is the least exact part of the format because Notability's native font shaping and wrapping are not stored as final glyph positions.

Images are `mediaObjects` with nested figure/background/snapshot objects pointing to files in `Images/`. Important fields are:

- `documentOrigin`: document-space position
- `unscaledContentSize`: displayed size
- `rotationDegrees`: rotation; some files store radians despite the name
- `isFlippedHorizontal` and `isFlippedVertical`: independent mirror flags
- `FigureCropRectKey`: crop rectangle in source-image pixels
- `zIndex`: layer order

To convert images, read the referenced asset from `Images/`, crop it if `FigureCropRectKey` exists, then place it at `documentOrigin` with size, rotation, and flips applied around the displayed image center. If a referenced RGB JPEG is uncropped or the crop covers the entire source image, it can be embedded directly in the output PDF instead of being decoded and re-encoded.

Imported PDF backgrounds are described by `richText -> pageLayoutArray`. Each entry maps:

- note page index
- source PDF filename under `PDFs/`
- source page index inside that PDF

To convert a PDF-backed page, use the source PDF page as the base layer, scale it to the reconstructed output page box, then draw the note overlays above it.

Handwriting is stored under `richText -> Handwriting Overlay -> SpatialHash`. The main stroke payloads are little-endian binary arrays:

- `curvespoints`: `f32` x/y point pairs
- `curvesnumpoints`: `i32` point counts per curve
- `curveswidth`: base stroke widths as `f32`
- `curvesforces`: pressure samples as `f32`
- `curvesfractionalwidths`: width multipliers as `f32`
- `curvescolors`: raw RGBA bytes
- `curvesstyles`: tool/style ids, one per stroke
- `curveUUIDs`: one UUID per curve; the pieces of an erased stroke share it
- `dashStyles`: dash/dot pattern metadata keyed by stroke index
- `groupsArrays`: nested stroke groups with transforms
- `shapes`: simple shape strokes

Many curves are cubic Bezier chains: the point count follows `1 + 3n`, where each segment uses one start point and three more points. Pressure and fractional-width arrays may align to the Bezier sample count rather than the raw point count. A converter should preserve that pressure data for pen and pencil width/opacity.

Polygon vertices can also have a `1 + 3n` point count. Keep them as straight edges, including when drawing with pencil or variable width. Small dots and short pen marks should not be discarded just because they are thin or dark.

A stroke that the eraser split is stored as several consecutive curves with the same UUID. `curvesstyles`, `options` and the keys of `dashStyles` count these strokes, not curves: the example note has 596 curves but 149 styles. Notes that store one style per curve, or no UUIDs, count curves.

Dash and dot patterns live in `dashStyles -> objectPatterns -> <stroke-index> -> pattern`. Keep the original stroke index when attaching dash metadata; filtering or reordering curves too early can assign dotted styles to the wrong stroke.

Grouped handwriting in `groupsArrays` has its own nested archive data plus a transform. Apply the group transform to stroke coordinates. Scale stroke width by the transform as well, otherwise grouped strokes render too thick or too thin.

Shape strokes are separate from normal raw curves:

- `square`, `triangle`, and `polygon` use explicit point lists
- `circle` uses `rotatedRect` or `rect`
- `line` uses `startPt` and `endPt`; curved lines also use `controlPoint1` / `controlPoint2`
- dashed/dotted shapes use `appearance -> dashStyle -> pattern`
- `partialshape` represents partially erased shape/line fragments; use its `strokePath` outline as a filled path to preserve the erased gap size

Layering is reconstructed from page backgrounds first, then text, text blocks, images, and handwriting according to the parsed order and z-order where available.

Important limitations when converting:

- text layout is heuristic because exact Notability glyph shaping is not fully encoded
- raw partial erase behavior is only partly understood, but `partialshape` fragments preserve erased size for partially erased shapes/lines
- highlighter dotted/dashed strokes can differ from Notability's exported PDF because the export may use filled fragments instead of native dash arrays
- image rotation, crop, and horizontal/vertical flips are separate fields and all must be applied

## Analysis of the `.ntb` file format

A `.ntb` file is a more recent file format by Notability. It is also a ZIP archive, but its contents are different from a `.note` file. The main entries are:

- `noteBundle`: a FlatBuffers event collection containing the note data
- `assets/`: embedded media named with a SHA-512 content hash and a file extension

The `noteBundle` stores a history of events instead of one final document object graph. Each event has an id, a type, and a payload. These event types are currently understood:

- type 1: document and paper settings
- type 7: text boundary anchors
- type 8: text
- type 12: character style ranges
- type 13: paragraph and list style ranges
- type 15: ink
- type 18: shapes
- type 20: lists of related event ids
- type 22: images and sticky notes

Text and style data are separate events. Text events refer to their container by event id. Character and paragraph styles refer to text event ids and UTF-16 offsets. Resolve these references before applying styles, especially when the note has multiple text boxes. Type 7 events can provide end anchors for text ranges.

Positioned text boxes store a page, position, size, and rotation. The converter keeps their text and styles separate from the main document text. It also preserves main text that continues onto later pages.

Ink paths use compact binary data with half-precision or single-precision values. The payload also stores the page, origin, scale, rotation, width, color, tool, and line style. The scale applies to both the path coordinates and the stroke width.

Cubic path records also contain an opacity byte. Pencil opacity varies between records, so keep it with each segment when splitting erased fragments. Use the stored opacity rather than estimating it from pressure.

Shapes are separate events. Lines can have a control point, polygons use point lists, and ellipses use a bounding size. Shape events also store page, origin, rotation, width, color, tool, and dash style.

Transformed groups store rotation on each child event. Apply it to ink and shapes around each event origin. Line style 2 is dashed and line style 3 is dotted.

Partial eraser and shape eraser results are stored as breaks inside an ink path. In cubic paths, command 0 continues the curve and command 3 moves to the next fragment. In linear paths, command 6 continues the line and command 7 moves to the next fragment. A move command must start a new path. Drawing a line across it fills the erased area again.

Type 20 events contain lists of related event ids. They do not act as eraser masks, so their referenced ink and shapes should still be rendered.

Images refer to files under `assets/` by their content hash. Their events store a MIME type, logical asset size, page, position, displayed size, rotation in radians, and an optional crop rectangle. Crop values use logical asset coordinates and do not always match the pixel dimensions of the stored image.

For a bundle with one PDF asset under `assets/`, the converter uses all source pages in their original order as backgrounds, including pages without handwriting. PDF output preserves the source page content, crop, and rotation. SVG output embeds rendered backgrounds while keeping note strokes as SVG paths.

Multiple PDF assets and reordered source pages still need explicit page mapping. The converter reports an error for multiple PDF assets rather than choosing one. The supplied example has no PDF backgrounds or positioned text boxes, so these paths are tested with generated fixtures.

The main differences from `.note` are:

- `.note` uses an Apple keyed archive in `Session.plist`; `.ntb` uses FlatBuffers in `noteBundle`
- `.note` mostly stores the current document state; `.ntb` stores an ordered event collection with event references
- `.note` uses one continuous vertical coordinate space; `.ntb` events contain explicit page references
- `.note` media uses paths under `Images/`; `.ntb` media uses content hashes under `assets/`
- `.note` handwriting uses parallel binary arrays; `.ntb` stores a compact path inside each ink event

## Converting `.ntb` to `.note`

`--format note` writes the package that Notability 16 saves (`sessionFormatVersion` 9): `Session.plist`, `metadata.plist`, `thumb12x.png`, `Recordings/library.plist` and the media under `Images/`, `Assets/` and `PDFs/`, stored uncompressed under one folder named after the note.

The example note in `input/` exists in both formats, which made the mapping checkable. Converting its `.ntb` gives back the strokes, shapes, groups, images, sticky notes and text of the `.note` copy, within the half-float precision of the bundle. Every archived object has the class names, keys and value encodings that Notability wrote.

How the bundle maps to the package:

- Page coordinates become `.note` document coordinates: x − `W / 38.4`, y + the top of the page. Plain pages are the bundle's page height apart; a PDF page takes `ceil(W × h / w)` of the strip.
- An ink event becomes one stroke, and the pieces left by the eraser become consecutive curves sharing one UUID.
- A cubic node holds the width multiplier and force as half floats, then the pencil altitude (`byte / 255 × π/2`) and azimuth (`byte / 256` of a turn). The byte the renderer reads as pencil opacity is this altitude. Nodes become `curvesfractionalwidths`, `curvesforces`, `curvesaltitudeangles` and `curvesazimuthunitvector`.
- Tool 0 (pen) is style 3, tool 1 (pencil) style 5 and tool 2 (highlighter) style 4. Line styles 2 and 3 (dashed, dotted) are dash patterns 1 and 2.
- Strokes, shapes and groups share one z-order, the order of their events. `shapes.indices` and each group's `index` are positions in it, and strokes fill the other positions.
- Type 20 events list the members of a group. The members are written in document coordinates under an identity transform. Type 25 events list erased records, which are left out.
- Shapes stay editable: lines with an optional quadratic control point, polygons named `triangle`, `square`, `rectangle` or `polygon` by their geometry, and ellipses (`circle` or `ellipse`) by their rotated frame. `rect` is the bounding box grown by half the stroke width. Pencil shapes keep their force.
- Images keep their crop (with the crop path in `Figure.$0`), rotation, z-order and lock. A GIF keeps its animation in `Assets/<id>/media.gif` and shows a still of its first frame at the logical size.
- Text keeps its styles, with fonts turned from a family and bold/italic flags into PostScript names, and ranges counted in UTF-16 units. Line spacing is stored in inches of an 8.5 in wide page.

Limitations:

- The output has not been opened in Notability yet. It mirrors Notability's own output field for field, but only the app can confirm that it imports.
- Partially erased shapes are written as strokes with straight segments rather than as `partialshape` outlines.
- `.ntb` bundles do not store image flips, so the converted images are not flipped.
- Bundles with several PDFs are refused, as for PDF output. Text boxes are written without `kCanvasMediaObjectPaperAttributes`, whose values are known only for sticky notes.
- `bezierPathsDataDictionary`, a render cache, is left empty, as Notability 15 writes it.
