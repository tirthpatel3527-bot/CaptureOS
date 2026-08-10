//! Read-only recursive discovery, basic classification, and bounded content fingerprinting.
//! This crate never copies, mutates, or deletes source media.

use blake3::Hasher;
use chrono::{DateTime, Utc};
use media_model::{
    IndexIssueSeverity, JobStage, MediaFingerprint, MediaType, Provenance, StorageVolumeId,
    Timestamp,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Component, Path},
};
use thiserror::Error;

const SAMPLE_BYTES: usize = 64 * 1024;
const FULL_HASH_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SafeRelativePath(String);

/// Index mode understands existing user media in place. It is intentionally
/// separate from the future `ingest::IngestPlan` and contains no copy target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadOnlyIndexRequest {
    pub storage_volume_id: StorageVolumeId,
    pub root: SafeRelativePath,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathSafetyError {
    #[error("path may not be empty")]
    Empty,
    #[error("path contains a NUL byte")]
    NulByte,
    #[error("path must be relative")]
    Absolute,
    #[error("path traversal is not allowed")]
    Traversal,
    #[error("path contains an unsupported prefix")]
    Prefix,
}

impl SafeRelativePath {
    pub fn parse(value: impl AsRef<str>) -> Result<Self, PathSafetyError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(PathSafetyError::Empty);
        }
        if value.contains('\0') {
            return Err(PathSafetyError::NulByte);
        }
        if value.starts_with('/') || value.starts_with('\\') || is_windows_drive_path(value) {
            return Err(PathSafetyError::Absolute);
        }
        let path = Path::new(value);
        for component in path.components() {
            match component {
                Component::ParentDir => return Err(PathSafetyError::Traversal),
                Component::RootDir | Component::Prefix(_) => return Err(PathSafetyError::Absolute),
                Component::CurDir => return Err(PathSafetyError::Prefix),
                Component::Normal(_) => {}
            }
        }
        Ok(Self(value.replace('\\', "/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexCandidate {
    pub relative_path: SafeRelativePath,
    pub display_name: String,
    pub extension: Option<String>,
    pub media_type: MediaType,
    pub fingerprint: MediaFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexWarning {
    pub relative_path: Option<SafeRelativePath>,
    pub severity: IndexIssueSeverity,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexEvent {
    Stage(JobStage),
    FileDiscovered { count: u64 },
    Candidate(IndexCandidate),
    Warning(IndexWarning),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanStats {
    pub files_discovered: u64,
    pub files_processed: u64,
    pub warnings: u64,
}

#[derive(Debug, Error)]
pub enum IndexingError {
    #[error("selected root is unavailable: {0}")]
    RootUnavailable(String),
    #[error("selected root is not a directory")]
    NotDirectory,
    #[error("index observer failed: {0}")]
    Observer(String),
}

/// Walks a selected directory without following symlinks. Entries are sorted per directory
/// before visiting, giving stable results across repeated scans of unchanged fixtures.
pub fn scan_read_only(
    root: &Path,
    mut observer: impl FnMut(IndexEvent) -> Result<(), String>,
) -> Result<ScanStats, IndexingError> {
    let root = root
        .canonicalize()
        .map_err(|error| IndexingError::RootUnavailable(error.to_string()))?;
    if !root.is_dir() {
        return Err(IndexingError::NotDirectory);
    }

    let mut stats = ScanStats::default();
    emit(&mut observer, IndexEvent::Stage(JobStage::Discover))?;
    visit_directory(&root, &root, &mut stats, &mut observer)?;
    emit(&mut observer, IndexEvent::Stage(JobStage::Finalize))?;
    Ok(stats)
}

fn visit_directory(
    root: &Path,
    directory: &Path,
    stats: &mut ScanStats,
    observer: &mut impl FnMut(IndexEvent) -> Result<(), String>,
) -> Result<(), IndexingError> {
    let directory_entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            warning(
                observer,
                stats,
                relative_path(root, directory).ok(),
                format!("cannot read directory: {error}"),
            )?;
            return Ok(());
        }
    };

    let mut entries = Vec::new();
    for entry in directory_entries {
        match entry {
            Ok(entry) => entries.push(entry),
            Err(error) => warning(
                observer,
                stats,
                None,
                format!("cannot read directory entry: {error}"),
            )?,
        }
    }
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let relative = match relative_path(root, &path) {
            Ok(path) => path,
            Err(error) => {
                warning(
                    observer,
                    stats,
                    None,
                    format!("unsafe path skipped: {error}"),
                )?;
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                warning(
                    observer,
                    stats,
                    Some(relative),
                    format!("cannot inspect entry: {error}"),
                )?;
                continue;
            }
        };
        if file_type.is_symlink() {
            warning(
                observer,
                stats,
                Some(relative),
                "symlink skipped during read-only indexing".into(),
            )?;
            continue;
        }
        if file_type.is_dir() {
            visit_directory(root, &path, stats, observer)?;
            continue;
        }
        if !file_type.is_file() {
            warning(
                observer,
                stats,
                Some(relative),
                "non-regular filesystem entry skipped".into(),
            )?;
            continue;
        }

        stats.files_discovered += 1;
        emit(
            observer,
            IndexEvent::FileDiscovered {
                count: stats.files_discovered,
            },
        )?;
        emit(observer, IndexEvent::Stage(JobStage::Inspect))?;
        match inspect_file(&path, relative.clone()) {
            Ok(candidate) => {
                emit(observer, IndexEvent::Stage(JobStage::Classify))?;
                emit(observer, IndexEvent::Stage(JobStage::Fingerprint))?;
                emit(observer, IndexEvent::Candidate(candidate))?;
                stats.files_processed += 1;
            }
            Err(error) => warning(observer, stats, Some(relative), error)?,
        }
    }
    Ok(())
}

fn relative_path(root: &Path, path: &Path) -> Result<SafeRelativePath, PathSafetyError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| PathSafetyError::Traversal)?;
    SafeRelativePath::parse(relative.to_string_lossy())
}

fn warning(
    observer: &mut impl FnMut(IndexEvent) -> Result<(), String>,
    stats: &mut ScanStats,
    relative_path: Option<SafeRelativePath>,
    message: String,
) -> Result<(), IndexingError> {
    stats.warnings += 1;
    emit(
        observer,
        IndexEvent::Warning(IndexWarning {
            relative_path,
            severity: IndexIssueSeverity::Warning,
            message,
        }),
    )
}

fn emit(
    observer: &mut impl FnMut(IndexEvent) -> Result<(), String>,
    event: IndexEvent,
) -> Result<(), IndexingError> {
    observer(event).map_err(IndexingError::Observer)
}

fn inspect_file(path: &Path, relative_path: SafeRelativePath) -> Result<IndexCandidate, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("cannot read metadata: {error}"))?;
    let display_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| relative_path.as_str().to_owned());
    let extension = extension_for(path);
    let fingerprint = fingerprint_basic(path, &metadata, extension.as_deref())?;
    Ok(IndexCandidate {
        relative_path,
        display_name,
        media_type: classify_extension(extension.as_deref()),
        extension,
        fingerprint,
    })
}

pub fn extension_for(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .filter(|extension| !extension.is_empty())
}

pub fn classify_extension(extension: Option<&str>) -> MediaType {
    match extension.unwrap_or_default().to_ascii_lowercase().as_str() {
        "arw" | "cr2" | "cr3" | "dng" | "nef" | "orf" | "raf" | "rw2" | "pef" | "srw" | "3fr" => {
            MediaType::RawPhoto
        }
        "jpg" | "jpeg" => MediaType::Jpeg,
        "heic" | "heif" => MediaType::Heif,
        "png" => MediaType::Png,
        "tif" | "tiff" => MediaType::Tiff,
        "mov" | "mp4" | "m4v" | "avi" | "mxf" => MediaType::Video,
        "wav" | "mp3" | "m4a" | "aac" | "aif" | "aiff" => MediaType::Audio,
        "xmp" | "srt" | "xml" | "json" => MediaType::Sidecar,
        _ => MediaType::Unknown,
    }
}

/// Produces a BLAKE3 fast fingerprint from file size, extension, and bounded content samples.
/// Files at or below 1 MiB additionally receive a full BLAKE3 content hash.
pub fn fingerprint_basic(
    path: &Path,
    metadata: &fs::Metadata,
    extension: Option<&str>,
) -> Result<MediaFingerprint, String> {
    let size = metadata.len();
    let modified_at = metadata.modified().ok().and_then(system_time_to_timestamp);
    let mut file = File::open(path).map_err(|error| format!("cannot open read-only: {error}"))?;
    let mut hasher = Hasher::new();
    hasher.update(b"captureos-fast-v1\0");
    hasher.update(&size.to_le_bytes());
    hasher.update(extension.unwrap_or_default().as_bytes());
    hasher.update(&[0]);

    let mut buffer = vec![0_u8; SAMPLE_BYTES];
    let first_read = file
        .read(&mut buffer)
        .map_err(|error| format!("cannot read source: {error}"))?;
    hasher.update(&buffer[..first_read]);
    if size > (SAMPLE_BYTES * 2) as u64 {
        file.seek(SeekFrom::End(-(SAMPLE_BYTES as i64)))
            .map_err(|error| format!("cannot seek source: {error}"))?;
        let tail_read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read source tail: {error}"))?;
        hasher.update(&buffer[..tail_read]);
    } else {
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("cannot read source: {error}"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }

    let cryptographic_hash = if size <= FULL_HASH_MAX_BYTES {
        full_blake3(path)?
    } else {
        None
    };
    Ok(MediaFingerprint {
        cryptographic_hash,
        fast_fingerprint: Some(hasher.finalize().to_hex().to_string()),
        byte_size: Some(size),
        observed_modified_at: modified_at,
        perceptual_fingerprint: None,
        metadata_fingerprint: None,
    })
}

fn full_blake3(path: &Path) -> Result<Option<String>, String> {
    let mut file =
        File::open(path).map_err(|error| format!("cannot open for full hash: {error}"))?;
    let mut hasher = Hasher::new();
    let mut buffer = [0_u8; SAMPLE_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read for full hash: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(hasher.finalize().to_hex().to_string()))
}

fn system_time_to_timestamp(value: std::time::SystemTime) -> Option<Timestamp> {
    let duration = value.duration_since(std::time::UNIX_EPOCH).ok()?;
    DateTime::<Utc>::from_timestamp(duration.as_secs() as i64, duration.subsec_nanos())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataSnapshot {
    pub declared_media_type: MediaType,
    pub mime_type: Option<String>,
    pub fields: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryDescriptor {
    pub format: String,
    pub byte_size: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedReference {
    pub relative_path: SafeRelativePath,
    pub media_type: MediaType,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportDescriptor {
    pub target: SafeRelativePath,
    pub format: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraDescriptor {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisResult {
    pub kind: String,
    pub payload: serde_json::Value,
    pub provenance: Provenance,
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("adapter failure: {0}")]
    Failed(String),
}

pub trait MetadataExtractor: Send + Sync {
    fn extract(&self, path: &SafeRelativePath) -> Result<MetadataSnapshot, AdapterError>;
}
pub trait MediaDecoder: Send + Sync {
    fn describe(&self, path: &SafeRelativePath) -> Result<BinaryDescriptor, AdapterError>;
}
pub trait FingerprintProvider: Send + Sync {
    fn fingerprint(&self, path: &SafeRelativePath) -> Result<MediaFingerprint, AdapterError>;
}
pub trait StorageProvider: Send + Sync {
    fn availability(&self, path: &SafeRelativePath) -> Result<bool, AdapterError>;
}
pub trait AIAnalyzer: Send + Sync {
    fn analyze(&self, path: &SafeRelativePath) -> Result<Vec<AnalysisResult>, AdapterError>;
}
pub trait ThumbnailProvider: Send + Sync {
    fn create_thumbnail(&self, path: &SafeRelativePath)
        -> Result<GeneratedReference, AdapterError>;
}
pub trait ProxyProvider: Send + Sync {
    fn create_proxy(&self, path: &SafeRelativePath) -> Result<GeneratedReference, AdapterError>;
}
pub trait NLEExporter: Send + Sync {
    fn export(&self, assets: &[SafeRelativePath]) -> Result<ExportDescriptor, AdapterError>;
}
pub trait CameraAdapter: Send + Sync {
    fn describe_camera(&self, path: &SafeRelativePath) -> Result<CameraDescriptor, AdapterError>;
}
pub trait WorkflowAction: Send + Sync {
    fn name(&self) -> &str;
    fn execute(&self) -> Result<(), AdapterError>;
}
pub trait ProvenanceProvider: Send + Sync {
    fn provenance(&self) -> Result<Provenance, AdapterError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn safe_relative_paths_are_portable_and_reject_traversal() {
        assert_eq!(
            SafeRelativePath::parse("Golden\\RAW\\IMG_1.ARW")
                .unwrap()
                .as_str(),
            "Golden/RAW/IMG_1.ARW"
        );
        assert_eq!(
            SafeRelativePath::parse("../private/file").unwrap_err(),
            PathSafetyError::Traversal
        );
        assert_eq!(
            SafeRelativePath::parse("/private/file").unwrap_err(),
            PathSafetyError::Absolute
        );
        assert_eq!(
            SafeRelativePath::parse("C:\\private\\file").unwrap_err(),
            PathSafetyError::Absolute
        );
        assert_eq!(
            SafeRelativePath::parse("a\0b").unwrap_err(),
            PathSafetyError::NulByte
        );
    }

    #[test]
    fn basic_classifier_covers_required_media_families() {
        assert_eq!(classify_extension(Some("arw")), MediaType::RawPhoto);
        assert_eq!(classify_extension(Some("JPG")), MediaType::Jpeg);
        assert_eq!(classify_extension(Some("mov")), MediaType::Video);
        assert_eq!(classify_extension(Some("wav")), MediaType::Audio);
        assert_eq!(classify_extension(Some("xmp")), MediaType::Sidecar);
        assert_eq!(classify_extension(Some("bin")), MediaType::Unknown);
    }

    #[test]
    fn recursive_discovery_is_deterministic_and_read_only() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("b.jpg"), b"second").unwrap();
        let mut file = File::create(directory.path().join("nested/a.wav")).unwrap();
        file.write_all(b"first").unwrap();
        let original = fs::read(directory.path().join("b.jpg")).unwrap();
        let mut first = Vec::new();
        let stats = scan_read_only(directory.path(), |event| {
            first.push(event);
            Ok(())
        })
        .unwrap();
        let mut second = Vec::new();
        scan_read_only(directory.path(), |event| {
            second.push(event);
            Ok(())
        })
        .unwrap();
        assert_eq!(stats.files_discovered, 2);
        assert_eq!(stats.files_processed, 2);
        assert_eq!(first, second);
        assert_eq!(fs::read(directory.path().join("b.jpg")).unwrap(), original);
    }

    #[test]
    fn duplicate_content_has_a_stable_fast_fingerprint() {
        let directory = tempdir().unwrap();
        let first = directory.path().join("a.jpg");
        let second = directory.path().join("b.jpg");
        fs::write(&first, b"same media payload").unwrap();
        fs::write(&second, b"same media payload").unwrap();
        let one = fingerprint_basic(&first, &fs::metadata(&first).unwrap(), Some("jpg")).unwrap();
        let two = fingerprint_basic(&second, &fs::metadata(&second).unwrap(), Some("jpg")).unwrap();
        assert_eq!(one.fast_fingerprint, two.fast_fingerprint);
        assert!(one.cryptographic_hash.is_some());
    }
}
