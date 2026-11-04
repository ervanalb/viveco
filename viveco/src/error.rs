//! Errors produced while validating a binary document.

use thiserror::Error;

/// Why a byte slice could not be read as a viveco document.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FormatError {
    #[error("file is too short to contain a viveco header")]
    TooShort,
    #[error("not a viveco file (bad magic)")]
    BadMagic,
    #[error("unsupported viveco binary version {0}")]
    UnsupportedVersion(u32),
    #[error("file is {len} bytes but its sections need at least {needed}")]
    Truncated { len: usize, needed: usize },
    #[error("shape {shape} has an out-of-range path range")]
    ShapePaths { shape: usize },
    #[error("shape {shape} has an out-of-range child range")]
    ShapeChildren { shape: usize },
    #[error("shape {shape} refers to child shape {child} (only {count} shapes)")]
    ShapeChildIndex {
        shape: usize,
        child: usize,
        count: usize,
    },
    #[error("path {path} has an out-of-range segment range")]
    PathSegments { path: usize },
    #[error("path {path} has an out-of-range weight range")]
    PathWeights { path: usize },
    #[error("path {path} has an invalid ends flag {flag}")]
    PathEnds { path: usize, flag: u32 },
    #[error("segment {segment} has an out-of-range weight range")]
    SegmentWeights { segment: usize },
    #[error("weight {weight} refers to bone {bone} (only {count} bones)")]
    WeightBone {
        weight: usize,
        bone: usize,
        count: usize,
    },
    #[error("bone {bone} has an invalid parent {parent}")]
    BoneParent { bone: usize, parent: usize },
    #[error("animation {animation} has an out-of-range curve range")]
    AnimationCurves { animation: usize },
    #[error("curve {curve} refers to bone {bone} (only {count} bones)")]
    CurveBone {
        curve: usize,
        bone: usize,
        count: usize,
    },
    #[error("curve {curve} has an invalid property {property}")]
    CurveProperty { curve: usize, property: u32 },
    #[error("curve {curve} has an out-of-range keyframe range")]
    CurveKeyframes { curve: usize },
    #[error("root shape range is out of bounds")]
    RootShapes,
}
