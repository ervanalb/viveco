//! SVG shape import.
//!
//! This module is enabled by the crate's `svg` feature. It converts the
//! drawable parts of an SVG document into an [`Asset`] of [`Shape`]s:
//!
//! - `path`, `rect`, `circle`, `ellipse`, `line`, `polyline`, and `polygon`
//!   elements become paths, including arcs and quadratic Béziers.
//! - Groups that clip (`clip-path`) become parent shapes whose own paths are
//!   the clip geometry, so their children are clipped exactly as in the SVG.
//!   When the clip geometry is provided by one of the group's own shapes (the
//!   usual Inkscape pattern, where a "Clip" group is also part of the art),
//!   that shape becomes the parent instead, keeping its name and paint. Every
//!   other group is flattened into its parent, because the state it carries
//!   (transform, opacity) is already folded into its contents. Element IDs are
//!   used as shape names.
//! - Transforms, `viewBox`, style inheritance, CSS presentation attributes,
//!   `use`, and `switch` are resolved by [`usvg`] before conversion.
//! - Solid fills and strokes are imported with their alpha. Group and fill
//!   opacity are folded into each shape's color, so an opacity group is
//!   approximated shape by shape rather than composited as a whole. Gradients
//!   are approximated with a single representative stop color, because the
//!   Viveco model only stores solid colors.
//! - A top-level group (a `<g>` directly inside the root `<svg>`) can be
//!   selected by name, so a document whose top-level groups hold separate
//!   pieces of art can be imported one group at a time. A group's name is its
//!   `inkscape:label` when it has one (as Inkscape layers do) and its `id`
//!   otherwise; both are accepted when both exist.
//!
//! Only artwork is imported. Bones, animations, text, and embedded images are
//! not part of the result, and SVG features the model cannot represent (masks,
//! filters, patterns, nested clip intersections, and the even-odd fill rule)
//! are ignored.
//!
//! All coordinates are in the SVG's user space after `viewBox` scaling, which
//! matches the asset model's top-left origin and downward Y axis.

use std::collections::HashMap;
use std::fmt;

use usvg::tiny_skia_path::PathSegment;

use crate::{Asset, Path, Point, Rgba8, Segment, Shape, StrokeStyle, Style};

/// Namespace of SVG elements.
const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// Namespace of the `xlink:href` attribute.
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";

/// Namespace of Inkscape's own attributes, such as `inkscape:label`.
const INKSCAPE_NS: &str = "http://www.inkscape.org/namespaces/inkscape";

/// An error loading shapes from an SVG document.
#[derive(Debug)]
pub enum SvgImportError {
    /// Reading the SVG file failed.
    ///
    /// Only [`Asset::load_svg_from_file`] produces this variant.
    Io(std::io::Error),
    /// The contents were not a valid SVG document.
    Parse(String),
    /// The requested top-level group does not exist in the document.
    UnknownGroup {
        /// The name that was asked for.
        name: String,
        /// The names of every named top-level group in the document, in
        /// document order.
        available: Vec<String>,
    },
}

impl fmt::Display for SvgImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "could not read SVG: {err}"),
            Self::Parse(err) => write!(f, "invalid SVG: {err}"),
            Self::UnknownGroup { name, available } if available.is_empty() => {
                write!(
                    f,
                    "no top-level SVG group named `{name}`; the document has no named top-level groups"
                )
            }
            Self::UnknownGroup { name, available } => {
                write!(
                    f,
                    "no top-level SVG group named `{name}`; available groups: {}",
                    available.join(", ")
                )
            }
        }
    }
}

impl std::error::Error for SvgImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Parse(_) | Self::UnknownGroup { .. } => None,
        }
    }
}

impl From<std::io::Error> for SvgImportError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<usvg::Error> for SvgImportError {
    fn from(err: usvg::Error) -> Self {
        Self::Parse(err.to_string())
    }
}

/// Options controlling which parts of an SVG document are imported.
///
/// The default imports every shape in the document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SvgImportOptions {
    /// Imports only the objects in the top-level group with this name.
    ///
    /// A top-level group is a `<g>` directly inside the root `<svg>`. Its name
    /// is its `inkscape:label` when present (as on Inkscape layers) and its
    /// `id` otherwise; both are accepted when both exist. Everything inside
    /// the selected group is imported. When no group matches, import fails
    /// with [`SvgImportError::UnknownGroup`].
    pub group: Option<String>,
}

impl SvgImportOptions {
    /// Options that import every shape in the document.
    pub fn new() -> Self {
        Self::default()
    }

    /// Restricts the import to the top-level group with the given name.
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }
}

impl Asset {
    /// Imports the shapes from an SVG document.
    ///
    /// The result has no bones or animations. See the [module
    /// documentation](self) for which SVG features are imported.
    pub fn from_svg_str(text: &str) -> Result<Self, SvgImportError> {
        Self::from_svg_str_with_options(text, &SvgImportOptions::default())
    }

    /// Imports the shapes selected by `import_options` from an SVG document.
    pub fn from_svg_str_with_options(
        text: &str,
        import_options: &SvgImportOptions,
    ) -> Result<Self, SvgImportError> {
        let tree = usvg::Tree::from_str(text, &options())?;
        let uses = clip_uses(text);
        let groups = svg_groups(text);
        Self::from_svg_tree(&tree, &uses, &groups, import_options)
    }

    /// Imports the shapes from SVG bytes, which may also be gzip-compressed
    /// (`svgz`).
    pub fn from_svg_bytes(bytes: &[u8]) -> Result<Self, SvgImportError> {
        Self::from_svg_bytes_with_options(bytes, &SvgImportOptions::default())
    }

    /// Imports the shapes selected by `import_options` from SVG bytes, which
    /// may also be gzip-compressed (`svgz`).
    pub fn from_svg_bytes_with_options(
        bytes: &[u8],
        import_options: &SvgImportOptions,
    ) -> Result<Self, SvgImportError> {
        let bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
            usvg::decompress_svgz(bytes)?
        } else {
            bytes.to_vec()
        };
        let text =
            std::str::from_utf8(&bytes).map_err(|err| SvgImportError::Parse(err.to_string()))?;
        Self::from_svg_str_with_options(text, import_options)
    }

    /// Reads an SVG document and imports its shapes.
    pub fn load_svg_from_file(path: impl AsRef<std::path::Path>) -> Result<Self, SvgImportError> {
        Self::from_svg_bytes(&std::fs::read(path)?)
    }

    /// Reads an SVG document and imports the shapes selected by
    /// `import_options`.
    pub fn load_svg_from_file_with_options(
        path: impl AsRef<std::path::Path>,
        import_options: &SvgImportOptions,
    ) -> Result<Self, SvgImportError> {
        Self::from_svg_bytes_with_options(&std::fs::read(path)?, import_options)
    }

    fn from_svg_tree(
        tree: &usvg::Tree,
        uses: &ClipUses,
        groups: &[SvgGroup],
        import_options: &SvgImportOptions,
    ) -> Result<Self, SvgImportError> {
        // A referenced group's geometry is relative to its parent, so remember
        // each rendered node's parent transform for clip recovery.
        let mut parents = HashMap::new();
        collect_parent_transforms(tree.root(), &mut parents);

        let resolver = ClipResolver {
            tree,
            uses,
            parents: &parents,
        };

        let shapes = match &import_options.group {
            None => shapes_from_children(tree.root(), 1.0, &resolver),
            Some(name) => {
                let group = groups
                    .iter()
                    .find(|group| group.matches(name))
                    .ok_or_else(|| SvgImportError::UnknownGroup {
                        name: name.clone(),
                        available: groups
                            .iter()
                            .map(|group| group.display_name().to_owned())
                            .collect(),
                    })?;
                let mut shapes = Vec::new();
                // Importing the group node preserves its opacity and clip path
                // while flattening it otherwise, just like any other group in
                // the document.
                if let Some(node) = tree.node_by_id(&group.id) {
                    append_node(node, 1.0, &mut shapes, &resolver);
                }
                reset_z(&mut shapes);
                shapes
            }
        };

        Ok(Self {
            shapes,
            bones: Vec::new(),
            animations: Vec::new(),
        })
    }
}

/// Parsing options for shape import.
///
/// Images are not imported, so their references are not resolved either. This
/// keeps importing a document from reading unrelated files next to it.
fn options() -> usvg::Options<'static> {
    usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    }
}

/// A `<use>` element inside a `<clipPath>`.
struct ClipUse {
    /// The `<use>` element's own ID, which names its wrapper group in `usvg`.
    use_id: String,
    /// The ID of the element the `<use>` references.
    target: String,
}

/// The `<use>` references of each `<clipPath>`, keyed by clip path ID.
#[derive(Default)]
struct ClipUses(HashMap<String, Vec<ClipUse>>);

/// Reads the clip path references that `usvg` cannot resolve on its own.
///
/// `usvg` handles `<use>` in a clip path when it references a shape, but drops
/// it when it references a group (a common Inkscape pattern). Keeping the raw
/// references lets the importer recover that geometry from the rendered tree.
fn clip_uses(text: &str) -> ClipUses {
    let options = usvg::roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let Ok(document) = usvg::roxmltree::Document::parse_with_options(text, options) else {
        return ClipUses::default();
    };

    let mut uses = ClipUses::default();
    for clip in document
        .descendants()
        .filter(|node| node.has_tag_name((SVG_NS, "clipPath")))
    {
        let Some(id) = clip.attribute("id") else {
            continue;
        };
        let referenced: Vec<ClipUse> = clip
            .descendants()
            .filter(|node| node.has_tag_name((SVG_NS, "use")))
            .filter_map(|node| {
                let href = node
                    .attribute((XLINK_NS, "href"))
                    .or_else(|| node.attribute("href"))?;
                Some(ClipUse {
                    use_id: node.attribute("id").unwrap_or_default().to_owned(),
                    target: href.strip_prefix('#')?.to_owned(),
                })
            })
            .collect();
        if !referenced.is_empty() {
            uses.0.insert(id.to_owned(), referenced);
        }
    }
    uses
}

/// A top-level group: the ID `usvg` uses to find it and its optional label.
struct SvgGroup {
    id: String,
    label: Option<String>,
}

impl SvgGroup {
    /// Whether `name` identifies this group.
    ///
    /// Inkscape layers carry a display label; other groups only have an ID.
    /// Both are accepted when both exist, since the ID is what other editors
    /// and SVG tooling show.
    fn matches(&self, name: &str) -> bool {
        self.label.as_deref() == Some(name) || self.id == name
    }

    /// The name shown when listing the selectable groups.
    fn display_name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }
}

/// Reads the document's named top-level groups in document order.
///
/// `usvg` does not keep `inkscape:label`, so labels are read from the raw XML
/// and paired with the IDs that identify the same groups in the rendered tree.
/// Groups without an ID cannot be selected by name and are skipped.
fn svg_groups(text: &str) -> Vec<SvgGroup> {
    let options = usvg::roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let Ok(document) = usvg::roxmltree::Document::parse_with_options(text, options) else {
        return Vec::new();
    };

    document
        .root_element()
        .children()
        .filter(|node| node.is_element() && node.has_tag_name((SVG_NS, "g")))
        .filter_map(|node| {
            Some(SvgGroup {
                id: node.attribute("id")?.to_owned(),
                label: node.attribute((INKSCAPE_NS, "label")).map(str::to_owned),
            })
        })
        .collect()
}

/// Resolves clip paths that `usvg` left empty.
struct ClipResolver<'a> {
    tree: &'a usvg::Tree,
    uses: &'a ClipUses,
    /// Absolute transform of each rendered node's parent, keyed by node ID.
    parents: &'a HashMap<String, usvg::Transform>,
}

impl ClipResolver<'_> {
    /// Recovers clip geometry from `<use>` references to groups.
    ///
    /// `usvg` drops these from the clip path, but the referenced group is
    /// usually still present in the rendered tree. Its geometry is taken
    /// relative to the referenced node's parent, which is what a `<use>`
    /// instantiates, and then placed in the clipped group's space.
    fn collect_use_clip_paths(
        &self,
        clip: &usvg::ClipPath,
        group: &usvg::Group,
        geometry: &mut ClipGeometry,
    ) {
        let Some(references) = self.uses.0.get(clip.id()) else {
            return;
        };
        let group_transform = group.abs_transform();

        for reference in references {
            let Some(node) = self.tree.node_by_id(&reference.target) else {
                continue;
            };
            let Some(parent) = self.parents.get(&reference.target) else {
                continue;
            };
            let Some(inverse_parent) = parent.invert() else {
                continue;
            };
            let use_transform = find_use_transform(clip.root(), &reference.use_id);
            // Instantiate the referenced subtree at the `<use>`, then map it
            // through the clipped group and the clip path's own transform.
            let base = group_transform
                .pre_concat(clip.transform())
                .pre_concat(use_transform)
                .pre_concat(inverse_parent);
            collect_clip_node(node, base, geometry);
        }
    }
}

/// Records the absolute transform of each rendered node's parent.
fn collect_parent_transforms(group: &usvg::Group, parents: &mut HashMap<String, usvg::Transform>) {
    for node in group.children() {
        let id = node.id();
        if !id.is_empty() {
            // Duplicate IDs resolve to the first occurrence, matching
            // `usvg::Tree::node_by_id`.
            parents
                .entry(id.to_owned())
                .or_insert_with(|| group.abs_transform());
        }
        if let usvg::Node::Group(child) = node {
            collect_parent_transforms(child, parents);
        }
    }
}

/// The transform `usvg` recorded for a `<use>` wrapper group.
fn find_use_transform(root: &usvg::Group, use_id: &str) -> usvg::Transform {
    root.children()
        .iter()
        .find_map(|node| match node {
            usvg::Node::Group(group)
                if group.id() == use_id || (use_id.is_empty() && group.id().is_empty()) =>
            {
                Some(group.transform())
            }
            _ => None,
        })
        .unwrap_or_default()
}

fn collect_clip_node(node: &usvg::Node, transform: usvg::Transform, geometry: &mut ClipGeometry) {
    match node {
        usvg::Node::Group(group) => {
            for child in group.children() {
                collect_clip_node(child, transform, geometry);
            }
        }
        usvg::Node::Path(path) => {
            let transform = transform.pre_concat(path.abs_transform());
            if let Some(data) = path.data().clone().transform(transform) {
                let paths = split_subpaths(&data);
                if !paths.is_empty() {
                    geometry.paths.extend(paths);
                    geometry.sources.push(path.id().to_owned());
                }
            }
        }
        usvg::Node::Text(_) | usvg::Node::Image(_) => {}
    }
}

/// Converts a group's children, assigning Z in document order.
///
/// `opacity` is the accumulated opacity of the group's ancestors, including
/// this group.
fn shapes_from_children(group: &usvg::Group, opacity: f32, resolver: &ClipResolver) -> Vec<Shape> {
    let mut shapes = Vec::new();
    append_children(group, opacity, &mut shapes, resolver);
    reset_z(&mut shapes);
    shapes
}

fn append_children(
    group: &usvg::Group,
    opacity: f32,
    shapes: &mut Vec<Shape>,
    resolver: &ClipResolver,
) {
    for node in group.children() {
        append_node(node, opacity, shapes, resolver);
    }
}

/// SVG paints siblings in document order, which the shape vector already
/// records. Keep every imported shape at the default Z of zero so painting can
/// rely on stable Z sorting, and leave Z free for animation offsets.
fn reset_z(shapes: &mut [Shape]) {
    for shape in shapes {
        shape.z = 0.0;
    }
}

fn append_node(node: &usvg::Node, opacity: f32, shapes: &mut Vec<Shape>, resolver: &ClipResolver) {
    match node {
        usvg::Node::Group(group) => append_group(group, opacity, shapes, resolver),
        usvg::Node::Path(path) => {
            if let Some(shape) = shape_from_path(path, opacity) {
                shapes.push(shape);
            }
        }
        // Text is disabled in `usvg`'s feature set, and neither text nor
        // images are shapes.
        usvg::Node::Text(_) | usvg::Node::Image(_) => {}
    }
}

/// Adds a group's contents to `shapes`.
///
/// A clip group becomes a parent shape whose own paths are the group's clip
/// geometry; the model clips a shape's children to its paths, which reproduces
/// the SVG clip without painting the clip outline. When that geometry comes
/// from a single shape in the group's own artwork, that shape is promoted to
/// parent instead of creating an invisible stand-in, so the document keeps the
/// shape that actually defines the clip. Plain groups are flattened because
/// the changes they carry (transforms and opacity) are already folded into
/// their children.
fn append_group(
    group: &usvg::Group,
    opacity: f32,
    shapes: &mut Vec<Shape>,
    resolver: &ClipResolver,
) {
    let opacity = opacity * group.opacity().get();

    let Some(clip) = group.clip_path() else {
        append_children(group, opacity, shapes, resolver);
        return;
    };

    let mut children = Vec::new();
    append_children(group, opacity, &mut children, resolver);
    if children.is_empty() {
        return;
    }
    reset_z(&mut children);

    let clip = clip_geometry(clip, group, resolver);

    let mut parent = match clip_source(&clip, &children) {
        Some(index) => children.remove(index),
        None => Shape {
            name: group.id().to_owned(),
            paths: clip.paths,
            ..Default::default()
        },
    };
    parent.children = children;
    shapes.push(parent);
}

/// The index of the child that supplies a clip's geometry, if one does.
///
/// A clip path's geometry can come from a shape that is also drawn inside the
/// clipped group (Inkscape exports a clipped object with the clipping group as
/// its first child). Such a shape needs no stand-in: promoting it to parent
/// paints and clips exactly as before. The geometry must match, because a
/// `clipPath` transform would otherwise make the painted shape differ from the
/// clip region.
fn clip_source(clip: &ClipGeometry, children: &[Shape]) -> Option<usize> {
    let [source] = clip.sources.as_slice() else {
        return None;
    };
    if source.is_empty() {
        return None;
    }
    children
        .iter()
        .position(|child| child.name == *source && same_paths(&child.paths, &clip.paths))
}

/// Whether two path lists describe the same geometry.
///
/// The clip geometry and the drawn shape are transformed through different
/// chains, so their coordinates can differ by float noise even when they
/// describe the same outline.
fn same_paths(a: &[Path], b: &[Path]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.is_closed() == b.is_closed()
                && same_point(a.start(), b.start())
                && a.segments.len() == b.segments.len()
                && a.segments.iter().zip(&b.segments).all(|(a, b)| {
                    same_point(a.ctrl1, b.ctrl1) && same_point(a.ctrl2, b.ctrl2) && near(a.to, b.to)
                })
        })
}

fn same_point(a: Option<Point>, b: Option<Point>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => near(a, b),
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
    }
}

/// Compares points with an absolute and a relative tolerance.
fn near(a: Point, b: Point) -> bool {
    a.iter()
        .zip(b)
        .all(|(a, b)| (a - b).abs() <= 1e-3 + 1e-5 * a.abs().max(b.abs()))
}

/// A clip path's geometry and the IDs of the paths that provided it.
struct ClipGeometry {
    paths: Vec<Path>,
    /// One entry per contributing path element. A single path with several
    /// subpaths still appears once.
    sources: Vec<String>,
}

/// Converts a clip path's geometry into canvas-space model paths.
///
/// A clip path's contents live in the clipped group's user space, with the
/// `clipPath` element's own transform applied on top.
fn clip_geometry(
    clip: &usvg::ClipPath,
    group: &usvg::Group,
    resolver: &ClipResolver,
) -> ClipGeometry {
    let transform = group.abs_transform().pre_concat(clip.transform());
    let mut geometry = ClipGeometry {
        paths: Vec::new(),
        sources: Vec::new(),
    };
    collect_clip_paths(clip.root(), transform, &mut geometry);
    if geometry.paths.is_empty() {
        resolver.collect_use_clip_paths(clip, group, &mut geometry);
    }
    geometry
}

fn collect_clip_paths(
    group: &usvg::Group,
    transform: usvg::Transform,
    geometry: &mut ClipGeometry,
) {
    for node in group.children() {
        match node {
            usvg::Node::Path(path) => {
                let transform = transform.pre_concat(path.abs_transform());
                if let Some(data) = path.data().clone().transform(transform) {
                    let paths = split_subpaths(&data);
                    if !paths.is_empty() {
                        geometry.paths.extend(paths);
                        geometry.sources.push(path.id().to_owned());
                    }
                }
            }
            usvg::Node::Group(group) => collect_clip_paths(group, transform, geometry),
            usvg::Node::Text(_) | usvg::Node::Image(_) => {}
        }
    }
}

fn shape_from_path(path: &usvg::Path, opacity: f32) -> Option<Shape> {
    if !path.is_visible() {
        return None;
    }

    // Copy the geometry into canvas space so the asset needs no transforms.
    let data = path.data().clone().transform(path.abs_transform())?;
    let paths = split_subpaths(&data);
    if paths.is_empty() {
        return None;
    }

    Some(Shape {
        name: path.id().to_owned(),
        style: style_from_path(path, opacity),
        paths,
        ..Default::default()
    })
}

/// Splits a `usvg` path into one model [`Path`] per subpath.
///
/// `usvg` guarantees absolute MoveTo, LineTo, QuadTo, CurveTo, and ClosePath
/// segments, so relative commands and arcs have already been resolved.
fn split_subpaths(data: &usvg::tiny_skia_path::Path) -> Vec<Path> {
    let mut paths = Vec::new();
    let mut current: Option<Path> = None;

    for segment in data.segments() {
        match segment {
            PathSegment::MoveTo(point) => {
                finish_open(&mut paths, &mut current);
                current = Some(Path::new([point.x, point.y]));
            }
            PathSegment::LineTo(point) => {
                if let Some(path) = &mut current {
                    path.segments.push(Segment::line([point.x, point.y]));
                }
            }
            PathSegment::QuadTo(control, point) => {
                if let Some(path) = &mut current {
                    let to = [point.x, point.y];
                    let segment = quad_to_cubic(end_point(path), [control.x, control.y], to);
                    path.segments.push(segment);
                }
            }
            PathSegment::CubicTo(control1, control2, point) => {
                if let Some(path) = &mut current {
                    path.segments.push(Segment {
                        ctrl1: Some([control1.x, control1.y]),
                        ctrl2: Some([control2.x, control2.y]),
                        to: [point.x, point.y],
                        ..Default::default()
                    });
                }
            }
            PathSegment::Close => {
                if let Some(mut path) = current.take() {
                    let start = path.start();
                    if !path.segments.is_empty() {
                        path.close();
                        paths.push(path);
                    }
                    // Drawing commands after a close begin a new subpath at
                    // the closed subpath's start point.
                    if let Some(start) = start {
                        current = Some(Path::new(start));
                    }
                }
            }
        }
    }

    finish_open(&mut paths, &mut current);
    paths
}

/// Pushes the in-progress path when it has geometry.
fn finish_open(paths: &mut Vec<Path>, current: &mut Option<Path>) {
    if let Some(path) = current.take()
        && !path.segments.is_empty()
    {
        paths.push(path);
    }
}

/// The point the next segment starts from.
fn end_point(path: &Path) -> Point {
    path.segments
        .last()
        .map(|segment| segment.to)
        .or_else(|| path.start())
        .unwrap_or([0.0, 0.0])
}

/// Converts a quadratic Bézier into the equivalent cubic one.
fn quad_to_cubic(from: Point, control: Point, to: Point) -> Segment {
    let toward = |anchor: Point| {
        [
            anchor[0] + (control[0] - anchor[0]) * 2.0 / 3.0,
            anchor[1] + (control[1] - anchor[1]) * 2.0 / 3.0,
        ]
    };
    Segment {
        ctrl1: Some(toward(from)),
        ctrl2: Some(toward(to)),
        to,
        ..Default::default()
    }
}

fn style_from_path(path: &usvg::Path, opacity: f32) -> Style {
    Style {
        fill: path.fill().and_then(|fill| {
            let (color, paint_opacity) = paint_color(fill.paint())?;
            Some(with_alpha(
                color,
                paint_opacity * fill.opacity().get() * opacity,
            ))
        }),
        stroke: path.stroke().and_then(|stroke| {
            let (color, paint_opacity) = paint_color(stroke.paint())?;
            Some(StrokeStyle {
                color: with_alpha(color, paint_opacity * stroke.opacity().get() * opacity),
            })
        }),
    }
}

/// The solid color that stands in for a paint, plus the opacity it carries.
///
/// Gradients have no single color, so one representative stop is used. A
/// pattern has no meaningful stand-in and is skipped.
fn paint_color(paint: &usvg::Paint) -> Option<(Rgba8, f32)> {
    match paint {
        usvg::Paint::Color(color) => Some((rgb(*color), 1.0)),
        usvg::Paint::LinearGradient(gradient) => gradient_color(gradient.stops()),
        usvg::Paint::RadialGradient(gradient) => gradient_color(gradient.stops()),
        usvg::Paint::Pattern(_) => None,
    }
}

/// Picks the first visible stop, falling back to the first stop.
fn gradient_color(stops: &[usvg::Stop]) -> Option<(Rgba8, f32)> {
    let stop = stops
        .iter()
        .find(|stop| stop.opacity().get() > 0.0)
        .or_else(|| stops.first())?;
    Some((rgb(stop.color()), stop.opacity().get()))
}

fn rgb(color: usvg::Color) -> Rgba8 {
    Rgba8::rgb(color.red, color.green, color.blue)
}

/// Multiplies a color's alpha by `opacity`.
fn with_alpha(color: Rgba8, opacity: f32) -> Rgba8 {
    let alpha = (f32::from(color.a) * opacity.clamp(0.0, 1.0))
        .round()
        .clamp(0.0, 255.0) as u8;
    Rgba8::rgba(color.r, color.g, color.b, alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(body: &str) -> Asset {
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg"
                    xmlns:xlink="http://www.w3.org/1999/xlink"
                    viewBox="0 0 100 100">{body}</svg>"##
        );
        Asset::from_svg_str(&svg).unwrap()
    }

    #[test]
    fn imports_a_closed_path_with_fill_and_stroke() {
        let asset = import(
            r##"<path id="triangle" d="M 10 10 L 90 10 L 50 80 Z"
                      fill="#ff0000" stroke="#000000"/>"##,
        );

        assert_eq!(asset.shapes.len(), 1);
        let shape = &asset.shapes[0];
        assert_eq!(shape.name, "triangle");
        assert_eq!(shape.style.fill, Some(Rgba8::rgb(255, 0, 0)));
        assert_eq!(
            shape.style.stroke.map(|stroke| stroke.color),
            Some(Rgba8::rgb(0, 0, 0))
        );

        assert_eq!(shape.paths.len(), 1);
        let path = &shape.paths[0];
        assert!(path.is_closed());
        assert_eq!(path.start(), Some([10.0, 10.0]));
        assert_eq!(path.segments.len(), 3);
        assert_eq!(path.segments[2], Segment::line([10.0, 10.0]));
    }

    #[test]
    fn converts_basic_shapes_into_paths() {
        let asset = import(
            r##"<rect x="10" y="20" width="30" height="40" fill="#00ff00"/>
               <circle cx="50" cy="50" r="10" fill="#0000ff"/>"##,
        );

        assert_eq!(asset.shapes.len(), 2);
        assert!(asset.shapes.iter().all(|shape| shape.paths.len() == 1));
        assert!(asset.shapes.iter().all(|shape| shape.paths[0].is_closed()));
        assert_eq!(asset.shapes[0].paths[0].segments.len(), 4);
        assert!(!asset.shapes[1].paths[0].segments.is_empty());
    }

    #[test]
    fn flattens_groups_without_clips_and_bakes_their_state() {
        let asset = import(
            r##"<g id="layer" transform="translate(5 7)" fill="#123456">
                 <g id="inner">
                   <path id="mark" d="M 0 0 L 10 0 Z"/>
                 </g>
               </g>"##,
        );

        // Non-clipping groups do not create parent shapes.
        assert_eq!(asset.shapes.len(), 1);
        let shape = &asset.shapes[0];
        assert_eq!(shape.name, "mark");
        assert_eq!(shape.style.fill, Some(Rgba8::rgb(0x12, 0x34, 0x56)));
        assert_eq!(shape.paths[0].start(), Some([5.0, 7.0]));
        assert_eq!(shape.paths[0].segments[0].to, [15.0, 7.0]);
    }

    #[test]
    fn keeps_clip_groups_as_parents_with_clip_geometry() {
        let asset = import(
            r##"<defs>
                 <clipPath id="c" clipPathUnits="userSpaceOnUse"
                           transform="translate(5 0)">
                   <rect x="0" y="0" width="50" height="50"/>
                 </clipPath>
               </defs>
               <g id="clipped" transform="translate(10 0)" clip-path="url(#c)">
                 <path id="child" d="M 0 0 L 100 0 L 100 100 Z" fill="#000000"/>
               </g>"##,
        );

        assert_eq!(asset.shapes.len(), 1);
        let clipped = &asset.shapes[0];
        assert_eq!(clipped.name, "clipped");
        // The clip outline is not painted, but its geometry is kept.
        assert_eq!(clipped.style, Style::default());
        assert_eq!(clipped.paths.len(), 1);
        assert_eq!(clipped.children.len(), 1);

        // The child is unclipped geometry in canvas space; the parent clips it.
        let child = &clipped.children[0];
        assert_eq!(child.name, "child");
        assert_eq!(child.paths[0].start(), Some([10.0, 0.0]));

        // The clip's own translate(5) and the group's translate(10) compose:
        // 0..50 becomes 15..65 in canvas space.
        let (min, max) = points_bounds(&clipped.paths);
        assert_eq!(min, [15.0, 0.0]);
        assert_eq!(max, [65.0, 50.0]);
    }

    #[test]
    fn keeps_a_stand_in_when_the_clip_moves_the_shape_geometry() {
        let asset = import(
            r##"<defs>
                 <clipPath id="clip" transform="translate(5 0)">
                   <use xlink:href="#clip-shape"/>
                 </clipPath>
               </defs>
               <g id="clipped" clip-path="url(#clip)">
                 <g id="clip-shape">
                   <rect id="clip-geometry" x="0" y="0" width="20" height="20"/>
                 </g>
                 <rect id="content" x="0" y="0" width="100" height="100" fill="#000000"/>
               </g>"##,
        );

        // The drawn rect does not sit where the clip path puts its geometry,
        // so promoting it would paint and clip in the wrong place.
        assert_eq!(asset.shapes.len(), 1);
        let clipped = &asset.shapes[0];
        assert_eq!(clipped.name, "clipped");
        assert_eq!(clipped.style, Style::default());
        let (min, max) = points_bounds(&clipped.paths);
        assert_eq!(min, [5.0, 0.0]);
        assert_eq!(max, [25.0, 20.0]);

        // The rect stays an ordinary drawn child.
        assert_eq!(clipped.children.len(), 2);
        assert_eq!(clipped.children[0].name, "clip-geometry");
        assert_eq!(clipped.children[1].name, "content");
    }

    #[test]
    fn nested_clip_groups_stay_nested() {
        let asset = import(
            r##"<defs>
                 <clipPath id="outer"><rect x="0" y="0" width="80" height="80"/></clipPath>
                 <clipPath id="inner"><rect x="10" y="10" width="20" height="20"/></clipPath>
               </defs>
               <g id="outer-group" clip-path="url(#outer)">
                 <g id="inner-group" clip-path="url(#inner)">
                   <path id="leaf" d="M 0 0 L 100 0 L 100 100 Z" fill="#000000"/>
                 </g>
               </g>"##,
        );

        assert_eq!(asset.shapes.len(), 1);
        let outer = &asset.shapes[0];
        assert_eq!(outer.name, "outer-group");
        assert_eq!(outer.children.len(), 1);

        let inner = &outer.children[0];
        assert_eq!(inner.name, "inner-group");
        assert_eq!(inner.children.len(), 1);
        assert_eq!(inner.children[0].name, "leaf");
    }

    #[test]
    fn resolves_clip_path_uses_that_reference_groups() {
        let asset = import(
            r##"<defs>
                 <clipPath id="clip">
                   <use xlink:href="#clip-shape" id="use-clip"/>
                 </clipPath>
               </defs>
               <g id="clipped" clip-path="url(#clip)" transform="translate(10 5)">
                 <g id="clip-shape" transform="translate(2 3)">
                   <rect id="clip-geometry" x="0" y="0" width="20" height="20"/>
                 </g>
                 <rect id="content" x="0" y="0" width="100" height="100" fill="#000000"/>
               </g>"##,
        );

        // The referenced group's shape is both the clip and part of the art,
        // so it becomes the parent instead of an invisible stand-in, keeping
        // its name and paint (a rect's default fill is black).
        assert_eq!(asset.shapes.len(), 1);
        let clipped = &asset.shapes[0];
        assert_eq!(clipped.name, "clip-geometry");
        assert_eq!(clipped.style.fill, Some(Rgba8::rgb(0, 0, 0)));
        assert_eq!(clipped.paths.len(), 1);

        // Clip = rect 0..20 moved by the referenced group (2,3) and then by
        // the clipped group (10,5).
        let (min, max) = points_bounds(&clipped.paths);
        assert_eq!(min, [12.0, 8.0]);
        assert_eq!(max, [32.0, 28.0]);

        // The clip shape is not repeated as a child; only the content is.
        assert_eq!(clipped.children.len(), 1);
        assert_eq!(clipped.children[0].name, "content");
    }

    /// Bounds of all points in `paths`, including opening points and controls.
    fn points_bounds(paths: &[Path]) -> (Point, Point) {
        let mut points = Vec::new();
        for path in paths {
            if let Some(start) = path.start() {
                points.push(start);
            }
            for segment in &path.segments {
                points.extend(segment.ctrl1);
                points.extend(segment.ctrl2);
                points.push(segment.to);
            }
        }

        let min = points.iter().fold([f32::INFINITY; 2], |mut min, point| {
            min[0] = min[0].min(point[0]);
            min[1] = min[1].min(point[1]);
            min
        });
        let max = points
            .iter()
            .fold([f32::NEG_INFINITY; 2], |mut max, point| {
                max[0] = max[0].max(point[0]);
                max[1] = max[1].max(point[1]);
                max
            });
        (min, max)
    }

    #[test]
    fn converts_quadratic_curves_and_arcs_to_cubics() {
        let asset = import(
            r##"<path d="M 0 0 Q 10 20 20 0 A 10 10 0 0 1 40 0"
                      fill="none" stroke="#000000"/>"##,
        );

        let path = &asset.shapes[0].paths[0];
        assert_eq!(path.start(), Some([0.0, 0.0]));
        assert!(path.segments.len() >= 2);
        for segment in &path.segments {
            assert!(segment.ctrl1.is_some());
            assert!(segment.ctrl2.is_some());
            assert!(segment.to[0].is_finite() && segment.to[1].is_finite());
        }
    }

    #[test]
    fn skips_text_and_images() {
        let asset = import(
            r##"<text x="0" y="0">Hello</text>
               <rect x="5" y="5" width="10" height="10" fill="#000000"/>"##,
        );

        assert_eq!(asset.shapes.len(), 1);
        assert_eq!(asset.shapes[0].paths.len(), 1);
    }

    #[test]
    fn multiplies_group_and_fill_opacity_into_alpha() {
        let asset = import(
            r##"<g opacity="0.5">
                 <path d="M 0 0 L 10 0 L 10 10 Z"
                       fill="#ff0000" fill-opacity="0.5"/>
               </g>"##,
        );

        let shape = &asset.shapes[0];
        // 255 * 0.5 group * 0.5 fill-opacity, rounded.
        assert_eq!(shape.style.fill, Some(Rgba8::rgba(255, 0, 0, 64)));
    }

    #[test]
    fn approximates_gradients_with_a_stop_color() {
        let asset = import(
            r##"<defs>
                 <linearGradient id="fade">
                   <stop offset="0" stop-color="#ff0000"/>
                   <stop offset="1" stop-color="#0000ff"/>
                 </linearGradient>
               </defs>
               <rect x="0" y="0" width="10" height="10" fill="url(#fade)"/>"##,
        );

        assert_eq!(asset.shapes.len(), 1);
        assert_eq!(asset.shapes[0].style.fill, Some(Rgba8::rgb(255, 0, 0)));
    }

    /// A document with Inkscape layers, a plain group, and a group nested
    /// inside a layer.
    const GROUPED_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg"
            xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
            viewBox="0 0 100 100">
          <g inkscape:groupmode="layer" inkscape:label="Back" id="back">
            <rect id="back-rect" x="0" y="0" width="10" height="10" fill="#000"/>
          </g>
          <g inkscape:groupmode="layer" inkscape:label="Front" id="front">
            <rect id="front-rect" x="0" y="0" width="10" height="10" fill="#000"/>
            <g inkscape:groupmode="layer" inkscape:label="Details" id="details">
              <rect id="detail-rect" x="20" y="20" width="10" height="10" fill="#000"/>
            </g>
          </g>
          <g id="plain">
            <rect id="plain-rect" x="40" y="40" width="10" height="10" fill="#000"/>
          </g>
        </svg>"##;

    fn shape_names(asset: &Asset) -> Vec<&str> {
        asset
            .shapes
            .iter()
            .map(|shape| shape.name.as_str())
            .collect()
    }

    #[test]
    fn imports_every_group_by_default() {
        let asset = Asset::from_svg_str(GROUPED_SVG).unwrap();
        assert_eq!(
            shape_names(&asset),
            ["back-rect", "front-rect", "detail-rect", "plain-rect"]
        );
    }

    #[test]
    fn selects_a_top_level_group_by_label() {
        let options = SvgImportOptions::new().with_group("Front");
        let asset = Asset::from_svg_str_with_options(GROUPED_SVG, &options).unwrap();

        assert_eq!(shape_names(&asset), ["front-rect", "detail-rect"]);
        // Every imported shape keeps the default Z; vector order paints them
        // in the SVG's order.
        assert_eq!(asset.shapes[0].z, 0.0);
        assert_eq!(asset.shapes[1].z, 0.0);
    }

    #[test]
    fn selects_a_top_level_group_by_id() {
        let options = SvgImportOptions::new().with_group("plain");
        let asset = Asset::from_svg_str_with_options(GROUPED_SVG, &options).unwrap();
        assert_eq!(shape_names(&asset), ["plain-rect"]);

        // An Inkscape layer's element ID works as well as its label.
        let options = SvgImportOptions::new().with_group("back");
        let asset = Asset::from_svg_str_with_options(GROUPED_SVG, &options).unwrap();
        assert_eq!(shape_names(&asset), ["back-rect"]);
    }

    #[test]
    fn unknown_group_lists_the_available_top_level_groups() {
        // `Details` exists but is nested inside `Front`, so it is not
        // top-level and cannot be selected.
        for requested in ["Middle", "Details"] {
            let options = SvgImportOptions::new().with_group(requested);
            let err = Asset::from_svg_str_with_options(GROUPED_SVG, &options).unwrap_err();

            match err {
                SvgImportError::UnknownGroup { name, available } => {
                    assert_eq!(name, requested);
                    assert_eq!(available, ["Back", "Front", "plain"]);
                }
                other => panic!("expected UnknownGroup, got {other:?}"),
            }
        }
    }

    #[test]
    fn imported_assets_have_no_bones_or_animations() {
        let asset = import(r##"<rect width="10" height="10" fill="#000"/>"##);
        assert!(asset.bones.is_empty());
        assert!(asset.animations.is_empty());
    }

    #[test]
    fn rejects_invalid_svg() {
        assert!(matches!(
            Asset::from_svg_str("not an svg"),
            Err(SvgImportError::Parse(_))
        ));
    }
}
