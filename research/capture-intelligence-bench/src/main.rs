use capture_intelligence::{
    analysis_cache_key, build_similarity_groups, fingerprint_image, technical_evidence,
    AnalysisImage, GroupingInput, ANALYSIS_SETTINGS_VERSION,
};
use media_model::MediaType;
use media_visual::{prepare_analysis_preview, ArtifactStatus, LocalVisualAdapters};
use serde::Serialize;
use std::{env, fs, path::PathBuf, process::Command, time::Instant};
use tempfile::tempdir;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkReport {
    suite: String,
    images_analyzed: usize,
    analysis_elapsed_ms: u128,
    images_per_second: f64,
    grouping_inputs: usize,
    grouping_elapsed_ms: u128,
    groups_created: usize,
    cache_key_reuse_elapsed_ms: u128,
    analysis_preview_generation_elapsed_ms: Option<u128>,
    analysis_preview_cache_reuse_elapsed_ms: Option<u128>,
    analysis_preview_benchmark: &'static str,
    peak_memory_bytes: Option<u64>,
    face_eye_evaluation: &'static str,
    notes: Vec<&'static str>,
}

fn main() {
    let mut arguments = env::args().skip(1);
    let mut suite = "baseline".to_owned();
    let mut output = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--suite" => {
                suite = arguments
                    .next()
                    .unwrap_or_else(|| exit_usage("--suite needs a value"))
            }
            "--output" => {
                output = Some(PathBuf::from(
                    arguments
                        .next()
                        .unwrap_or_else(|| exit_usage("--output needs a path")),
                ))
            }
            "--help" | "-h" => {
                println!("Usage: cargo run -p capture-intelligence-bench -- --suite baseline [--output report.json]");
                return;
            }
            _ => exit_usage("unknown argument"),
        }
    }
    if suite != "baseline" {
        exit_usage("only the deterministic baseline suite is available");
    }
    validate_annotation_scaffolding();
    let analysis_preview = measure_analysis_preview();

    let images = [
        checkerboard(96, 96),
        checkerboard(192, 192),
        gradient(128, 96),
        solid(128, 96, [0, 0, 0]),
        solid(128, 96, [255, 255, 255]),
    ];
    let started = Instant::now();
    let evidence = images
        .iter()
        .map(|image| (fingerprint_image(image), technical_evidence(image)))
        .collect::<Vec<_>>();
    let analysis_elapsed = started.elapsed();

    // 1,001 descriptors deliberately share an LSH band. The grouping implementation caps that
    // bucket rather than turning it into a naive all-vs-all comparison.
    let mut grouping_inputs = (0..1_001)
        .map(|index| GroupingInput {
            project_id: "bench-project".into(),
            asset_id: format!("asset-{index:04}"),
            verified_content_hash: None,
            captured_at_unix_seconds: Some(1_700_000_000 + index as i64 * 60),
            camera_model: Some("bench-camera".into()),
            fingerprint: evidence[index % 2].0.clone(),
            technical_quality_score: Some(evidence[index % 2].1.technical_quality_score),
        })
        .collect::<Vec<_>>();
    // A separate, small time/camera set proves that the same bounded run still builds a useful
    // deterministic group instead of reporting only the adversarial oversized bucket.
    for index in 0..3 {
        grouping_inputs.push(GroupingInput {
            project_id: "bench-project".into(),
            asset_id: format!("controlled-related-{index}"),
            verified_content_hash: None,
            captured_at_unix_seconds: Some(1_800_000_000 + index),
            camera_model: Some("controlled-camera".into()),
            fingerprint: evidence[2].0.clone(),
            technical_quality_score: Some(evidence[2].1.technical_quality_score),
        });
    }
    let grouping_started = Instant::now();
    let groups = build_similarity_groups(&grouping_inputs);
    let grouping_elapsed = grouping_started.elapsed();

    let cache_started = Instant::now();
    for _ in 0..10_000 {
        let _ = analysis_cache_key(
            "synthetic-preview-fingerprint",
            "captureos-deterministic-image",
            "m4.det.v1",
            ANALYSIS_SETTINGS_VERSION,
        );
    }
    let cache_elapsed = cache_started.elapsed();
    let report = BenchmarkReport {
        suite,
        images_analyzed: images.len(),
        analysis_elapsed_ms: analysis_elapsed.as_millis(),
        images_per_second: images.len() as f64 / analysis_elapsed.as_secs_f64().max(f64::EPSILON),
        grouping_inputs: grouping_inputs.len(),
        grouping_elapsed_ms: grouping_elapsed.as_millis(),
        groups_created: groups.len(),
        cache_key_reuse_elapsed_ms: cache_elapsed.as_millis(),
        analysis_preview_generation_elapsed_ms: analysis_preview.generation_elapsed_ms,
        analysis_preview_cache_reuse_elapsed_ms: analysis_preview.cache_reuse_elapsed_ms,
        analysis_preview_benchmark: analysis_preview.status,
        peak_memory_bytes: peak_memory_bytes(),
        face_eye_evaluation: "not_evaluated: no licensed face/eye ground-truth fixture or bundled identity model",
        notes: vec![
            "All rasters are generated in memory; no customer or client media is used.",
            "This benchmark measures deterministic image evidence and bounded candidate generation, not creative quality.",
            "On macOS it also measures one generated 3000×2000 JPEG's dedicated analysis-preview preparation and a cache-hit reuse; no customer media is used.",
            "Face detection is a local optional platform provider and is excluded from this synthetic suite.",
        ],
    };
    let json = serde_json::to_string_pretty(&report).expect("benchmark report serializes");
    println!("{json}");
    if let Some(output) = output {
        fs::write(output, format!("{json}\n")).unwrap_or_else(|error| {
            eprintln!("Could not write benchmark report: {error}");
            std::process::exit(1);
        });
    }
}

struct AnalysisPreviewMeasurement {
    generation_elapsed_ms: Option<u128>,
    cache_reuse_elapsed_ms: Option<u128>,
    status: &'static str,
}

fn measure_analysis_preview() -> AnalysisPreviewMeasurement {
    if !cfg!(target_os = "macos") {
        return AnalysisPreviewMeasurement {
            generation_elapsed_ms: None,
            cache_reuse_elapsed_ms: None,
            status: "not_measured: no bundled macOS image adapter on this platform",
        };
    }

    let directory = tempdir().expect("temporary benchmark directory");
    let ppm = directory.path().join("generated.ppm");
    let source = directory.path().join("generated.jpg");
    let cache = directory.path().join("preview-cache");
    write_generated_ppm(&ppm, 3_000, 2_000);
    let converted = Command::new("/usr/bin/sips")
        .args(["-s", "format", "jpeg", "-s", "formatOptions", "80", "--out"])
        .arg(&source)
        .arg(&ppm)
        .status()
        .expect("macOS sips is available for the benchmark");
    assert!(converted.success(), "benchmark JPEG conversion succeeds");
    let original = fs::read(&source).expect("read generated source");

    let generation_started = Instant::now();
    let generated = prepare_analysis_preview(
        &LocalVisualAdapters,
        &cache,
        "benchmark-asset",
        "benchmark-instance",
        &source,
        &MediaType::Jpeg,
        "generated-jpeg-v1",
    )
    .expect("prepare benchmark analysis preview");
    assert_eq!(generated.status, ArtifactStatus::Ready);
    assert!(cache.join(&generated.cache_relative_path).is_file());
    assert_eq!(
        fs::read(&source).expect("re-read generated source"),
        original
    );

    let reuse_started = Instant::now();
    let reused = prepare_analysis_preview(
        &LocalVisualAdapters,
        &cache,
        "benchmark-asset",
        "benchmark-instance",
        &source,
        &MediaType::Jpeg,
        "generated-jpeg-v1",
    )
    .expect("reuse benchmark analysis preview");
    assert_eq!(reused.status, ArtifactStatus::Ready);
    assert_eq!(reused.provider, "cache");
    assert_eq!(
        fs::read(&source).expect("verify source remains unchanged"),
        original
    );

    AnalysisPreviewMeasurement {
        generation_elapsed_ms: Some(generation_started.elapsed().as_millis()),
        cache_reuse_elapsed_ms: Some(reuse_started.elapsed().as_millis()),
        status: "measured: generated 3000x2000 JPEG through the local analysis-preview adapter",
    }
}

fn write_generated_ppm(path: &std::path::Path, width: u32, height: u32) {
    let mut raster = format!("P6\n{width} {height}\n255\n").into_bytes();
    raster.reserve((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let checker = if (((x / 80) + (y / 80)) & 1) == 0 {
                24
            } else {
                232
            };
            raster.extend_from_slice(&[
                checker,
                ((x * 255) / width.max(1)) as u8,
                ((y * 255) / height.max(1)) as u8,
            ]);
        }
    }
    fs::write(path, raster).expect("write generated benchmark PPM");
}

fn exit_usage(message: &str) -> ! {
    eprintln!("{message}\nUsage: cargo run -p capture-intelligence-bench -- --suite baseline [--output report.json]");
    std::process::exit(2)
}

fn solid(width: u32, height: u32, color: [u8; 3]) -> AnalysisImage {
    AnalysisImage::solid(width, height, color)
}

fn checkerboard(width: u32, height: u32) -> AnalysisImage {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let value = if (((x * 8 / width) + (y * 8 / height)) & 1) == 0 {
                24
            } else {
                232
            };
            rgb.extend_from_slice(&[value, value, value]);
        }
    }
    AnalysisImage::new(width, height, rgb).expect("valid generated checkerboard")
}

fn gradient(width: u32, height: u32) -> AnalysisImage {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let r = (x * 255 / width.max(1)) as u8;
            let g = (y * 255 / height.max(1)) as u8;
            rgb.extend_from_slice(&[r, g, 128]);
        }
    }
    AnalysisImage::new(width, height, rgb).expect("valid generated gradient")
}

fn validate_annotation_scaffolding() {
    for document in [
        include_str!("../ground-truth/v1/duplicate-pairs.json"),
        include_str!("../ground-truth/v1/similarity-groups.json"),
        include_str!("../ground-truth/v1/sharpness-preferences.json"),
        include_str!("../ground-truth/v1/face-presence.json"),
        include_str!("../ground-truth/v1/eye-state.json"),
        include_str!("../ground-truth/v1/candidate-preferences.json"),
        include_str!("../ground-truth/v1/human-review-agreement.json"),
    ] {
        let value: serde_json::Value =
            serde_json::from_str(document).expect("versioned ground truth JSON is valid");
        assert_eq!(
            value["formatVersion"], 1,
            "ground truth format must be versioned"
        );
    }
}

#[cfg(unix)]
fn peak_memory_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // macOS reports ru_maxrss in bytes; Linux reports KiB. This app's initial desktop target is
    // macOS, so use the platform's documented byte unit and leave other Unix values unreported.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    #[cfg(target_os = "macos")]
    {
        Some(usage.ru_maxrss as u64)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(not(unix))]
fn peak_memory_bytes() -> Option<u64> {
    None
}
