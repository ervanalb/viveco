//! The checked-in example documents must load under the current format.

use std::path::Path as FsPath;

use viveco::{Asset, KeyframeTarget, Path, Shape, Weights};

fn visit_shape(shape: &Shape, bones: usize, source: &FsPath) {
    for path in &shape.paths {
        visit_weights(path, bones, source);
    }
    for child in &shape.children {
        visit_shape(child, bones, source);
    }
}

fn visit_weights(path: &Path, bones: usize, source: &FsPath) {
    assert_weights(&path.start_weights, bones, source);
    for segment in &path.segments {
        assert_weights(&segment.ctrl1_weights, bones, source);
        assert_weights(&segment.ctrl2_weights, bones, source);
        assert_weights(&segment.to_weights, bones, source);
    }
}

fn assert_weights(weights: &Weights, bones: usize, source: &FsPath) {
    for weight in weights.iter() {
        assert!(
            weight.bone < bones,
            "{} refers to bone {} but only {} exist",
            source.display(),
            weight.bone,
            bones
        );
    }
}

#[test]
fn examples_load_as_the_current_format() {
    let dir = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("../examples");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("examples directory") {
        let path = entry.expect("directory entry").path();
        let extension = path.extension().and_then(|ext| ext.to_str());
        if !matches!(extension, Some("json" | "viveco")) {
            continue;
        }

        let asset = Asset::load_from_file(&path)
            .unwrap_or_else(|err| panic!("failed to load {}: {err}", path.display()));

        let bones = asset.bone_count();
        for shape in &asset.shapes {
            visit_shape(shape, bones, &path);
        }
        for animation in &asset.animations {
            for keyframe in &animation.keyframes {
                if let KeyframeTarget::Bone { bone, .. } = keyframe.target {
                    assert!(
                        bone < bones,
                        "{} keys bone {} but only {} exist",
                        path.display(),
                        bone,
                        bones
                    );
                }
            }
        }

        // A loaded example must survive a round trip unchanged.
        let json = serde_json::to_string(&asset).expect("serialize");
        assert_eq!(
            Asset::from_json_str(&json).expect("deserialize"),
            asset,
            "{}",
            path.display()
        );
        checked += 1;
    }
    assert!(checked > 0, "no examples found in {}", dir.display());
}
