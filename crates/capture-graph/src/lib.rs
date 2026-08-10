//! CaptureGraph relationship types. These records can evolve without a schema-per-edge redesign.

use chrono::Utc;
use media_model::{Provenance, RelationshipId, Timestamp};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Project,
    Shoot,
    Moment,
    Scene,
    PersonCluster,
    MediaAsset,
    FileInstance,
    StorageVolume,
    CaptureDevice,
    Derivative,
    BackupCopy,
    AnalysisArtifact,
    SimilarityGroup,
    FaceAnalysis,
    TechnicalAnalysis,
}

impl EntityKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Shoot => "shoot",
            Self::Moment => "moment",
            Self::Scene => "scene",
            Self::PersonCluster => "person_cluster",
            Self::MediaAsset => "media_asset",
            Self::FileInstance => "file_instance",
            Self::StorageVolume => "storage_volume",
            Self::CaptureDevice => "capture_device",
            Self::Derivative => "derivative",
            Self::BackupCopy => "backup_copy",
            Self::AnalysisArtifact => "analysis_artifact",
            Self::SimilarityGroup => "similarity_group",
            Self::FaceAnalysis => "face_analysis",
            Self::TechnicalAnalysis => "technical_analysis",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRef {
    pub kind: EntityKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RelationshipKind {
    CapturedBy,
    ContainsPerson,
    BelongsToMoment,
    BelongsToScene,
    StoredOn,
    BackedUpAs,
    CopiedFrom,
    VerifiedCopyOf,
    DerivedFrom,
    SimilarTo,
    HasAudio,
    CapturedNear,
    CreatedBy,
    ExportedTo,
    SidecarOf,
    MemberOfSequence,
    HasFaceAnalysis,
    HasTechnicalAnalysis,
    Custom(String),
}

impl RelationshipKind {
    pub fn as_str(&self) -> String {
        match self {
            Self::CapturedBy => "CAPTURED_BY".into(),
            Self::ContainsPerson => "CONTAINS_PERSON".into(),
            Self::BelongsToMoment => "BELONGS_TO_MOMENT".into(),
            Self::BelongsToScene => "BELONGS_TO_SCENE".into(),
            Self::StoredOn => "STORED_ON".into(),
            Self::BackedUpAs => "BACKED_UP_AS".into(),
            Self::CopiedFrom => "COPIED_FROM".into(),
            Self::VerifiedCopyOf => "VERIFIED_COPY_OF".into(),
            Self::DerivedFrom => "DERIVED_FROM".into(),
            Self::SimilarTo => "SIMILAR_TO".into(),
            Self::HasAudio => "HAS_AUDIO".into(),
            Self::CapturedNear => "CAPTURED_NEAR".into(),
            Self::CreatedBy => "CREATED_BY".into(),
            Self::ExportedTo => "EXPORTED_TO".into(),
            Self::SidecarOf => "SIDECAR_OF".into(),
            Self::MemberOfSequence => "MEMBER_OF_SEQUENCE".into(),
            Self::HasFaceAnalysis => "HAS_FACE_ANALYSIS".into(),
            Self::HasTechnicalAnalysis => "HAS_TECHNICAL_ANALYSIS".into(),
            Self::Custom(value) => value.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    pub id: RelationshipId,
    pub source: EntityRef,
    pub target: EntityRef,
    pub kind: RelationshipKind,
    /// 0.0 to 1.0 when known. It is intentionally optional for direct facts.
    pub confidence: Option<f64>,
    pub provenance: Provenance,
    pub created_at: Timestamp,
}

#[derive(Debug, Error, PartialEq)]
pub enum RelationshipError {
    #[error("relationship confidence must be between 0 and 1")]
    InvalidConfidence,
    #[error("relationship endpoints must be non-empty")]
    EmptyEndpoint,
}

impl Relationship {
    pub fn validate(&self) -> Result<(), RelationshipError> {
        if self.source.id.is_empty() || self.target.id.is_empty() {
            return Err(RelationshipError::EmptyEndpoint);
        }
        if self
            .confidence
            .is_some_and(|value| !(0.0..=1.0).contains(&value))
        {
            return Err(RelationshipError::InvalidConfidence);
        }
        Ok(())
    }
}

pub fn inferred_relationship(
    source: EntityRef,
    target: EntityRef,
    kind: RelationshipKind,
    confidence: f64,
    algorithm_id: &str,
    algorithm_version: &str,
) -> Result<Relationship, RelationshipError> {
    let relationship = Relationship {
        id: RelationshipId::new(),
        source,
        target,
        kind,
        confidence: Some(confidence),
        provenance: Provenance {
            source: "inference".into(),
            algorithm_id: Some(algorithm_id.into()),
            algorithm_version: Some(algorithm_version.into()),
            produced_at: Utc::now(),
            human_confirmed: false,
        },
        created_at: Utc::now(),
    };
    relationship.validate()?;
    Ok(relationship)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_ai_confidence_and_provenance() {
        let relationship = inferred_relationship(
            EntityRef {
                kind: EntityKind::MediaAsset,
                id: "asset-1".into(),
            },
            EntityRef {
                kind: EntityKind::Moment,
                id: "moment-1".into(),
            },
            RelationshipKind::BelongsToMoment,
            0.87,
            "fixture-model",
            "1",
        )
        .unwrap();
        assert_eq!(relationship.confidence, Some(0.87));
        assert!(!relationship.provenance.human_confirmed);
    }

    #[test]
    fn rejects_out_of_range_confidence() {
        let result = inferred_relationship(
            EntityRef {
                kind: EntityKind::MediaAsset,
                id: "asset-1".into(),
            },
            EntityRef {
                kind: EntityKind::Moment,
                id: "moment-1".into(),
            },
            RelationshipKind::BelongsToMoment,
            1.1,
            "fixture-model",
            "1",
        );
        assert_eq!(result.unwrap_err(), RelationshipError::InvalidConfidence);
    }
}
