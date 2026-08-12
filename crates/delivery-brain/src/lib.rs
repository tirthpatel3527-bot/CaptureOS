//! Deterministic, local-only Production Plan and manifest mechanics for CaptureOS.
//!
//! This crate deliberately has no filesystem traversal, database, network, image decoder, or
//! cloud adapter. It turns bounded catalog evidence into a dry-run manifest; a local adapter
//! later resolves a selected `FileInstance` and reuses CaptureOS's verified-copy primitive.

#![forbid(unsafe_code)]

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const DELIVERY_MANIFEST_FORMAT_VERSION: u32 = 1;
pub const MIN_DESTINATION_RESERVE_BYTES: u64 = 128 * 1024 * 1024;
pub const DEFAULT_DESTINATION_RESERVE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductionPlanType {
    ClientDelivery,
    EditorWorkset,
    PortfolioSelects,
    ProofGallery,
    BackupArchive,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductionPlanStatus {
    Draft,
    Ready,
    Exporting,
    Completed,
    PartiallyCompleted,
    Blocked,
    Failed,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualCollectionKind {
    Static,
    Dynamic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganizationStrategy {
    SingleFolder,
    ByMoment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", content = "template", rename_all = "snake_case")]
pub enum FilenameStrategy {
    #[default]
    PreserveOriginal,
    Sequential,
    ProjectSequence,
    MomentSequence,
    CustomTemplate(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanOverrideKind {
    ForceInclude,
    ForceExclude,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SelectionRules {
    /// Explicit human culling decisions only. Studio recommendations are never a selection rule.
    pub decisions: Vec<String>,
    pub minimum_rating: Option<u8>,
    pub starred_only: bool,
    pub moment_ids: Vec<String>,
    /// A static collection may populate this bounded explicit list. It remains separate from
    /// the underlying human culling decision.
    pub static_asset_ids: Vec<String>,
    /// A project-local Virtual Collection reference. Persistence resolves it to asset
    /// references before invoking this pure planner; it is never a Studio signal.
    pub virtual_collection_id: Option<String>,
}

impl SelectionRules {
    pub fn client_delivery() -> Self {
        Self {
            decisions: vec!["keep".into()],
            ..Self::default()
        }
    }

    pub fn editor_workset() -> Self {
        Self {
            decisions: vec!["keep".into(), "review".into()],
            ..Self::default()
        }
    }

    pub fn matches(&self, asset: &DeliveryAssetCandidate) -> bool {
        let has_static_selection = !self.static_asset_ids.is_empty();
        if has_static_selection && !self.static_asset_ids.iter().any(|id| id == &asset.asset_id) {
            return false;
        }
        if !self.decisions.is_empty()
            && !asset
                .human_decision
                .as_deref()
                .is_some_and(|decision| self.decisions.iter().any(|value| value == decision))
        {
            return false;
        }
        if self
            .minimum_rating
            .is_some_and(|rating| asset.rating < rating)
        {
            return false;
        }
        if self.starred_only && !asset.starred {
            return false;
        }
        self.moment_ids.is_empty()
            || asset
                .moment
                .as_ref()
                .is_some_and(|moment| self.moment_ids.iter().any(|id| id == &moment.id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MomentDestination {
    pub id: String,
    pub ordinal: u64,
    pub human_label: Option<String>,
    pub suggested_label: Option<String>,
}

impl MomentDestination {
    pub fn folder_name(&self) -> String {
        let label = self
            .human_label
            .as_deref()
            .or(self.suggested_label.as_deref())
            .map(sanitize_component)
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| format!("Moment_{:03}", self.ordinal + 1));
        format!("{:02}_{label}", self.ordinal + 1)
    }

    pub fn naming_label(&self) -> String {
        self.human_label
            .as_deref()
            .or(self.suggested_label.as_deref())
            .map(sanitize_component)
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| format!("Moment_{:03}", self.ordinal + 1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverySourceCandidate {
    pub file_instance_id: String,
    /// A local adapter resolves this only at execution time. It is never part of a client report.
    pub source_root_id: Option<String>,
    pub relative_path: String,
    pub available: bool,
    pub verified: bool,
    pub observed_at: String,
    pub byte_size: u64,
    pub content_checksum: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryAssetCandidate {
    pub asset_id: String,
    pub original_filename: String,
    pub extension: Option<String>,
    pub byte_size: u64,
    pub captured_at: Option<String>,
    pub camera: Option<String>,
    pub human_decision: Option<String>,
    pub rating: u8,
    pub starred: bool,
    pub moment: Option<MomentDestination>,
    pub sources: Vec<DeliverySourceCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOverride {
    pub media_asset_id: String,
    pub kind: PlanOverrideKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestBuildInput {
    pub plan_id: String,
    pub project_id: String,
    pub project_name: String,
    pub plan_type: ProductionPlanType,
    pub selection_rules: SelectionRules,
    pub organization: OrganizationStrategy,
    pub filename_strategy: FilenameStrategy,
    pub overrides: Vec<PlanOverride>,
    /// `None` means no collection constraint; `Some(empty)` represents an intentionally empty
    /// collection. This is resolved locally by persistence, not by the pure planner.
    pub virtual_collection_asset_ids: Option<Vec<String>>,
    pub assets: Vec<DeliveryAssetCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestEntryStatus {
    Planned,
    BlockedSourceUnavailable,
    BlockedInternalCollision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntryDraft {
    pub media_asset_id: String,
    pub selected_file_instance_id: Option<String>,
    pub source_relative_path: Option<String>,
    pub original_filename: String,
    pub destination_relative_path: String,
    pub destination_filename: String,
    pub expected_byte_size: u64,
    pub source_checksum: Option<String>,
    pub human_decision: Option<String>,
    pub rating: u8,
    pub starred: bool,
    pub moment_id: Option<String>,
    pub moment_label: Option<String>,
    pub status: ManifestEntryStatus,
    pub issue: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestIssue {
    pub code: String,
    pub message: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestDraft {
    pub format_version: u32,
    pub plan_id: String,
    pub project_id: String,
    pub selected_file_count: u64,
    pub estimated_bytes: u64,
    pub entries: Vec<ManifestEntryDraft>,
    pub issues: Vec<ManifestIssue>,
    pub checksum: String,
}

impl ManifestDraft {
    pub fn is_ready(&self) -> bool {
        !self.issues.iter().any(|issue| issue.blocking)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DeliveryBrainError {
    #[error("custom filename template contains an unsupported token: {0}")]
    UnsupportedTemplateToken(String),
    #[error("custom filename template is empty")]
    EmptyTemplate,
    #[error("manifest byte count overflow")]
    ByteCountOverflow,
    #[error("manifest serialization failed: {0}")]
    Serialization(String),
}

/// Selects the safest local source by availability, independently verified-copy evidence,
/// checksum availability, newest observation, and stable FileInstance identity. It never falls
/// back to an unavailable source merely to make a manifest look complete.
pub fn select_source(asset: &DeliveryAssetCandidate) -> Option<DeliverySourceCandidate> {
    let mut available = asset
        .sources
        .iter()
        .filter(|source| source.available)
        .cloned()
        .collect::<Vec<_>>();
    available.sort_by(|left, right| {
        right
            .verified
            .cmp(&left.verified)
            .then_with(|| {
                right
                    .content_checksum
                    .is_some()
                    .cmp(&left.content_checksum.is_some())
            })
            .then_with(|| right.observed_at.cmp(&left.observed_at))
            .then_with(|| left.file_instance_id.cmp(&right.file_instance_id))
    });
    available.into_iter().next()
}

/// Creates a deterministic, immutable candidate manifest. The caller must still perform a
/// destination preflight and persist the completed draft in one transaction before export.
pub fn build_manifest(input: &ManifestBuildInput) -> Result<ManifestDraft, DeliveryBrainError> {
    validate_filename_strategy(&input.filename_strategy)?;
    let overrides = input
        .overrides
        .iter()
        .map(|override_value| (override_value.media_asset_id.as_str(), override_value.kind))
        .collect::<BTreeMap<_, _>>();
    let collection_asset_ids = input
        .virtual_collection_asset_ids
        .as_ref()
        .map(|ids| ids.iter().map(String::as_str).collect::<BTreeSet<_>>());
    let mut assets = input
        .assets
        .iter()
        .filter(|asset| match overrides.get(asset.asset_id.as_str()) {
            Some(PlanOverrideKind::ForceExclude) => false,
            Some(PlanOverrideKind::ForceInclude) => true,
            None => {
                collection_asset_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(asset.asset_id.as_str()))
                    && input.selection_rules.matches(asset)
            }
        })
        .collect::<Vec<_>>();
    assets.sort_by_key(|asset| stable_asset_sort_key(asset));

    let mut entries = Vec::with_capacity(assets.len());
    let mut issues = Vec::new();
    let mut estimated_bytes = 0_u64;
    let mut used_destinations = BTreeMap::<String, Vec<usize>>::new();
    for (index, asset) in assets.into_iter().enumerate() {
        estimated_bytes = estimated_bytes
            .checked_add(asset.byte_size)
            .ok_or(DeliveryBrainError::ByteCountOverflow)?;
        let filename = generated_filename(
            &input.filename_strategy,
            &input.project_name,
            asset,
            index as u64 + 1,
        )?;
        let folder = match input.organization {
            OrganizationStrategy::SingleFolder => None,
            OrganizationStrategy::ByMoment => {
                asset.moment.as_ref().map(MomentDestination::folder_name)
            }
        };
        let destination_relative_path = match folder {
            Some(folder) => format!("{folder}/{filename}"),
            None => filename.clone(),
        };
        let destination_key = destination_relative_path.to_lowercase();
        used_destinations
            .entry(destination_key)
            .or_default()
            .push(entries.len());
        let source = select_source(asset);
        let (status, issue) = if source.is_some() {
            (ManifestEntryStatus::Planned, None)
        } else {
            (
                ManifestEntryStatus::BlockedSourceUnavailable,
                Some("No available local FileInstance is suitable for this export entry.".into()),
            )
        };
        if issue.is_some() {
            issues.push(ManifestIssue {
                code: "source_unavailable".into(),
                message: format!(
                    "{} is selected but no source copy is available.",
                    asset.original_filename
                ),
                blocking: true,
            });
        }
        entries.push(ManifestEntryDraft {
            media_asset_id: asset.asset_id.clone(),
            selected_file_instance_id: source.as_ref().map(|value| value.file_instance_id.clone()),
            source_relative_path: source.as_ref().map(|value| value.relative_path.clone()),
            original_filename: asset.original_filename.clone(),
            destination_relative_path,
            destination_filename: filename,
            expected_byte_size: asset.byte_size,
            source_checksum: source.and_then(|value| value.content_checksum),
            human_decision: asset.human_decision.clone(),
            rating: asset.rating,
            starred: asset.starred,
            moment_id: asset.moment.as_ref().map(|moment| moment.id.clone()),
            moment_label: asset.moment.as_ref().map(MomentDestination::naming_label),
            status,
            issue,
        });
    }
    for indices in used_destinations
        .values()
        .filter(|indices| indices.len() > 1)
    {
        for index in indices {
            let entry = &mut entries[*index];
            entry.status = ManifestEntryStatus::BlockedInternalCollision;
            entry.issue =
                Some("Another planned entry resolves to this same destination path.".into());
        }
        issues.push(ManifestIssue {
            code: "internal_destination_collision".into(),
            message: format!(
                "{} planned entries resolve to the same destination path.",
                indices.len()
            ),
            blocking: true,
        });
    }
    let checksum = manifest_checksum(&input.plan_id, &input.project_id, &entries, &issues)?;
    Ok(ManifestDraft {
        format_version: DELIVERY_MANIFEST_FORMAT_VERSION,
        plan_id: input.plan_id.clone(),
        project_id: input.project_id.clone(),
        selected_file_count: entries.len() as u64,
        estimated_bytes,
        entries,
        issues,
        checksum,
    })
}

pub fn sanitize_component(value: &str) -> String {
    let mut sanitized = value
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .trim()
        .to_owned();
    while sanitized.contains("__") {
        sanitized = sanitized.replace("__", "_");
    }
    if sanitized.is_empty() {
        return "Untitled".into();
    }
    let upper = sanitized.to_ascii_uppercase();
    let base = upper.split('.').next().unwrap_or_default();
    if is_reserved_component(base) {
        sanitized = format!("_{sanitized}");
    }
    // Keep names portable while remaining recognizable. A suffix keeps close names deterministic.
    const MAX_COMPONENT_CHARS: usize = 120;
    if sanitized.chars().count() > MAX_COMPONENT_CHARS {
        sanitized = sanitized.chars().take(MAX_COMPONENT_CHARS).collect();
    }
    sanitized
}

pub fn safe_destination_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && path.split('/').all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && component == sanitize_component(component)
        })
}

fn stable_asset_sort_key(asset: &DeliveryAssetCandidate) -> (u8, String, u64, String) {
    match &asset.captured_at {
        Some(captured_at) => (
            0,
            captured_at.clone(),
            asset
                .moment
                .as_ref()
                .map_or(u64::MAX, |moment| moment.ordinal),
            asset.asset_id.clone(),
        ),
        None => (
            1,
            String::new(),
            asset
                .moment
                .as_ref()
                .map_or(u64::MAX, |moment| moment.ordinal),
            asset.asset_id.clone(),
        ),
    }
}

fn generated_filename(
    strategy: &FilenameStrategy,
    project_name: &str,
    asset: &DeliveryAssetCandidate,
    sequence: u64,
) -> Result<String, DeliveryBrainError> {
    let (stem, extension) = split_filename(&asset.original_filename, asset.extension.as_deref());
    let project = sanitize_component(project_name);
    let moment = asset
        .moment
        .as_ref()
        .map(MomentDestination::naming_label)
        .unwrap_or_else(|| "Moment_000".into());
    let capture_date = asset
        .captured_at
        .as_deref()
        .and_then(|value| value.get(0..10))
        .map(|value| value.replace('-', ""))
        .unwrap_or_else(|| "UnknownDate".into());
    let camera = asset
        .camera
        .as_deref()
        .map(sanitize_component)
        .unwrap_or_else(|| "UnknownCamera".into());
    let generated_stem = match strategy {
        FilenameStrategy::PreserveOriginal => stem,
        FilenameStrategy::Sequential => format!("{sequence:04}"),
        FilenameStrategy::ProjectSequence => format!("{project}_{sequence:04}"),
        FilenameStrategy::MomentSequence => format!("{moment}_{sequence:04}"),
        FilenameStrategy::CustomTemplate(template) => render_template(
            template,
            &[
                ("project", project),
                ("moment", moment),
                ("sequence", format!("{sequence:04}")),
                ("original", stem),
                ("capture_date", capture_date),
                ("camera", camera),
            ],
        )?,
    };
    let stem = sanitize_component(&generated_stem);
    Ok(match extension {
        Some(extension) if !extension.is_empty() => format!("{stem}.{extension}"),
        _ => stem,
    })
}

fn split_filename(filename: &str, fallback_extension: Option<&str>) -> (String, Option<String>) {
    let filename = filename.trim();
    let mut parts = filename.rsplitn(2, '.');
    let extension = parts.next().filter(|part| !part.is_empty());
    let stem = parts.next();
    match (stem, extension) {
        (Some(stem), Some(extension)) => (
            sanitize_component(stem),
            Some(sanitize_component(extension)),
        ),
        _ => (
            sanitize_component(filename),
            fallback_extension
                .map(sanitize_component)
                .filter(|value| !value.is_empty()),
        ),
    }
}

fn render_template(
    template: &str,
    values: &[(&str, String)],
) -> Result<String, DeliveryBrainError> {
    if template.trim().is_empty() {
        return Err(DeliveryBrainError::EmptyTemplate);
    }
    let mut rendered = template.to_owned();
    let allowed = values
        .iter()
        .map(|(token, _)| *token)
        .collect::<BTreeSet<_>>();
    let mut remaining = rendered.as_str();
    while let Some(start) = remaining.find('{') {
        let after_start = &remaining[start + 1..];
        let end = after_start
            .find('}')
            .ok_or_else(|| DeliveryBrainError::UnsupportedTemplateToken(after_start.into()))?;
        let token = &after_start[..end];
        if !allowed.contains(token) {
            return Err(DeliveryBrainError::UnsupportedTemplateToken(token.into()));
        }
        remaining = &after_start[end + 1..];
    }
    if remaining.contains('}') {
        return Err(DeliveryBrainError::UnsupportedTemplateToken(
            "unmatched closing brace".into(),
        ));
    }
    for (token, value) in values {
        rendered = rendered.replace(&format!("{{{token}}}"), value);
    }
    Ok(rendered)
}

fn validate_filename_strategy(strategy: &FilenameStrategy) -> Result<(), DeliveryBrainError> {
    if let FilenameStrategy::CustomTemplate(template) = strategy {
        render_template(
            template,
            &[
                ("project", String::new()),
                ("moment", String::new()),
                ("sequence", String::new()),
                ("original", String::new()),
                ("capture_date", String::new()),
                ("camera", String::new()),
            ],
        )?;
    }
    Ok(())
}

fn manifest_checksum(
    plan_id: &str,
    project_id: &str,
    entries: &[ManifestEntryDraft],
    issues: &[ManifestIssue],
) -> Result<String, DeliveryBrainError> {
    let payload = serde_json::to_vec(&(
        DELIVERY_MANIFEST_FORMAT_VERSION,
        plan_id,
        project_id,
        entries,
        issues,
    ))
    .map_err(|error| DeliveryBrainError::Serialization(error.to_string()))?;
    let mut hasher = Hasher::new();
    hasher.update(&payload);
    Ok(hasher.finalize().to_hex().to_string())
}

fn is_reserved_component(value: &str) -> bool {
    matches!(value, "CON" | "PRN" | "AUX" | "NUL")
        || value
            .strip_prefix("COM")
            .or_else(|| value.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, available: bool, verified: bool) -> DeliverySourceCandidate {
        DeliverySourceCandidate {
            file_instance_id: id.into(),
            source_root_id: Some("root".into()),
            relative_path: format!("camera/{id}.ARW"),
            available,
            verified,
            observed_at: "2026-08-12T10:00:00Z".into(),
            byte_size: 4,
            content_checksum: verified.then(|| "abc".into()),
        }
    }

    fn asset(id: &str, filename: &str, decision: Option<&str>) -> DeliveryAssetCandidate {
        DeliveryAssetCandidate {
            asset_id: id.into(),
            original_filename: filename.into(),
            extension: Some("ARW".into()),
            byte_size: 4,
            captured_at: Some(format!("2026-08-12T10:00:0{id}Z")),
            camera: None,
            human_decision: decision.map(str::to_owned),
            rating: 5,
            starred: true,
            moment: None,
            sources: vec![
                source(&format!("offline-{id}"), false, true),
                source(&format!("live-{id}"), true, true),
            ],
        }
    }

    fn input() -> ManifestBuildInput {
        ManifestBuildInput {
            plan_id: "plan".into(),
            project_id: "project".into(),
            project_name: "A / Test".into(),
            plan_type: ProductionPlanType::ClientDelivery,
            selection_rules: SelectionRules::client_delivery(),
            organization: OrganizationStrategy::SingleFolder,
            filename_strategy: FilenameStrategy::ProjectSequence,
            overrides: vec![],
            virtual_collection_asset_ids: None,
            assets: vec![
                asset("1", "DSC0001.ARW", Some("keep")),
                asset("2", "DSC0002.ARW", Some("review")),
            ],
        }
    }

    #[test]
    fn source_selection_prefers_available_verified_copy() {
        let candidate = asset("1", "DSC0001.ARW", Some("keep"));
        assert_eq!(
            select_source(&candidate).unwrap().file_instance_id,
            "live-1"
        );
    }

    #[test]
    fn client_delivery_never_uses_studio_advice_as_selection() {
        let draft = build_manifest(&input()).unwrap();
        assert_eq!(draft.entries.len(), 1);
        assert_eq!(draft.entries[0].media_asset_id, "1");
    }

    #[test]
    fn resolved_virtual_collection_intersects_human_rules_but_force_include_remains_explicit() {
        let mut request = input();
        request.virtual_collection_asset_ids = Some(vec!["2".into()]);
        assert!(build_manifest(&request).unwrap().entries.is_empty());
        request.overrides = vec![PlanOverride {
            media_asset_id: "1".into(),
            kind: PlanOverrideKind::ForceInclude,
        }];
        let draft = build_manifest(&request).unwrap();
        assert_eq!(draft.entries.len(), 1);
        assert_eq!(draft.entries[0].media_asset_id, "1");
    }

    #[test]
    fn force_include_and_exclude_are_plan_scoped() {
        let mut request = input();
        request.overrides = vec![
            PlanOverride {
                media_asset_id: "2".into(),
                kind: PlanOverrideKind::ForceInclude,
            },
            PlanOverride {
                media_asset_id: "1".into(),
                kind: PlanOverrideKind::ForceExclude,
            },
        ];
        let draft = build_manifest(&request).unwrap();
        assert_eq!(draft.entries.len(), 1);
        assert_eq!(draft.entries[0].media_asset_id, "2");
    }

    #[test]
    fn names_are_safe_deterministic_and_extension_preserving() {
        let first = build_manifest(&input()).unwrap();
        let second = build_manifest(&input()).unwrap();
        assert_eq!(first.checksum, second.checksum);
        assert_eq!(first.entries[0].destination_filename, "A _ Test_0001.ARW");
        assert!(safe_destination_relative_path(
            &first.entries[0].destination_relative_path
        ));
        assert_eq!(sanitize_component("CON / boat?."), "CON _ boat_");
        assert_eq!(sanitize_component("CON"), "_CON");
    }

    #[test]
    fn collision_and_unavailable_sources_block_manifest() {
        let mut request = input();
        request.filename_strategy = FilenameStrategy::PreserveOriginal;
        request.assets.push(asset("3", "DSC0001.ARW", Some("keep")));
        request.assets[2].sources.clear();
        let draft = build_manifest(&request).unwrap();
        assert!(!draft.is_ready());
        assert!(draft
            .issues
            .iter()
            .any(|issue| issue.code == "internal_destination_collision"));
        assert!(draft
            .issues
            .iter()
            .any(|issue| issue.code == "source_unavailable"));
    }

    #[test]
    fn moment_folders_use_human_then_conservative_then_ordinal_label() {
        let moment = MomentDestination {
            id: "m".into(),
            ordinal: 2,
            human_label: Some("Boat / Portraits".into()),
            suggested_label: Some("Ignored".into()),
        };
        assert_eq!(moment.folder_name(), "03_Boat _ Portraits");
        assert_eq!(
            MomentDestination {
                id: "m".into(),
                ordinal: 2,
                human_label: None,
                suggested_label: None
            }
            .folder_name(),
            "03_Moment_003"
        );
    }
}
