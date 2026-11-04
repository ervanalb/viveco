//! Geometry bounds used for framing and input mapping.
//!
//! The asset format no longer carries a bounds type: it is a presentation
//! concern, so the editor owns the accumulation and fallback rules here.

use viveco::{Asset, Path, Point, Shape};

/// Axis-aligned rectangle in asset space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: Point,
    pub max: Point,
}

impl Bounds {
    pub fn width(&self) -> f32 {
        self.max[0] - self.min[0]
    }

    pub fn height(&self) -> f32 {
        self.max[1] - self.min[1]
    }

    pub fn center(&self) -> Point {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
        ]
    }
}

/// Bounds used to map input before an empty asset has any geometry to fit.
pub const EMPTY_BOUNDS: Bounds = Bounds {
    min: [0.0, 0.0],
    max: [1000.0, 1000.0],
};

/// Bounds used to fit and map input for `asset`, falling back to
/// [`EMPTY_BOUNDS`] when it has no geometry.
pub fn bounds_for(asset: &Asset) -> Bounds {
    let mut acc = BoundsAccumulator::default();
    for shape in &asset.shapes {
        acc.add_shape(shape);
    }
    acc.finish().unwrap_or(EMPTY_BOUNDS)
}

/// Axis-aligned bounds of all geometry in `asset`, including nested children.
///
/// Bézier control points are included, so the result is a conservative bound.
pub fn bounds_of(asset: &Asset) -> Option<Bounds> {
    let mut acc = BoundsAccumulator::default();
    for shape in &asset.shapes {
        acc.add_shape(shape);
    }
    acc.finish()
}

#[derive(Default)]
struct BoundsAccumulator {
    min: Point,
    max: Point,
    any: bool,
}

impl BoundsAccumulator {
    fn add_point(&mut self, point: Point) {
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

    fn add_path(&mut self, path: &Path) {
        if let Some(start) = path.start() {
            self.add_point(start);
        }
        for segment in &path.segments {
            if let Some(ctrl1) = segment.ctrl1 {
                self.add_point(ctrl1);
            }
            if let Some(ctrl2) = segment.ctrl2 {
                self.add_point(ctrl2);
            }
            self.add_point(segment.to);
        }
    }

    fn add_shape(&mut self, shape: &Shape) {
        for path in &shape.paths {
            self.add_path(path);
        }
        for child in &shape.children {
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

#[cfg(test)]
mod tests {
    use super::*;
    use viveco::Path;

    #[test]
    fn bounds_include_nested_shapes_and_controls() {
        let asset = Asset {
            shapes: vec![Shape {
                paths: vec![Path::new([10.0, 20.0]).cubic_to(
                    [0.0, 0.0],
                    [40.0, 50.0],
                    [30.0, 30.0],
                )],
                children: vec![Shape {
                    paths: vec![Path::new([-5.0, 25.0]).line_to([15.0, 60.0])],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        let bounds = bounds_of(&asset).unwrap();
        assert_eq!(bounds.min, [-5.0, 0.0]);
        assert_eq!(bounds.max, [40.0, 60.0]);
    }

    #[test]
    fn empty_assets_fall_back_to_the_default_bounds() {
        assert_eq!(bounds_for(&Asset::new()), EMPTY_BOUNDS);
        assert_eq!(bounds_of(&Asset::new()), None);
    }
}
