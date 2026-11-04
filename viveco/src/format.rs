//! Zero-copy reader for the compiled `.viveco` binary format.
//!
//! The format is deliberately simple. A fixed 64-byte header stores the element
//! counts and the root-shape range, followed by nine tightly packed arrays.
//! Multi-valued references (a shape's paths, a path's segments, a segment's
//! weights, and so on) are `(start, count)` ranges into those arrays. Names are
//! never stored; bones, shapes, and animations are addressed by index.
//!
//! # Binary layout
//!
//! | Section | Element | Size |
//! |---------|---------|------|
//! | header | — | 64 |
//! | shapes | [`SHAPE_SIZE`] | 40 |
//! | paths | [`PATH_SIZE`] | 32 |
//! | segments | [`SEGMENT_SIZE`] | 52 |
//! | weights | [`WEIGHT_SIZE`] | 8 |
//! | child indices | [`CHILD_SIZE`] | 4 |
//! | bones | [`BONE_SIZE`] | 24 |
//! | animations | [`ANIMATION_SIZE`] | 16 |
//! | curves | [`CURVE_SIZE`] | 16 |
//! | keyframes | [`KEYFRAME_SIZE`] | 16 |
//!
//! All integers and floats are little-endian. The byte layout is produced by
//! `viveco-macros`; both sides must agree on the constants below.

use crate::error::FormatError;

/// Magic bytes at the start of every file: `VIVECO01`.
pub const MAGIC: [u8; 8] = *b"VIVECO01";
/// Current binary layout version.
pub const VERSION: u32 = 1;
/// Size of the fixed header.
pub const HEADER_SIZE: usize = 64;

pub const SHAPE_SIZE: usize = 40;
pub const PATH_SIZE: usize = 32;
pub const SEGMENT_SIZE: usize = 52;
pub const WEIGHT_SIZE: usize = 8;
pub const CHILD_SIZE: usize = 4;
pub const BONE_SIZE: usize = 24;
pub const ANIMATION_SIZE: usize = 16;
pub const CURVE_SIZE: usize = 16;
pub const KEYFRAME_SIZE: usize = 16;

// Header field offsets.
pub const H_MAGIC: usize = 0;
pub const H_VERSION: usize = 8;
pub const H_SHAPE_COUNT: usize = 16;
pub const H_PATH_COUNT: usize = 20;
pub const H_SEGMENT_COUNT: usize = 24;
pub const H_WEIGHT_COUNT: usize = 28;
pub const H_BONE_COUNT: usize = 32;
pub const H_ANIMATION_COUNT: usize = 36;
pub const H_CURVE_COUNT: usize = 40;
pub const H_KEYFRAME_COUNT: usize = 44;
pub const H_ROOT_START: usize = 48;
pub const H_ROOT_COUNT: usize = 52;
pub const H_CHILD_COUNT: usize = 56;

// Shape fields.
const S_Z: usize = 0;
const S_STYLE_FLAGS: usize = 4;
const S_FILL_RGBA: usize = 8;
const S_STROKE_WIDTH: usize = 12;
const S_STROKE_RGBA: usize = 16;
const S_PATH_START: usize = 20;
const S_PATH_COUNT: usize = 24;
const S_CHILD_START: usize = 28;
const S_CHILD_COUNT: usize = 32;
const S_STYLE_FILL: u32 = 1;
const S_STYLE_STROKE: u32 = 2;

// Path fields.
const P_ENDS: usize = 0;
const P_START_X: usize = 8;
const P_START_Y: usize = 12;
const P_SEG_START: usize = 16;
const P_SEG_COUNT: usize = 20;
const P_WEIGHT_START: usize = 24;
const P_WEIGHT_COUNT: usize = 28;
const P_CLOSED: u32 = 1;

// Segment fields.
const G_CTRL1_X: usize = 0;
const G_CTRL1_Y: usize = 4;
const G_CTRL2_X: usize = 8;
const G_CTRL2_Y: usize = 12;
const G_TO_X: usize = 16;
const G_TO_Y: usize = 20;
const G_FLAGS: usize = 24;
const G_CTRL1_WEIGHT_START: usize = 28;
const G_CTRL1_WEIGHT_COUNT: usize = 32;
const G_CTRL2_WEIGHT_START: usize = 36;
const G_CTRL2_WEIGHT_COUNT: usize = 40;
const G_TO_WEIGHT_START: usize = 44;
const G_TO_WEIGHT_COUNT: usize = 48;
const G_HAS_CTRL1: u32 = 1;
const G_HAS_CTRL2: u32 = 2;

// Weight fields.
const W_BONE: usize = 0;
const W_WEIGHT: usize = 4;

// Bone fields.
const B_HEAD_X: usize = 0;
const B_HEAD_Y: usize = 4;
const B_TAIL_X: usize = 8;
const B_TAIL_Y: usize = 12;
const B_PARENT: usize = 16;
const B_FLAGS: usize = 20;
const B_CONNECTED: u32 = 1;

// Animation fields.
const A_DURATION: usize = 0;
const A_FLAGS: usize = 4;
const A_CURVE_START: usize = 8;
const A_CURVE_COUNT: usize = 12;
const A_CYCLIC: u32 = 1;

// Curve fields.
const C_BONE: usize = 0;
const C_PROPERTY: usize = 4;
const C_KEY_START: usize = 8;
const C_KEY_COUNT: usize = 12;

// Keyframe fields.
const K_TIME: usize = 0;
const K_VALUE: usize = 4;
const K_HANDLE_IN: usize = 8;
const K_HANDLE_OUT: usize = 12;

/// Sentinel for "no parent" in a bone entry.
pub const NO_PARENT: u32 = u32::MAX;

/// A property a curve can drive. Matches the codes stored in the binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoneProperty {
    X,
    Y,
    Angle,
    Scale,
}

impl BoneProperty {
    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::X),
            1 => Some(Self::Y),
            2 => Some(Self::Angle),
            3 => Some(Self::Scale),
            _ => None,
        }
    }

    /// Rest value of the property. Translation and rotation start at zero,
    /// scale starts at one.
    pub fn rest(self) -> f32 {
        match self {
            Self::Scale => 1.0,
            _ => 0.0,
        }
    }
}

/// An 8-bit sRGB color with straight alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    fn from_packed(packed: u32) -> Self {
        Self {
            r: (packed >> 24) as u8,
            g: (packed >> 16) as u8,
            b: (packed >> 8) as u8,
            a: packed as u8,
        }
    }
}

/// Element counts read from the header.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counts {
    pub shapes: usize,
    pub paths: usize,
    pub segments: usize,
    pub weights: usize,
    pub bones: usize,
    pub animations: usize,
    pub curves: usize,
    pub keyframes: usize,
    pub root_start: usize,
    pub root_count: usize,
    pub child_count: usize,
}

/// Byte offsets of each section.
#[derive(Clone, Copy, Debug, Default)]
struct Layout {
    shapes: usize,
    paths: usize,
    segments: usize,
    weights: usize,
    children: usize,
    bones: usize,
    animations: usize,
    curves: usize,
    keyframes: usize,
}

impl Layout {
    fn new(counts: &Counts, len: usize) -> Result<Self, FormatError> {
        let mut offset = HEADER_SIZE;
        let mut section = |count: usize, size: usize| -> Result<usize, FormatError> {
            let start = offset;
            offset =
                offset
                    .checked_add(count.saturating_mul(size))
                    .ok_or(FormatError::Truncated {
                        len,
                        needed: usize::MAX,
                    })?;
            if offset > len {
                return Err(FormatError::Truncated {
                    len,
                    needed: offset,
                });
            }
            Ok(start)
        };

        let shapes = section(counts.shapes, SHAPE_SIZE)?;
        let paths = section(counts.paths, PATH_SIZE)?;
        let segments = section(counts.segments, SEGMENT_SIZE)?;
        let weights = section(counts.weights, WEIGHT_SIZE)?;
        let children = section(counts.child_count, CHILD_SIZE)?;
        let bones = section(counts.bones, BONE_SIZE)?;
        let animations = section(counts.animations, ANIMATION_SIZE)?;
        let curves = section(counts.curves, CURVE_SIZE)?;
        let keyframes = section(counts.keyframes, KEYFRAME_SIZE)?;

        Ok(Self {
            shapes,
            paths,
            segments,
            weights,
            children,
            bones,
            animations,
            curves,
            keyframes,
        })
    }

    fn entry(&self, base: usize, size: usize, index: usize) -> usize {
        base + index * size
    }
}

/// A validated, zero-copy view over an embedded viveco binary.
#[derive(Clone, Copy, Debug)]
pub struct Viveco<'a> {
    bytes: &'a [u8],
    counts: Counts,
    layout: Layout,
}

impl<'a> Viveco<'a> {
    /// Validates `bytes` and returns a view over it.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FormatError> {
        let art = Self::parse_trusted(bytes)?;
        art.validate()?;
        Ok(art)
    }

    /// Reads the header and section table without checking every index.
    ///
    /// This is the cheap path used after a document has already been validated
    /// once (for example by the asset loader). It still rejects a bad magic,
    /// version, or truncated section table.
    pub fn parse_trusted(bytes: &'a [u8]) -> Result<Self, FormatError> {
        if bytes.len() < HEADER_SIZE {
            return Err(FormatError::TooShort);
        }
        if bytes[H_MAGIC..H_MAGIC + 8] != MAGIC {
            return Err(FormatError::BadMagic);
        }
        let version = read_u32(bytes, H_VERSION);
        if version != VERSION {
            return Err(FormatError::UnsupportedVersion(version));
        }

        let counts = Counts {
            shapes: read_u32(bytes, H_SHAPE_COUNT) as usize,
            paths: read_u32(bytes, H_PATH_COUNT) as usize,
            segments: read_u32(bytes, H_SEGMENT_COUNT) as usize,
            weights: read_u32(bytes, H_WEIGHT_COUNT) as usize,
            bones: read_u32(bytes, H_BONE_COUNT) as usize,
            animations: read_u32(bytes, H_ANIMATION_COUNT) as usize,
            curves: read_u32(bytes, H_CURVE_COUNT) as usize,
            keyframes: read_u32(bytes, H_KEYFRAME_COUNT) as usize,
            root_start: read_u32(bytes, H_ROOT_START) as usize,
            root_count: read_u32(bytes, H_ROOT_COUNT) as usize,
            child_count: read_u32(bytes, H_CHILD_COUNT) as usize,
        };

        let layout = Layout::new(&counts, bytes.len())?;
        Ok(Self {
            bytes,
            counts,
            layout,
        })
    }

    /// Returns the raw bytes backing this view.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Element counts.
    pub fn counts(&self) -> Counts {
        self.counts
    }

    pub fn shape_count(&self) -> usize {
        self.counts.shapes
    }

    pub fn bone_count(&self) -> usize {
        self.counts.bones
    }

    pub fn animation_count(&self) -> usize {
        self.counts.animations
    }

    fn entry(&self, offset: usize, size: usize) -> &'a [u8] {
        let bytes: &'a [u8] = self.bytes;
        &bytes[offset..offset + size]
    }

    fn shape_entry(&self, index: usize) -> &'a [u8] {
        self.entry(
            self.layout.entry(self.layout.shapes, SHAPE_SIZE, index),
            SHAPE_SIZE,
        )
    }

    fn path_entry(&self, index: usize) -> &'a [u8] {
        self.entry(
            self.layout.entry(self.layout.paths, PATH_SIZE, index),
            PATH_SIZE,
        )
    }

    fn segment_entry(&self, index: usize) -> &'a [u8] {
        self.entry(
            self.layout.entry(self.layout.segments, SEGMENT_SIZE, index),
            SEGMENT_SIZE,
        )
    }

    fn weight_entry(&self, index: usize) -> &'a [u8] {
        self.entry(
            self.layout.entry(self.layout.weights, WEIGHT_SIZE, index),
            WEIGHT_SIZE,
        )
    }

    fn child_index(&self, index: usize) -> usize {
        read_u32(
            self.entry(
                self.layout.entry(self.layout.children, CHILD_SIZE, index),
                CHILD_SIZE,
            ),
            0,
        ) as usize
    }

    fn bone_entry(&self, index: usize) -> &'a [u8] {
        self.entry(
            self.layout.entry(self.layout.bones, BONE_SIZE, index),
            BONE_SIZE,
        )
    }

    fn animation_entry(&self, index: usize) -> &'a [u8] {
        self.entry(
            self.layout
                .entry(self.layout.animations, ANIMATION_SIZE, index),
            ANIMATION_SIZE,
        )
    }

    fn curve_entry(&self, index: usize) -> &'a [u8] {
        self.entry(
            self.layout.entry(self.layout.curves, CURVE_SIZE, index),
            CURVE_SIZE,
        )
    }

    fn keyframe_entry(&self, index: usize) -> &'a [u8] {
        self.entry(
            self.layout
                .entry(self.layout.keyframes, KEYFRAME_SIZE, index),
            KEYFRAME_SIZE,
        )
    }

    /// A shape by flat index.
    pub fn shape(&self, index: usize) -> Option<ShapeRef<'a>> {
        (index < self.counts.shapes).then_some(ShapeRef { art: *self, index })
    }

    /// A bone by flat index.
    pub fn bone(&self, index: usize) -> Option<BoneRef<'a>> {
        (index < self.counts.bones).then_some(BoneRef { art: *self, index })
    }

    /// An animation by index.
    pub fn animation(&self, index: usize) -> Option<AnimationRef<'a>> {
        (index < self.counts.animations).then_some(AnimationRef { art: *self, index })
    }

    /// Indices of the top-level shapes in painting order.
    pub fn root_shape_indices(&self) -> impl Iterator<Item = usize> + 'a {
        let start = self.counts.root_start;
        let end = start + self.counts.root_count;
        let art = *self;
        (start..end).map(move |index| art.child_index(index))
    }

    /// Top-level shapes in painting order.
    pub fn root_shapes(&self) -> impl Iterator<Item = ShapeRef<'a>> {
        self.root_shape_indices()
            .filter_map(|index| self.shape(index))
    }

    /// Conservative rest-pose bounds, including Bézier control points.
    pub fn bounds(&self) -> Option<Bounds> {
        let mut acc = BoundsAccumulator::default();
        for shape in self.root_shapes() {
            acc.add_shape(shape);
        }
        acc.finish()
    }

    fn validate(&self) -> Result<(), FormatError> {
        for index in 0..self.counts.shapes {
            let shape = self.shape(index).unwrap();
            let path_start = shape.path_start();
            if path_start + shape.path_count() > self.counts.paths {
                return Err(FormatError::ShapePaths { shape: index });
            }
            let child_start = shape.child_start();
            if child_start + shape.child_count() > self.counts.child_count {
                return Err(FormatError::ShapeChildren { shape: index });
            }
            for child in shape.child_index_range() {
                if child >= self.counts.shapes {
                    return Err(FormatError::ShapeChildIndex {
                        shape: index,
                        child,
                        count: self.counts.shapes,
                    });
                }
            }
        }

        for index in 0..self.counts.paths {
            let path = self.path(index).unwrap();
            if path.segment_start() + path.segment_count() > self.counts.segments {
                return Err(FormatError::PathSegments { path: index });
            }
            if path.weight_start() + path.weight_count() > self.counts.weights {
                return Err(FormatError::PathWeights { path: index });
            }
            if path.ends_code() > 1 {
                return Err(FormatError::PathEnds {
                    path: index,
                    flag: path.ends_code(),
                });
            }
        }

        for index in 0..self.counts.segments {
            let segment = self.segment(index).unwrap();
            for (start, count) in [
                segment.ctrl1_weight_range(),
                segment.ctrl2_weight_range(),
                segment.to_weight_range(),
            ] {
                if start + count > self.counts.weights {
                    return Err(FormatError::SegmentWeights { segment: index });
                }
            }
        }

        for index in 0..self.counts.weights {
            let weight = self.weight(index);
            if weight.bone >= self.counts.bones {
                return Err(FormatError::WeightBone {
                    weight: index,
                    bone: weight.bone,
                    count: self.counts.bones,
                });
            }
        }

        for index in 0..self.counts.bones {
            let bone = self.bone(index).unwrap();
            if let Some(parent) = bone.parent()
                && parent >= index
            {
                return Err(FormatError::BoneParent {
                    bone: index,
                    parent,
                });
            }
        }

        for index in 0..self.counts.animations {
            let animation = self.animation(index).unwrap();
            if animation.curve_start() + animation.curve_count_raw() > self.counts.curves {
                return Err(FormatError::AnimationCurves { animation: index });
            }
            for curve_index in
                animation.curve_start()..animation.curve_start() + animation.curve_count_raw()
            {
                let curve = self.curve(curve_index).unwrap();
                if curve.bone() >= self.counts.bones {
                    return Err(FormatError::CurveBone {
                        curve: curve_index,
                        bone: curve.bone(),
                        count: self.counts.bones,
                    });
                }
                if curve.property().is_none() {
                    return Err(FormatError::CurveProperty {
                        curve: curve_index,
                        property: curve.property_code(),
                    });
                }
                if curve.key_start() + curve.key_count() > self.counts.keyframes {
                    return Err(FormatError::CurveKeyframes { curve: curve_index });
                }
            }
        }

        if self.counts.root_start + self.counts.root_count > self.counts.child_count {
            return Err(FormatError::RootShapes);
        }

        Ok(())
    }
}

/// A shape entry.
#[derive(Clone, Copy, Debug)]
pub struct ShapeRef<'a> {
    art: Viveco<'a>,
    index: usize,
}

impl<'a> ShapeRef<'a> {
    pub fn index(&self) -> usize {
        self.index
    }

    fn entry(&self) -> &'a [u8] {
        self.art.shape_entry(self.index)
    }

    pub fn z(&self) -> f32 {
        read_f32(self.entry(), S_Z)
    }

    pub fn fill(&self) -> Option<Rgba8> {
        let flags = read_u32(self.entry(), S_STYLE_FLAGS);
        (flags & S_STYLE_FILL != 0).then(|| Rgba8::from_packed(read_u32(self.entry(), S_FILL_RGBA)))
    }

    pub fn stroke(&self) -> Option<(Rgba8, f32)> {
        let entry = self.entry();
        let flags = read_u32(entry, S_STYLE_FLAGS);
        if flags & S_STYLE_STROKE == 0 {
            return None;
        }
        Some((
            Rgba8::from_packed(read_u32(entry, S_STROKE_RGBA)),
            read_f32(entry, S_STROKE_WIDTH),
        ))
    }

    fn path_start(&self) -> usize {
        read_u32(self.entry(), S_PATH_START) as usize
    }

    fn path_count(&self) -> usize {
        read_u32(self.entry(), S_PATH_COUNT) as usize
    }

    pub fn has_paths(&self) -> bool {
        self.path_count() != 0
    }

    pub fn paths(&self) -> impl Iterator<Item = PathRef<'a>> {
        let art = self.art;
        let start = self.path_start();
        let end = start + self.path_count();
        (start..end).filter_map(move |index| art.path(index))
    }

    fn child_start(&self) -> usize {
        read_u32(self.entry(), S_CHILD_START) as usize
    }

    fn child_count(&self) -> usize {
        read_u32(self.entry(), S_CHILD_COUNT) as usize
    }

    pub fn has_children(&self) -> bool {
        self.child_count() != 0
    }

    fn child_index_range(&self) -> impl Iterator<Item = usize> {
        let art = self.art;
        let start = self.child_start();
        let end = start + self.child_count();
        (start..end).map(move |index| art.child_index(index))
    }

    pub fn children(&self) -> impl Iterator<Item = ShapeRef<'a>> {
        let art = self.art;
        self.child_index_range()
            .filter_map(move |index| art.shape(index))
    }
}

/// A path entry.
#[derive(Clone, Copy, Debug)]
pub struct PathRef<'a> {
    art: Viveco<'a>,
    index: usize,
}

impl<'a> PathRef<'a> {
    pub fn index(&self) -> usize {
        self.index
    }

    fn entry(&self) -> &'a [u8] {
        self.art.path_entry(self.index)
    }

    fn ends_code(&self) -> u32 {
        read_u32(self.entry(), P_ENDS)
    }

    pub fn is_closed(&self) -> bool {
        self.ends_code() & P_CLOSED != 0
    }

    /// The stored start point. For a closed path this mirrors the last
    /// segment's end point, but [`Self::start_point`] is authoritative.
    pub fn stored_start(&self) -> [f32; 2] {
        let entry = self.entry();
        [read_f32(entry, P_START_X), read_f32(entry, P_START_Y)]
    }

    /// The first anchor: the stored start of an open path, or the last
    /// segment's end point of a closed loop.
    pub fn start_point(&self) -> Option<[f32; 2]> {
        if self.is_closed() {
            self.last_segment().map(|segment| segment.to())
        } else {
            Some(self.stored_start())
        }
    }

    /// Weights of the start anchor. A closed loop has no separate start, so
    /// this is only meaningful for open paths.
    pub fn start_weights(&self) -> WeightIter<'a> {
        let entry = self.entry();
        WeightIter {
            art: self.art,
            index: read_u32(entry, P_WEIGHT_START) as usize,
            count: read_u32(entry, P_WEIGHT_COUNT) as usize,
        }
    }

    fn segment_start(&self) -> usize {
        read_u32(self.entry(), P_SEG_START) as usize
    }

    fn segment_count(&self) -> usize {
        read_u32(self.entry(), P_SEG_COUNT) as usize
    }

    pub fn segments(&self) -> impl Iterator<Item = SegmentRef<'a>> {
        let art = self.art;
        let start = self.segment_start();
        let end = start + self.segment_count();
        (start..end).filter_map(move |index| art.segment(index))
    }

    pub fn last_segment(&self) -> Option<SegmentRef<'a>> {
        let count = self.segment_count();
        if count == 0 {
            None
        } else {
            self.art.segment(self.segment_start() + count - 1)
        }
    }

    fn weight_start(&self) -> usize {
        read_u32(self.entry(), P_WEIGHT_START) as usize
    }

    fn weight_count(&self) -> usize {
        read_u32(self.entry(), P_WEIGHT_COUNT) as usize
    }
}

/// One Bézier segment.
#[derive(Clone, Copy, Debug)]
pub struct SegmentRef<'a> {
    art: Viveco<'a>,
    index: usize,
}

impl<'a> SegmentRef<'a> {
    pub fn index(&self) -> usize {
        self.index
    }

    fn entry(&self) -> &'a [u8] {
        self.art.segment_entry(self.index)
    }

    fn flags(&self) -> u32 {
        read_u32(self.entry(), G_FLAGS)
    }

    pub fn ctrl1(&self) -> Option<[f32; 2]> {
        (self.flags() & G_HAS_CTRL1 != 0).then(|| {
            let entry = self.entry();
            [read_f32(entry, G_CTRL1_X), read_f32(entry, G_CTRL1_Y)]
        })
    }

    pub fn ctrl2(&self) -> Option<[f32; 2]> {
        (self.flags() & G_HAS_CTRL2 != 0).then(|| {
            let entry = self.entry();
            [read_f32(entry, G_CTRL2_X), read_f32(entry, G_CTRL2_Y)]
        })
    }

    pub fn to(&self) -> [f32; 2] {
        let entry = self.entry();
        [read_f32(entry, G_TO_X), read_f32(entry, G_TO_Y)]
    }

    pub fn is_line(&self) -> bool {
        self.ctrl1().is_none() && self.ctrl2().is_none()
    }

    fn ctrl1_weight_range(&self) -> (usize, usize) {
        let entry = self.entry();
        (
            read_u32(entry, G_CTRL1_WEIGHT_START) as usize,
            read_u32(entry, G_CTRL1_WEIGHT_COUNT) as usize,
        )
    }

    fn ctrl2_weight_range(&self) -> (usize, usize) {
        let entry = self.entry();
        (
            read_u32(entry, G_CTRL2_WEIGHT_START) as usize,
            read_u32(entry, G_CTRL2_WEIGHT_COUNT) as usize,
        )
    }

    fn to_weight_range(self) -> (usize, usize) {
        let entry = self.entry();
        (
            read_u32(entry, G_TO_WEIGHT_START) as usize,
            read_u32(entry, G_TO_WEIGHT_COUNT) as usize,
        )
    }

    fn weights(&self, range: (usize, usize)) -> WeightIter<'a> {
        WeightIter {
            art: self.art,
            index: range.0,
            count: range.1,
        }
    }

    pub fn ctrl1_weights(&self) -> WeightIter<'a> {
        self.weights(self.ctrl1_weight_range())
    }

    pub fn ctrl2_weights(&self) -> WeightIter<'a> {
        self.weights(self.ctrl2_weight_range())
    }

    pub fn to_weights(&self) -> WeightIter<'a> {
        self.weights(self.to_weight_range())
    }
}

/// One `(bone, weight)` pair.
#[derive(Clone, Copy, Debug)]
pub struct WeightRef {
    pub bone: usize,
    pub weight: f32,
}

/// Iterator over a contiguous range of weights.
#[derive(Clone, Copy, Debug)]
pub struct WeightIter<'a> {
    art: Viveco<'a>,
    index: usize,
    count: usize,
}

impl Iterator for WeightIter<'_> {
    type Item = WeightRef;

    fn next(&mut self) -> Option<Self::Item> {
        if self.count == 0 {
            return None;
        }
        let weight = self.art.weight(self.index);
        self.index += 1;
        self.count -= 1;
        Some(weight)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.count, Some(self.count))
    }
}

impl ExactSizeIterator for WeightIter<'_> {}

/// A bone entry. Parents always precede their children.
#[derive(Clone, Copy, Debug)]
pub struct BoneRef<'a> {
    art: Viveco<'a>,
    index: usize,
}

impl<'a> BoneRef<'a> {
    pub fn index(&self) -> usize {
        self.index
    }

    fn entry(&self) -> &'a [u8] {
        self.art.bone_entry(self.index)
    }

    /// Rest-space head, with connected heads already resolved to the parent's
    /// tail by the compiler.
    pub fn head(&self) -> [f32; 2] {
        let entry = self.entry();
        [read_f32(entry, B_HEAD_X), read_f32(entry, B_HEAD_Y)]
    }

    pub fn tail(&self) -> [f32; 2] {
        let entry = self.entry();
        [read_f32(entry, B_TAIL_X), read_f32(entry, B_TAIL_Y)]
    }

    pub fn parent(&self) -> Option<usize> {
        let parent = read_u32(self.entry(), B_PARENT);
        (parent != NO_PARENT).then_some(parent as usize)
    }

    /// Whether the document omitted this bone's head, making it the parent's
    /// live tail joint. The resolved head is the same either way.
    pub fn connected(&self) -> bool {
        read_u32(self.entry(), B_FLAGS) & B_CONNECTED != 0
    }
}

/// An animation clip.
#[derive(Clone, Copy, Debug)]
pub struct AnimationRef<'a> {
    art: Viveco<'a>,
    index: usize,
}

impl<'a> AnimationRef<'a> {
    pub fn index(&self) -> usize {
        self.index
    }

    fn entry(&self) -> &'a [u8] {
        self.art.animation_entry(self.index)
    }

    pub fn duration(&self) -> f32 {
        read_f32(self.entry(), A_DURATION)
    }

    pub fn cyclic(&self) -> bool {
        read_u32(self.entry(), A_FLAGS) & A_CYCLIC != 0
    }

    fn curve_start(&self) -> usize {
        read_u32(self.entry(), A_CURVE_START) as usize
    }

    fn curve_count_raw(&self) -> usize {
        read_u32(self.entry(), A_CURVE_COUNT) as usize
    }

    pub fn curves(&self) -> impl Iterator<Item = CurveRef<'a>> {
        let art = self.art;
        let start = self.curve_start();
        let end = start + self.curve_count_raw();
        (start..end).filter_map(move |index| art.curve(index))
    }

    pub fn curve_count(&self) -> usize {
        self.curve_count_raw()
    }
}

/// One animated property of one bone.
#[derive(Clone, Copy, Debug)]
pub struct CurveRef<'a> {
    art: Viveco<'a>,
    index: usize,
}

impl<'a> CurveRef<'a> {
    pub fn index(&self) -> usize {
        self.index
    }

    fn entry(&self) -> &'a [u8] {
        self.art.curve_entry(self.index)
    }

    pub fn bone(&self) -> usize {
        read_u32(self.entry(), C_BONE) as usize
    }

    fn property_code(&self) -> u32 {
        read_u32(self.entry(), C_PROPERTY)
    }

    pub fn property(&self) -> Option<BoneProperty> {
        BoneProperty::from_code(self.property_code())
    }

    fn key_start(&self) -> usize {
        read_u32(self.entry(), C_KEY_START) as usize
    }

    fn key_count(&self) -> usize {
        read_u32(self.entry(), C_KEY_COUNT) as usize
    }

    pub fn len(&self) -> usize {
        self.key_count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The keyframe at `index`, relative to this curve.
    pub fn keyframe(&self, index: usize) -> Option<KeyframeRef<'a>> {
        if index >= self.key_count() {
            return None;
        }
        let frame = self.key_start() + index;
        Some(KeyframeRef {
            entry: self.art.keyframe_entry(frame),
        })
    }

    pub fn keyframes(&self) -> impl Iterator<Item = KeyframeRef<'a>> {
        let art = self.art;
        let start = self.key_start();
        let end = start + self.key_count();
        (start..end).map(move |index| KeyframeRef {
            entry: art.keyframe_entry(index),
        })
    }
}

/// One value of a curve.
#[derive(Clone, Copy, Debug)]
pub struct KeyframeRef<'a> {
    entry: &'a [u8],
}

impl<'a> KeyframeRef<'a> {
    pub fn timestamp(&self) -> f32 {
        read_f32(self.entry, K_TIME)
    }

    pub fn value(&self) -> f32 {
        read_f32(self.entry, K_VALUE)
    }

    pub fn handle_in(&self) -> Option<f32> {
        read_handle(self.entry, K_HANDLE_IN)
    }

    pub fn handle_out(&self) -> Option<f32> {
        read_handle(self.entry, K_HANDLE_OUT)
    }
}

impl<'a> Viveco<'a> {
    fn path(&self, index: usize) -> Option<PathRef<'a>> {
        (index < self.counts.paths).then_some(PathRef { art: *self, index })
    }

    fn segment(&self, index: usize) -> Option<SegmentRef<'a>> {
        (index < self.counts.segments).then_some(SegmentRef { art: *self, index })
    }

    fn weight(&self, index: usize) -> WeightRef {
        let entry = self.weight_entry(index.min(self.counts.weights.saturating_sub(1)));
        WeightRef {
            bone: read_u32(entry, W_BONE) as usize,
            weight: read_f32(entry, W_WEIGHT),
        }
    }

    fn curve(&self, index: usize) -> Option<CurveRef<'a>> {
        (index < self.counts.curves).then_some(CurveRef { art: *self, index })
    }
}

/// Axis-aligned bounds in document space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl Bounds {
    pub fn width(&self) -> f32 {
        self.max[0] - self.min[0]
    }

    pub fn height(&self) -> f32 {
        self.max[1] - self.min[1]
    }

    pub fn center(&self) -> [f32; 2] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
        ]
    }
}

#[derive(Default)]
struct BoundsAccumulator {
    min: [f32; 2],
    max: [f32; 2],
    any: bool,
}

impl BoundsAccumulator {
    fn add_point(&mut self, point: [f32; 2]) {
        if !point[0].is_finite() || !point[1].is_finite() {
            return;
        }
        if self.any {
            self.min[0] = self.min[0].min(point[0]);
            self.min[1] = self.min[1].min(point[1]);
            self.max[0] = self.max[0].max(point[0]);
            self.max[1] = self.max[1].max(point[1]);
        } else {
            self.min = point;
            self.max = point;
            self.any = true;
        }
    }

    fn add_shape(&mut self, shape: ShapeRef<'_>) {
        for path in shape.paths() {
            if !path.is_closed() {
                self.add_point(path.stored_start());
            }
            for segment in path.segments() {
                if let Some(ctrl) = segment.ctrl1() {
                    self.add_point(ctrl);
                }
                if let Some(ctrl) = segment.ctrl2() {
                    self.add_point(ctrl);
                }
                self.add_point(segment.to());
            }
        }
        for child in shape.children() {
            self.add_shape(child);
        }
    }

    fn finish(self) -> Option<Bounds> {
        self.any.then_some(Bounds {
            min: self.min,
            max: self.max,
        })
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_f32(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_handle(bytes: &[u8], offset: usize) -> Option<f32> {
    let value = read_f32(bytes, offset);
    (!value.is_nan()).then_some(value)
}
