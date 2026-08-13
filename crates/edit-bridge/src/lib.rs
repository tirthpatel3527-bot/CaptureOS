//! Local, editor-agnostic Edit Bridge interchange and matching mechanics.
//!
//! This crate deliberately has no database, filesystem traversal, image decoder, network,
//! proprietary-editor integration, or copy implementation. It turns a frozen Production
//! snapshot into privacy-safe local interchange files and performs deterministic candidate
//! matching over already discovered returned-output metadata. Filesystem work stays in
//! `capture-core`; durable authority and history stay in `persistence`.

#![forbid(unsafe_code)]

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

pub const EDIT_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const EDIT_ADAPTER_VERSION: &str = "captureos-edit-bridge.v1";
pub const MAX_EXPOSED_MATCH_CANDIDATES: usize = 25;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EditBridgeError {
    #[error("handoff relative path is unsafe")]
    UnsafeRelativePath,
    #[error("handoff manifest serialization failed: {0}")]
    Serialization(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditSessionTemplate {
    WeddingMainEdit,
    AlbumSelectRetouch,
    PortfolioRetouch,
    ClientRevisionRound,
    VideoRoughCut,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditSessionState {
    Draft,
    Ready,
    HandedOff,
    InProgress,
    AwaitingOutputs,
    Review,
    Completed,
    PartiallyCompleted,
    Blocked,
    Stale,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditWorkItemState {
    Queued,
    HandedOff,
    Editing,
    OutputReceived,
    ReadyForReview,
    Approved,
    NeedsRevision,
    Done,
    MissingOutput,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffMode {
    Reference,
    Package,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedOutputPolicy {
    Required,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMatchState {
    Discovered,
    Matched,
    Unmatched,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchConfidence {
    Exact,
    Strong,
    Possible,
    Ambiguous,
    Unmatched,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditVersionReviewState {
    ReadyForReview,
    Approved,
    NeedsRevision,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditAdapterIdentity {
    pub kind: String,
    pub version: String,
    pub local_only: bool,
}

impl EditAdapterIdentity {
    pub fn native_manifest() -> Self {
        Self {
            kind: "captureos_native_manifest".into(),
            version: EDIT_ADAPTER_VERSION.into(),
            local_only: true,
        }
    }
}

/// A future editor adapter is an interchange renderer, never a proprietary-catalog writer.
/// M10 ships only the native manifest/CSV/local-folder representation below.
pub trait EditAdapter {
    fn identity(&self) -> EditAdapterIdentity;
    fn render(&self, manifest: &CanonicalEditManifest)
        -> Result<EditHandoffFiles, EditBridgeError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NativeManifestAdapter;

impl EditAdapter for NativeManifestAdapter {
    fn identity(&self) -> EditAdapterIdentity {
        EditAdapterIdentity::native_manifest()
    }

    fn render(
        &self,
        manifest: &CanonicalEditManifest,
    ) -> Result<EditHandoffFiles, EditBridgeError> {
        native_handoff_files(manifest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditHandoffFiles {
    pub manifest_json: String,
    pub csv: String,
    pub html: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditManifestProject {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditManifestSource {
    pub production_plan_id: String,
    pub export_manifest_id: String,
    pub export_manifest_checksum: String,
    pub export_manifest_version: u64,
    pub handoff_mode: HandoffMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditManifestPrivacy {
    pub private_notes_excluded: bool,
    pub studio_brain_data_excluded: bool,
    pub technical_model_internals_excluded: bool,
    pub database_paths_excluded: bool,
}

impl Default for EditManifestPrivacy {
    fn default() -> Self {
        Self {
            private_notes_excluded: true,
            studio_brain_data_excluded: true,
            technical_model_internals_excluded: true,
            database_paths_excluded: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditManifestWorkItem {
    pub work_item_id: String,
    pub source_media_asset_id: String,
    pub source_manifest_entry_id: String,
    pub handoff_relative_path: String,
    pub original_filename: String,
    pub source_checksum: Option<String>,
    pub captured_at: Option<String>,
    pub camera: Option<String>,
    pub moment_label: Option<String>,
    pub human_decision: Option<String>,
    pub rating: u8,
    pub starred: bool,
    pub expected_output_policy: ExpectedOutputPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalEditManifest {
    pub schema_version: u32,
    pub session_id: String,
    pub handoff_id: String,
    pub created_at: String,
    pub project: EditManifestProject,
    pub source: EditManifestSource,
    pub expected_output_policy: ExpectedOutputPolicy,
    pub privacy: EditManifestPrivacy,
    pub adapter: EditAdapterIdentity,
    pub work_items: Vec<EditManifestWorkItem>,
}

impl CanonicalEditManifest {
    pub fn checksum(&self) -> Result<String, EditBridgeError> {
        let serialized = serde_json::to_vec(self)
            .map_err(|error| EditBridgeError::Serialization(error.to_string()))?;
        let mut hasher = Hasher::new();
        hasher.update(&serialized);
        Ok(hasher.finalize().to_hex().to_string())
    }

    pub fn validate(&self) -> Result<(), EditBridgeError> {
        if self.schema_version != EDIT_MANIFEST_SCHEMA_VERSION {
            return Err(EditBridgeError::Serialization(
                "unsupported Edit Bridge manifest schema".into(),
            ));
        }
        if self.privacy != EditManifestPrivacy::default() {
            return Err(EditBridgeError::Serialization(
                "the M10 native handoff privacy exclusions must remain enabled".into(),
            ));
        }
        if self
            .work_items
            .iter()
            .any(|item| !safe_relative_path(&item.handoff_relative_path))
        {
            return Err(EditBridgeError::UnsafeRelativePath);
        }
        Ok(())
    }
}

/// Renders the only M10 shipping interchange bundle: a canonical native manifest, neutral CSV,
/// and human-readable local HTML. None of the files carries a source absolute path, private note,
/// Studio artifact, embedding, technical score, database path, or proprietary-editor payload.
pub fn native_handoff_files(
    manifest: &CanonicalEditManifest,
) -> Result<EditHandoffFiles, EditBridgeError> {
    manifest.validate()?;
    let manifest_json = serde_json::to_string_pretty(manifest)
        .map_err(|error| EditBridgeError::Serialization(error.to_string()))?;
    let csv = editor_handoff_csv(manifest);
    let html = editor_handoff_html(manifest);
    Ok(EditHandoffFiles {
        manifest_json,
        csv,
        html,
    })
}

pub fn editor_handoff_csv(manifest: &CanonicalEditManifest) -> String {
    let mut csv = String::from(
        "handoff_filename,original_filename,capture_time,camera,rating,star,human_selection,moment_label,work_item_status\n",
    );
    for item in &manifest.work_items {
        let filename = Path::new(&item.handoff_relative_path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(&item.original_filename);
        let row = [
            filename,
            item.original_filename.as_str(),
            item.captured_at.as_deref().unwrap_or(""),
            item.camera.as_deref().unwrap_or(""),
            &item.rating.to_string(),
            if item.starred { "true" } else { "false" },
            item.human_decision.as_deref().unwrap_or(""),
            item.moment_label.as_deref().unwrap_or(""),
            "queued",
        ];
        csv.push_str(
            &row.into_iter()
                .map(csv_escape)
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push('\n');
    }
    csv
}

pub fn editor_handoff_html(manifest: &CanonicalEditManifest) -> String {
    let project = escape_html(&manifest.project.name);
    let session = escape_html(&manifest.session_id);
    let source = escape_html(&manifest.source.export_manifest_checksum);
    format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>CaptureOS Edit Handoff</title><body><main><h1>CaptureOS Edit Handoff</h1><dl><dt>Project</dt><dd>{project}</dd><dt>Session</dt><dd>{session}</dd><dt>Assets</dt><dd>{}</dd><dt>Source manifest checksum</dt><dd>{source}</dd><dt>Status</dt><dd>Ready for editing</dd><dt>Privacy</dt><dd>Private notes and Studio Brain data are not included.</dd></dl><p>This handoff is local workflow metadata. It does not approve edits or modify originals.</p></main></body></html>",
        manifest.work_items.len(),
    )
}

fn csv_escape(value: &str) -> String {
    // Spreadsheet applications may execute an otherwise ordinary external filename or camera
    // string as a formula when it begins with one of their formula sigils. The handoff is local
    // but its metadata is untrusted, so neutralize that interpretation before normal CSV
    // quoting. The visible value remains intact after the leading literal marker.
    let formula_safe = if value.starts_with(['=', '+', '-', '@']) {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    if formula_safe.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", formula_safe.replace('"', "\"\""))
    } else {
        formula_safe
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Validates a relative handoff path before it is rendered or joined to a selected local root.
/// The caller must separately validate that the selected root itself is safe.
pub fn safe_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.trim().is_empty()
        && !path.is_absolute()
        && path.components().all(|component| {
            matches!(component, Component::Normal(_)) || matches!(component, Component::CurDir)
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItemMatchCandidate {
    pub work_item_id: String,
    pub source_media_asset_id: String,
    pub handoff_relative_path: String,
    pub original_filename: String,
    pub source_checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputMatchInput {
    pub relative_path: String,
    pub filename: String,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResolution {
    pub confidence: MatchConfidence,
    /// M10 only auto-associates exact, provenance-preserving evidence. Filename-derived strong
    /// and possible candidates are intentionally retained for human confirmation rather than
    /// silently assigning an external file to a source.
    pub auto_match_work_item_id: Option<String>,
    pub candidate_work_item_ids: Vec<String>,
    pub candidate_count: usize,
    pub evidence: String,
}

#[derive(Debug, Clone, Default)]
pub struct OutputCandidateIndex {
    by_handoff_relative_path: BTreeMap<String, Vec<String>>,
    by_source_checksum: BTreeMap<String, Vec<String>>,
    by_handoff_filename: BTreeMap<String, Vec<String>>,
    by_editable_stem: BTreeMap<String, Vec<String>>,
}

impl OutputCandidateIndex {
    pub fn build(candidates: impl IntoIterator<Item = WorkItemMatchCandidate>) -> Self {
        let mut index = Self::default();
        for candidate in candidates {
            insert_index(
                &mut index.by_handoff_relative_path,
                normalize_relative_path(&candidate.handoff_relative_path),
                candidate.work_item_id.clone(),
            );
            if let Some(checksum) = candidate
                .source_checksum
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                insert_index(
                    &mut index.by_source_checksum,
                    checksum.to_ascii_lowercase(),
                    candidate.work_item_id.clone(),
                );
            }
            insert_index(
                &mut index.by_handoff_filename,
                normalize_filename(&candidate.handoff_relative_path),
                candidate.work_item_id.clone(),
            );
            insert_index(
                &mut index.by_editable_stem,
                editable_stem(&candidate.handoff_relative_path),
                candidate.work_item_id.clone(),
            );
            insert_index(
                &mut index.by_editable_stem,
                editable_stem(&candidate.original_filename),
                candidate.work_item_id,
            );
        }
        index
    }

    pub fn resolve(&self, output: &OutputMatchInput) -> MatchResolution {
        let relative = normalize_relative_path(&output.relative_path);
        if let Some(candidates) = self.by_handoff_relative_path.get(&relative) {
            return resolution(
                candidates,
                MatchConfidence::Exact,
                "returned output relative path matches its frozen handoff path",
            );
        }
        if let Some(checksum) = output.checksum.as_deref().filter(|value| !value.is_empty()) {
            if let Some(candidates) = self.by_source_checksum.get(&checksum.to_ascii_lowercase()) {
                return resolution(
                    candidates,
                    MatchConfidence::Exact,
                    "returned output checksum matches frozen source provenance",
                );
            }
        }
        let filename = normalize_filename(&output.filename);
        if let Some(candidates) = self.by_handoff_filename.get(&filename) {
            return resolution(
                candidates,
                MatchConfidence::Strong,
                "returned output filename matches one frozen handoff filename",
            );
        }
        let stem = editable_stem(&output.filename);
        if let Some(candidates) = self.by_editable_stem.get(&stem) {
            return resolution(
                candidates,
                MatchConfidence::Possible,
                "returned output filename stem resembles frozen handoff/source filenames",
            );
        }
        MatchResolution {
            confidence: MatchConfidence::Unmatched,
            auto_match_work_item_id: None,
            candidate_work_item_ids: Vec::new(),
            candidate_count: 0,
            evidence: "no safe provenance, path, checksum, or filename candidate was found".into(),
        }
    }
}

fn insert_index(index: &mut BTreeMap<String, Vec<String>>, key: String, candidate: String) {
    if key.is_empty() {
        return;
    }
    let values = index.entry(key).or_default();
    if !values.iter().any(|existing| existing == &candidate) {
        values.push(candidate);
        values.sort();
    }
}

fn resolution(
    candidates: &[String],
    requested: MatchConfidence,
    evidence: &str,
) -> MatchResolution {
    let candidate_count = candidates.len();
    let candidate_work_item_ids = candidates
        .iter()
        .take(MAX_EXPOSED_MATCH_CANDIDATES)
        .cloned()
        .collect();
    if candidate_count != 1 {
        return MatchResolution {
            confidence: MatchConfidence::Ambiguous,
            auto_match_work_item_id: None,
            candidate_work_item_ids,
            candidate_count,
            evidence: format!(
                "{evidence}; {candidate_count} frozen work items are plausible, so CaptureOS requires a human match"
            ),
        };
    }
    let only = candidates[0].clone();
    MatchResolution {
        confidence: requested,
        auto_match_work_item_id: matches!(requested, MatchConfidence::Exact).then_some(only),
        candidate_work_item_ids,
        candidate_count,
        evidence: evidence.into(),
    }
}

fn normalize_relative_path(value: &str) -> String {
    value
        .replace('\\', "/")
        .nfc()
        .collect::<String>()
        .to_lowercase()
}

fn normalize_filename(value: &str) -> String {
    let filename = Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value);
    filename.nfc().collect::<String>().to_lowercase()
}

fn editable_stem(value: &str) -> String {
    let filename = normalize_filename(value);
    let stem = filename
        .rsplit_once('.')
        .map_or(filename.as_str(), |(stem, _)| stem);
    let compact = stem
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    ["edited", "edit", "retouch", "final"]
        .iter()
        .find_map(|suffix| compact.strip_suffix(suffix))
        .unwrap_or(&compact)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> CanonicalEditManifest {
        CanonicalEditManifest {
            schema_version: EDIT_MANIFEST_SCHEMA_VERSION,
            session_id: "session-1".into(),
            handoff_id: "handoff-1".into(),
            created_at: "2026-08-12T00:00:00Z".into(),
            project: EditManifestProject {
                id: "project-1".into(),
                name: "A < B".into(),
            },
            source: EditManifestSource {
                production_plan_id: "plan-1".into(),
                export_manifest_id: "manifest-1".into(),
                export_manifest_checksum: "abc".into(),
                export_manifest_version: 1,
                handoff_mode: HandoffMode::Reference,
            },
            expected_output_policy: ExpectedOutputPolicy::Required,
            privacy: EditManifestPrivacy::default(),
            adapter: EditAdapterIdentity::native_manifest(),
            work_items: vec![EditManifestWorkItem {
                work_item_id: "item-1".into(),
                source_media_asset_id: "asset-1".into(),
                source_manifest_entry_id: "entry-1".into(),
                handoff_relative_path: "01_Ceremony/DSC0001.JPG".into(),
                original_filename: "DSC0001.JPG".into(),
                source_checksum: Some("source-hash".into()),
                captured_at: Some("2026-08-12T10:00:00Z".into()),
                camera: Some("Camera A".into()),
                moment_label: Some("Ceremony".into()),
                human_decision: Some("keep".into()),
                rating: 5,
                starred: true,
                expected_output_policy: ExpectedOutputPolicy::Required,
            }],
        }
    }

    #[test]
    fn native_files_are_deterministic_and_private() {
        let first = native_handoff_files(&manifest()).unwrap();
        let second = native_handoff_files(&manifest()).unwrap();
        assert_eq!(first.manifest_json, second.manifest_json);
        assert_eq!(first.csv, second.csv);
        assert!(!first.manifest_json.contains("Client requested this one"));
        assert!(!first.csv.contains("Client requested this one"));
        assert!(first.html.contains("A &lt; B"));
    }

    #[test]
    fn unsafe_handoff_path_is_rejected() {
        let mut value = manifest();
        value.work_items[0].handoff_relative_path = "../outside.JPG".into();
        assert_eq!(
            native_handoff_files(&value),
            Err(EditBridgeError::UnsafeRelativePath)
        );
    }

    #[test]
    fn duplicate_basenames_are_never_silently_assigned() {
        let index = OutputCandidateIndex::build([
            WorkItemMatchCandidate {
                work_item_id: "camera-a".into(),
                source_media_asset_id: "a".into(),
                handoff_relative_path: "A/DSC0001.JPG".into(),
                original_filename: "DSC0001.JPG".into(),
                source_checksum: None,
            },
            WorkItemMatchCandidate {
                work_item_id: "camera-b".into(),
                source_media_asset_id: "b".into(),
                handoff_relative_path: "B/DSC0001.JPG".into(),
                original_filename: "DSC0001.JPG".into(),
                source_checksum: None,
            },
        ]);
        let result = index.resolve(&OutputMatchInput {
            relative_path: "returned/DSC0001_EDIT.JPG".into(),
            filename: "DSC0001_EDIT.JPG".into(),
            checksum: None,
        });
        assert_eq!(result.confidence, MatchConfidence::Ambiguous);
        assert_eq!(result.auto_match_work_item_id, None);
        assert_eq!(result.candidate_count, 2);
    }

    #[test]
    fn exact_path_auto_resolves_but_unique_filename_remains_a_human_suggestion() {
        let index = OutputCandidateIndex::build([WorkItemMatchCandidate {
            work_item_id: "item".into(),
            source_media_asset_id: "asset".into(),
            handoff_relative_path: "Work/IMG_0001.JPG".into(),
            original_filename: "IMG_0001.JPG".into(),
            source_checksum: Some("abc".into()),
        }]);
        let exact = index.resolve(&OutputMatchInput {
            relative_path: "work/img_0001.jpg".into(),
            filename: "IMG_0001.JPG".into(),
            checksum: None,
        });
        assert_eq!(exact.confidence, MatchConfidence::Exact);
        assert_eq!(exact.auto_match_work_item_id.as_deref(), Some("item"));
        let strong = index.resolve(&OutputMatchInput {
            relative_path: "elsewhere/IMG_0001.JPG".into(),
            filename: "IMG_0001.JPG".into(),
            checksum: None,
        });
        assert_eq!(strong.confidence, MatchConfidence::Strong);
        assert_eq!(strong.auto_match_work_item_id, None);
        assert_eq!(strong.candidate_work_item_ids, vec!["item"]);
    }

    #[test]
    fn possible_stem_needs_human_confirmation() {
        let index = OutputCandidateIndex::build([WorkItemMatchCandidate {
            work_item_id: "item".into(),
            source_media_asset_id: "asset".into(),
            handoff_relative_path: "IMG_0001.JPG".into(),
            original_filename: "IMG_0001.JPG".into(),
            source_checksum: None,
        }]);
        let result = index.resolve(&OutputMatchInput {
            relative_path: "output/IMG_0001_EDIT.JPG".into(),
            filename: "IMG_0001_EDIT.JPG".into(),
            checksum: None,
        });
        assert_eq!(result.confidence, MatchConfidence::Possible);
        assert_eq!(result.auto_match_work_item_id, None);
    }

    #[test]
    fn safe_relative_path_rejects_absolute_and_parent_paths() {
        assert!(safe_relative_path("folder/file.jpg"));
        assert!(!safe_relative_path("../file.jpg"));
        assert!(!safe_relative_path("/file.jpg"));
    }
}
