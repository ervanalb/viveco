//! JSON document to binary compiler.
//!
//! This runs inside the proc macro at compile time. It flattens the shape and
//! bone trees, replaces every name with an index, sorts siblings by Z, and
//! groups animation keyframes into per-target curves.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::json::{
    Animation, Bone, Document, FORMAT_TYPE, FORMAT_VERSION, Keyframe, MIN_VERSION, Path, PathEnds,
    Segment, Shape, Style, Target, Weight,
};

// The constants below are mirrored in `viveco::format`. The layout is validated
// by the core reader and covered by the crate's tests.
#[allow(dead_code)]
mod layout {
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

    pub const MAGIC: &[u8; 8] = b"VIVECO01";
    pub const VERSION: u32 = 1;

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

    pub const NO_PARENT: u32 = u32::MAX;
    pub const NONE_HANDLE: f32 = f32::NAN;
}

use layout::*;

/// Compiles a parsed document into the viveco binary format.
pub fn compile(document: &Document) -> Result<Vec<u8>, String> {
    if document.kind != FORMAT_TYPE {
        return Err(format!(
            "expected a {FORMAT_TYPE:?} document, found type {:?}",
            document.kind
        ));
    }
    if !(MIN_VERSION..=FORMAT_VERSION).contains(&document.version) {
        return Err(format!(
            "unsupported viveco version {} (supported {MIN_VERSION}..={FORMAT_VERSION})",
            document.version
        ));
    }

    let mut bones = Vec::new();
    let mut bone_names = HashMap::new();
    flatten_bones(&document.bones, None, None, &mut bones, &mut bone_names)?;

    let animations = compile_animations(&document.animations, &bone_names)?;

    let mut roots: Vec<ShapeIr> = document
        .shapes
        .iter()
        .map(|shape| shape_ir(shape, &bone_names))
        .collect::<Result<_, _>>()?;
    sort_by_z(&mut roots);

    let mut next_id = 0;
    assign_ids(&mut roots, &mut next_id);
    let root_ids: Vec<u32> = roots.iter().map(|shape| shape.id as u32).collect();

    let mut flat = Vec::with_capacity(next_id);
    flatten_shapes(&roots, &mut flat);

    let mut shapes_bytes = Vec::with_capacity(flat.len() * SHAPE_SIZE);
    let mut paths_bytes = Vec::new();
    let mut segments_bytes = Vec::new();
    let mut weights_bytes = Vec::new();
    let mut children = Vec::new();

    let mut path_count = 0usize;
    let mut segment_count = 0usize;
    let mut weight_count = 0usize;

    for shape in &flat {
        let path_start = path_count;
        for path in &shape.paths {
            encode_path(
                path,
                &mut paths_bytes,
                &mut segments_bytes,
                &mut weights_bytes,
                &mut path_count,
                &mut segment_count,
                &mut weight_count,
            );
        }
        let path_count_here = path_count - path_start;

        let child_start = children.len() / CHILD_SIZE;
        for child in &shape.children {
            push_u32(&mut children, child.id as u32);
        }
        let child_count_here = children.len() / CHILD_SIZE - child_start;

        push_f32(&mut shapes_bytes, shape.z);
        let mut flags = 0u32;
        if shape.fill.is_some() {
            flags |= 1;
        }
        if shape.stroke.is_some() {
            flags |= 2;
        }
        push_u32(&mut shapes_bytes, flags);
        push_u32(&mut shapes_bytes, shape.fill.unwrap_or(0));
        push_f32(
            &mut shapes_bytes,
            shape.stroke.map(|stroke| stroke.1).unwrap_or(0.0),
        );
        push_u32(
            &mut shapes_bytes,
            shape.stroke.map(|stroke| stroke.0).unwrap_or(0),
        );
        push_u32(&mut shapes_bytes, path_start as u32);
        push_u32(&mut shapes_bytes, path_count_here as u32);
        push_u32(&mut shapes_bytes, child_start as u32);
        push_u32(&mut shapes_bytes, child_count_here as u32);
        push_u32(&mut shapes_bytes, 0);
    }

    let root_start = children.len() / CHILD_SIZE;
    for id in &root_ids {
        push_u32(&mut children, *id);
    }
    let root_count = children.len() / CHILD_SIZE - root_start;

    let mut bones_bytes = Vec::with_capacity(bones.len() * BONE_SIZE);
    for bone in &bones {
        push_point(&mut bones_bytes, bone.head);
        push_point(&mut bones_bytes, bone.tail);
        push_u32(
            &mut bones_bytes,
            bone.parent.map(|parent| parent as u32).unwrap_or(NO_PARENT),
        );
        push_u32(&mut bones_bytes, u32::from(bone.connected));
    }

    let mut animations_bytes = Vec::new();
    let mut curves_bytes = Vec::new();
    let mut keyframes_bytes = Vec::new();
    let mut curve_count = 0usize;
    let mut keyframe_count = 0usize;

    for animation in &animations {
        let curve_start = curve_count;
        for curve in &animation.curves {
            let key_start = keyframe_count;
            for key in &curve.keys {
                push_f32(&mut keyframes_bytes, key.time);
                push_f32(&mut keyframes_bytes, key.value);
                push_f32(&mut keyframes_bytes, key.handle_in.unwrap_or(NONE_HANDLE));
                push_f32(&mut keyframes_bytes, key.handle_out.unwrap_or(NONE_HANDLE));
                keyframe_count += 1;
            }
            let key_count = keyframe_count - key_start;

            push_u32(&mut curves_bytes, curve.bone);
            push_u32(&mut curves_bytes, curve.property);
            push_u32(&mut curves_bytes, key_start as u32);
            push_u32(&mut curves_bytes, key_count as u32);
            curve_count += 1;
        }
        let curve_count_here = curve_count - curve_start;

        push_f32(&mut animations_bytes, animation.duration);
        push_u32(&mut animations_bytes, u32::from(animation.cyclic));
        push_u32(&mut animations_bytes, curve_start as u32);
        push_u32(&mut animations_bytes, curve_count_here as u32);
    }

    let child_count = children.len() / CHILD_SIZE;
    let mut out = Vec::with_capacity(
        HEADER_SIZE
            + shapes_bytes.len()
            + paths_bytes.len()
            + segments_bytes.len()
            + weights_bytes.len()
            + children.len()
            + bones_bytes.len()
            + animations_bytes.len()
            + curves_bytes.len()
            + keyframes_bytes.len(),
    );
    out.resize(HEADER_SIZE, 0);
    out[..8].copy_from_slice(MAGIC);
    write_u32(&mut out, H_VERSION, VERSION);
    write_u32(&mut out, H_SHAPE_COUNT, flat.len() as u32);
    write_u32(&mut out, H_PATH_COUNT, path_count as u32);
    write_u32(&mut out, H_SEGMENT_COUNT, segment_count as u32);
    write_u32(&mut out, H_WEIGHT_COUNT, weight_count as u32);
    write_u32(&mut out, H_BONE_COUNT, bones.len() as u32);
    write_u32(&mut out, H_ANIMATION_COUNT, animations.len() as u32);
    write_u32(&mut out, H_CURVE_COUNT, curve_count as u32);
    write_u32(&mut out, H_KEYFRAME_COUNT, keyframe_count as u32);
    write_u32(&mut out, H_ROOT_START, root_start as u32);
    write_u32(&mut out, H_ROOT_COUNT, root_count as u32);
    write_u32(&mut out, H_CHILD_COUNT, child_count as u32);

    out.extend_from_slice(&shapes_bytes);
    out.extend_from_slice(&paths_bytes);
    out.extend_from_slice(&segments_bytes);
    out.extend_from_slice(&weights_bytes);
    out.extend_from_slice(&children);
    out.extend_from_slice(&bones_bytes);
    out.extend_from_slice(&animations_bytes);
    out.extend_from_slice(&curves_bytes);
    out.extend_from_slice(&keyframes_bytes);

    Ok(out)
}

#[derive(Clone, Copy)]
struct FlatBone {
    head: [f32; 2],
    tail: [f32; 2],
    parent: Option<usize>,
    connected: bool,
}

fn flatten_bones(
    bones: &[Bone],
    parent_tail: Option<[f32; 2]>,
    parent: Option<usize>,
    out: &mut Vec<FlatBone>,
    names: &mut HashMap<String, usize>,
) -> Result<(), String> {
    for bone in bones {
        let index = out.len();
        if !bone.name.is_empty() && names.insert(bone.name.clone(), index).is_some() {
            return Err(format!("duplicate bone name {:?}", bone.name));
        }
        let connected = bone.head.is_none();
        let head = bone.head.or(parent_tail).unwrap_or(bone.tail);
        out.push(FlatBone {
            head,
            tail: bone.tail,
            parent,
            connected,
        });
        flatten_bones(&bone.children, Some(bone.tail), Some(index), out, names)?;
    }
    Ok(())
}

struct AnimationData {
    duration: f32,
    cyclic: bool,
    curves: Vec<CurveData>,
}

struct CurveData {
    bone: u32,
    property: u32,
    keys: Vec<KeyData>,
}

struct KeyData {
    time: f32,
    value: f32,
    handle_in: Option<f32>,
    handle_out: Option<f32>,
}

fn compile_animations(
    animations: &[Animation],
    bone_names: &HashMap<String, usize>,
) -> Result<Vec<AnimationData>, String> {
    let mut out = Vec::with_capacity(animations.len());

    for animation in animations {
        let duration = if animation.duration.is_finite() && animation.duration > 0.0 {
            animation.duration
        } else {
            0.0
        };

        let mut keys: Vec<&Keyframe> = animation
            .keyframes
            .iter()
            .filter(|key| {
                key.timestamp.is_finite() && key.timestamp >= 0.0 && key.timestamp <= duration
            })
            .collect();
        keys.sort_by(|a, b| {
            a.timestamp
                .partial_cmp(&b.timestamp)
                .unwrap_or(Ordering::Equal)
        });

        let mut curves: Vec<CurveData> = Vec::new();
        for key in keys {
            let Target::Bone { bone, property } = &key.target else {
                // Shape animation is not implemented by the authoring format.
                continue;
            };
            let Some(&bone_index) = bone_names.get(bone) else {
                return Err(format!("animation keyframe targets unknown bone {bone:?}"));
            };
            let property = property.code();

            let curve = match curves
                .iter_mut()
                .find(|curve| curve.bone == bone_index as u32 && curve.property == property)
            {
                Some(curve) => curve,
                None => {
                    curves.push(CurveData {
                        bone: bone_index as u32,
                        property,
                        keys: Vec::new(),
                    });
                    curves.last_mut().expect("just pushed")
                }
            };
            curve.keys.push(KeyData {
                time: key.timestamp,
                value: key.value,
                handle_in: key.handle_in,
                handle_out: key.handle_out,
            });
        }

        out.push(AnimationData {
            duration,
            cyclic: animation.cyclic,
            curves,
        });
    }

    Ok(out)
}

struct ShapeIr {
    id: usize,
    z: f32,
    fill: Option<u32>,
    stroke: Option<(u32, f32)>,
    paths: Vec<PathIr>,
    children: Vec<ShapeIr>,
}

struct PathIr {
    closed: bool,
    start: [f32; 2],
    start_weights: Vec<(u32, f32)>,
    segments: Vec<SegmentIr>,
}

struct SegmentIr {
    ctrl1: Option<[f32; 2]>,
    ctrl2: Option<[f32; 2]>,
    to: [f32; 2],
    ctrl1_weights: Vec<(u32, f32)>,
    ctrl2_weights: Vec<(u32, f32)>,
    to_weights: Vec<(u32, f32)>,
}

fn shape_ir(shape: &Shape, bone_names: &HashMap<String, usize>) -> Result<ShapeIr, String> {
    let mut children = shape
        .children
        .iter()
        .map(|child| shape_ir(child, bone_names))
        .collect::<Result<Vec<_>, _>>()?;
    sort_by_z(&mut children);

    let paths = shape
        .paths
        .iter()
        .map(|path| path_ir(path, bone_names))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ShapeIr {
        id: 0,
        z: shape.z,
        fill: shape.style.fill.as_deref().map(parse_color).transpose()?,
        stroke: stroke_ir(&shape.style)?,
        paths,
        children,
    })
}

fn stroke_ir(style: &Style) -> Result<Option<(u32, f32)>, String> {
    match &style.stroke {
        Some(stroke) => Ok(Some((parse_color(&stroke.color)?, stroke.width))),
        None => Ok(None),
    }
}

fn path_ir(path: &Path, bone_names: &HashMap<String, usize>) -> Result<PathIr, String> {
    let (closed, start) = match path.ends {
        PathEnds::Open(start) => (false, start),
        PathEnds::Closed => (
            true,
            path.segments
                .last()
                .map(|segment| segment.to)
                .unwrap_or([0.0, 0.0]),
        ),
    };

    let start_weights = if closed {
        Vec::new()
    } else {
        raw_weights(&path.start_weights, bone_names)?
    };

    let segments = path
        .segments
        .iter()
        .map(|segment| segment_ir(segment, bone_names))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(PathIr {
        closed,
        start,
        start_weights,
        segments,
    })
}

fn segment_ir(segment: &Segment, bone_names: &HashMap<String, usize>) -> Result<SegmentIr, String> {
    Ok(SegmentIr {
        ctrl1: segment.ctrl1,
        ctrl2: segment.ctrl2,
        to: segment.to,
        ctrl1_weights: raw_weights(&segment.ctrl1_weights, bone_names)?,
        ctrl2_weights: raw_weights(&segment.ctrl2_weights, bone_names)?,
        to_weights: raw_weights(&segment.to_weights, bone_names)?,
    })
}

fn raw_weights(
    weights: &[Weight],
    bone_names: &HashMap<String, usize>,
) -> Result<Vec<(u32, f32)>, String> {
    weights
        .iter()
        .map(|weight| {
            let Some(&bone) = bone_names.get(&weight.bone) else {
                return Err(format!("weight references unknown bone {:?}", weight.bone));
            };
            Ok((bone as u32, weight.weight))
        })
        .collect()
}

fn sort_by_z<T>(shapes: &mut [T])
where
    T: BorrowZ,
{
    shapes.sort_by(|a, b| a.z().partial_cmp(&b.z()).unwrap_or(Ordering::Equal));
}

trait BorrowZ {
    fn z(&self) -> f32;
}

impl BorrowZ for ShapeIr {
    fn z(&self) -> f32 {
        self.z
    }
}

fn assign_ids(shapes: &mut [ShapeIr], next: &mut usize) {
    for shape in shapes {
        shape.id = *next;
        *next += 1;
        assign_ids(&mut shape.children, next);
    }
}

fn flatten_shapes<'a>(shapes: &'a [ShapeIr], out: &mut Vec<&'a ShapeIr>) {
    for shape in shapes {
        out.push(shape);
        flatten_shapes(&shape.children, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn encode_path(
    path: &PathIr,
    paths: &mut Vec<u8>,
    segments: &mut Vec<u8>,
    weights: &mut Vec<u8>,
    path_count: &mut usize,
    segment_count: &mut usize,
    weight_count: &mut usize,
) {
    let start_weight_start = *weight_count;
    for &(bone, weight) in &path.start_weights {
        push_u32(weights, bone);
        push_f32(weights, weight);
        *weight_count += 1;
    }
    let start_weight_count = *weight_count - start_weight_start;

    let segment_start = *segment_count;
    for segment in &path.segments {
        encode_segment(segment, segments, weights, segment_count, weight_count);
    }
    let segment_count_here = *segment_count - segment_start;

    push_u32(paths, u32::from(path.closed));
    push_u32(paths, 0);
    push_point(paths, path.start);
    push_u32(paths, segment_start as u32);
    push_u32(paths, segment_count_here as u32);
    push_u32(paths, start_weight_start as u32);
    push_u32(paths, start_weight_count as u32);

    *path_count += 1;
}

fn encode_segment(
    segment: &SegmentIr,
    segments: &mut Vec<u8>,
    weights: &mut Vec<u8>,
    segment_count: &mut usize,
    weight_count: &mut usize,
) {
    let mut ranges = [0u32; 6];
    for (slot, list) in [
        &segment.ctrl1_weights,
        &segment.ctrl2_weights,
        &segment.to_weights,
    ]
    .into_iter()
    .enumerate()
    {
        let start = *weight_count;
        for &(bone, weight) in list {
            push_u32(weights, bone);
            push_f32(weights, weight);
            *weight_count += 1;
        }
        ranges[slot * 2] = start as u32;
        ranges[slot * 2 + 1] = (*weight_count - start) as u32;
    }

    let ctrl1 = segment.ctrl1.unwrap_or([0.0, 0.0]);
    let ctrl2 = segment.ctrl2.unwrap_or([0.0, 0.0]);
    push_point(segments, ctrl1);
    push_point(segments, ctrl2);
    push_point(segments, segment.to);
    let mut flags = 0u32;
    if segment.ctrl1.is_some() {
        flags |= 1;
    }
    if segment.ctrl2.is_some() {
        flags |= 2;
    }
    push_u32(segments, flags);
    for value in ranges {
        push_u32(segments, value);
    }

    *segment_count += 1;
}

fn parse_color(text: &str) -> Result<u32, String> {
    let hex = text.trim().trim_start_matches('#');
    if !hex.is_ascii() {
        return Err(format!("invalid color {text:?}"));
    }
    let component = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&hex[range], 16).map_err(|_| format!("invalid color {text:?}"))
    };
    let (r, g, b, a) = match hex.len() {
        3 => {
            let digit = |i: usize| {
                u8::from_str_radix(&hex[i..i + 1], 16)
                    .map(|value| value * 17)
                    .map_err(|_| format!("invalid color {text:?}"))
            };
            (digit(0)?, digit(1)?, digit(2)?, 255)
        }
        6 => (component(0..2)?, component(2..4)?, component(4..6)?, 255),
        8 => (
            component(0..2)?,
            component(2..4)?,
            component(4..6)?,
            component(6..8)?,
        ),
        _ => return Err(format!("invalid color {text:?}")),
    };
    Ok(((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | a as u32)
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_f32(bytes: &mut Vec<u8>, value: f32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_point(bytes: &mut Vec<u8>, point: [f32; 2]) {
    push_f32(bytes, point[0]);
    push_f32(bytes, point[1]);
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
