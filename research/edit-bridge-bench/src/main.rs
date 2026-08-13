//! Deterministic generated-data benchmark for M10 Edit Bridge interchange and matching.
//!
//! The fixture contains only generated strings and metadata. It never opens a CaptureOS
//! catalog, customer media, preview, model, filesystem output tree, or network service.

#![forbid(unsafe_code)]

use edit_bridge::{
    native_handoff_files, CanonicalEditManifest, EditAdapterIdentity, EditManifestPrivacy,
    EditManifestProject, EditManifestSource, EditManifestWorkItem, ExpectedOutputPolicy,
    HandoffMode, OutputCandidateIndex, OutputMatchInput, WorkItemMatchCandidate,
    EDIT_MANIFEST_SCHEMA_VERSION,
};
use serde::Serialize;
use std::{env, time::Instant};

const SCALES: [usize; 5] = [100, 1_000, 10_000, 50_000, 100_000];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    suite: String,
    benchmark_kind: &'static str,
    fixture: Fixture,
    scales: Vec<Scale>,
    duplicate_basename: DuplicateBasename,
    idempotency: Idempotency,
    caveats: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    version: &'static str,
    source: &'static str,
    contains_customer_media: bool,
    contains_network_calls: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Scale {
    work_item_count: usize,
    session_creation_elapsed_us: u128,
    handoff_manifest_elapsed_us: u128,
    work_item_first_page_elapsed_us: u128,
    output_candidate_index_elapsed_us: u128,
    matching_elapsed_us: u128,
    version_registration_lookup_elapsed_us: u128,
    status_aggregation_elapsed_us: u128,
    manifest_checksum_stable: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateBasename {
    confidence: String,
    candidate_count: usize,
    silently_assigned: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Idempotency {
    repeated_generated_output_count: usize,
    unique_identity_count: usize,
    duplicate_prevented: bool,
}

fn main() {
    let suite = parse_suite();
    let scales = SCALES.into_iter().map(measure_scale).collect();
    let duplicate_index = OutputCandidateIndex::build([
        candidate("camera-a", "A/DSC0001.JPG", None),
        candidate("camera-b", "B/DSC0001.JPG", None),
    ]);
    let duplicate = duplicate_index.resolve(&OutputMatchInput {
        relative_path: "returned/DSC0001_EDIT.JPG".into(),
        filename: "DSC0001_EDIT.JPG".into(),
        checksum: None,
    });
    let repeated = (0..1_000)
        .map(|index| format!("output/{index:04}.jpg:checksum-{index:04}"))
        .chain((0..1_000).map(|index| format!("output/{index:04}.jpg:checksum-{index:04}")))
        .collect::<Vec<_>>();
    let unique = repeated.iter().collect::<std::collections::BTreeSet<_>>();
    let report = Report {
        suite,
        benchmark_kind: "deterministic generated Edit Bridge handoff and output-candidate matching",
        fixture: Fixture {
            version: "captureos-edit-bridge.synthetic.v1",
            source: "generated immutable work-item metadata and output identifiers only",
            contains_customer_media: false,
            contains_network_calls: false,
        },
        scales,
        duplicate_basename: DuplicateBasename {
            confidence: format!("{:?}", duplicate.confidence).to_lowercase(),
            candidate_count: duplicate.candidate_count,
            silently_assigned: duplicate.auto_match_work_item_id.is_some(),
        },
        idempotency: Idempotency {
            repeated_generated_output_count: repeated.len(),
            unique_identity_count: unique.len(),
            duplicate_prevented: unique.len() == 1_000,
        },
        caveats: vec![
            "The benchmark measures generated metadata and deterministic matching, not customer catalogs, media binaries, drive latency, or editor behavior.",
            "Registration identity is modeled by generated relative path plus checksum; production registration performs streaming local hashing outside the database transaction.",
            "No filesystem output scan, CaptureOS database, preview cache, model, embedding, note, absolute customer path, or network service is opened.",
        ],
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("report serializes")
    );
}

fn parse_suite() -> String {
    let mut arguments = env::args().skip(1);
    match (arguments.next().as_deref(), arguments.next()) {
        (Some("--suite"), Some(value)) if value == "baseline" => value,
        _ => panic!("usage: edit-bridge-bench --suite baseline"),
    }
}

fn measure_scale(count: usize) -> Scale {
    let session_start = Instant::now();
    let work_items = (0..count).map(work_item).collect::<Vec<_>>();
    let session_creation_elapsed_us = session_start.elapsed().as_micros();
    let manifest = manifest(work_items);
    let handoff_start = Instant::now();
    let first = native_handoff_files(&manifest).expect("generated manifest is valid");
    let handoff_manifest_elapsed_us = handoff_start.elapsed().as_micros();
    let second = native_handoff_files(&manifest).expect("generated manifest is valid repeatedly");
    let page_start = Instant::now();
    let first_page = manifest.work_items.iter().take(250).count();
    assert_eq!(first_page, count.min(250));
    let work_item_first_page_elapsed_us = page_start.elapsed().as_micros();
    let candidates = manifest
        .work_items
        .iter()
        .map(|item| {
            candidate(
                &item.work_item_id,
                &item.handoff_relative_path,
                item.source_checksum.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    let index_start = Instant::now();
    let index = OutputCandidateIndex::build(candidates);
    let output_candidate_index_elapsed_us = index_start.elapsed().as_micros();
    let match_start = Instant::now();
    for item in manifest.work_items.iter().step_by((count / 100).max(1)) {
        let result = index.resolve(&OutputMatchInput {
            relative_path: item.handoff_relative_path.clone(),
            filename: item.original_filename.clone(),
            checksum: item.source_checksum.clone(),
        });
        assert!(result.auto_match_work_item_id.is_some());
    }
    let matching_elapsed_us = match_start.elapsed().as_micros();
    let versions = manifest
        .work_items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.work_item_id.as_str(), index))
        .collect::<std::collections::BTreeMap<_, _>>();
    let version_start = Instant::now();
    assert_eq!(versions.get("work-000000"), Some(&0));
    let version_registration_lookup_elapsed_us = version_start.elapsed().as_micros();
    let aggregate_start = Instant::now();
    let approved = versions.values().filter(|index| **index % 3 == 0).count();
    let needs_revision = versions.values().filter(|index| **index % 3 == 1).count();
    assert_eq!(
        approved + needs_revision + versions.len() / 3,
        versions.len()
    );
    let status_aggregation_elapsed_us = aggregate_start.elapsed().as_micros();
    Scale {
        work_item_count: count,
        session_creation_elapsed_us,
        handoff_manifest_elapsed_us,
        work_item_first_page_elapsed_us,
        output_candidate_index_elapsed_us,
        matching_elapsed_us,
        version_registration_lookup_elapsed_us,
        status_aggregation_elapsed_us,
        manifest_checksum_stable: first.manifest_json == second.manifest_json,
    }
}

fn work_item(index: usize) -> EditManifestWorkItem {
    EditManifestWorkItem {
        work_item_id: format!("work-{index:06}"),
        source_media_asset_id: format!("asset-{index:06}"),
        source_manifest_entry_id: format!("entry-{index:06}"),
        handoff_relative_path: format!("Moment_{:03}/IMG_{index:06}.JPG", index % 200),
        original_filename: format!("IMG_{index:06}.JPG"),
        source_checksum: Some(format!("checksum-{index:06}")),
        captured_at: Some("2026-08-12T10:00:00Z".into()),
        camera: Some("Generated Camera".into()),
        moment_label: Some(format!("Moment {}", index % 200)),
        human_decision: Some("keep".into()),
        rating: (index % 6) as u8,
        starred: index.is_multiple_of(7),
        expected_output_policy: ExpectedOutputPolicy::Required,
    }
}

fn candidate(id: &str, path: &str, checksum: Option<&str>) -> WorkItemMatchCandidate {
    WorkItemMatchCandidate {
        work_item_id: id.into(),
        source_media_asset_id: format!("asset-{id}"),
        handoff_relative_path: path.into(),
        original_filename: path.rsplit('/').next().unwrap_or(path).into(),
        source_checksum: checksum.map(str::to_owned),
    }
}

fn manifest(work_items: Vec<EditManifestWorkItem>) -> CanonicalEditManifest {
    CanonicalEditManifest {
        schema_version: EDIT_MANIFEST_SCHEMA_VERSION,
        session_id: "generated-session".into(),
        handoff_id: "generated-handoff".into(),
        created_at: "2026-08-12T00:00:00Z".into(),
        project: EditManifestProject {
            id: "generated-project".into(),
            name: "Generated Project".into(),
        },
        source: EditManifestSource {
            production_plan_id: "generated-plan".into(),
            export_manifest_id: "generated-export-manifest".into(),
            export_manifest_checksum: "generated-checksum".into(),
            export_manifest_version: 1,
            handoff_mode: HandoffMode::Reference,
        },
        expected_output_policy: ExpectedOutputPolicy::Required,
        privacy: EditManifestPrivacy::default(),
        adapter: EditAdapterIdentity::native_manifest(),
        work_items,
    }
}
