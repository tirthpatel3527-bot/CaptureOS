//! Local, non-destructive ingest discovery, pre-flight, copy, and verification primitives.
//!
//! This crate never mutates a source path. Destination writes use a visible
//! `.captureos-partial` suffix until cryptographic verification succeeds.

use blake3::Hasher;
use media_index::{classify_extension, extension_for, SafeRelativePath};
use media_model::{IngestDestinationRole, MediaType};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::SystemTime,
};
use thiserror::Error;
use uuid::Uuid;

const COPY_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestSourceInput {
    pub label: String,
    pub selected_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestDestinationInput {
    pub role: IngestDestinationRole,
    pub selected_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestRequest {
    pub project_name: String,
    pub sources: Vec<IngestSourceInput>,
    pub master: IngestDestinationInput,
    pub backups: Vec<IngestDestinationInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredSourceFile {
    pub relative_path: String,
    pub byte_size: u64,
    pub extension: Option<String>,
    pub media_type: MediaType,
    #[serde(skip)]
    pub source_path: PathBuf,
    #[serde(skip)]
    pub modified_at: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInventory {
    pub label: String,
    pub selected_path: String,
    pub file_count: u64,
    pub total_bytes: u64,
    pub detected_media_types: Vec<String>,
    pub files: Vec<DiscoveredSourceFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightIssue {
    pub severity: PreflightSeverity,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationPreflight {
    pub role: IngestDestinationRole,
    pub selected_path: String,
    pub available_bytes: Option<u64>,
    pub required_bytes: u64,
    /// Available capacity remaining after this destination receives the full ingest.
    pub headroom_bytes: Option<u64>,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightReport {
    pub sources: Vec<SourceInventory>,
    pub destinations: Vec<DestinationPreflight>,
    pub total_source_bytes: u64,
    pub issues: Vec<PreflightIssue>,
}

impl PreflightReport {
    pub fn can_start(&self) -> bool {
        !self
            .issues
            .iter()
            .any(|issue| issue.severity == PreflightSeverity::Error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultDestinationLayout;

pub trait DestinationLayout {
    fn destination_file(
        &self,
        destination_root: &Path,
        project_name: &str,
        source_label: &str,
        relative_path: &str,
    ) -> Result<PathBuf, IngestError>;
}

impl DestinationLayout for DefaultDestinationLayout {
    fn destination_file(
        &self,
        destination_root: &Path,
        project_name: &str,
        source_label: &str,
        relative_path: &str,
    ) -> Result<PathBuf, IngestError> {
        let relative_path = SafeRelativePath::parse(relative_path)
            .map_err(|error| IngestError::UnsafePath(error.to_string()))?;
        let mut target = destination_root.join(safe_component(project_name));
        target.push("01_SOURCES");
        target.push(safe_component(source_label));
        for component in Path::new(relative_path.as_str()).components() {
            match component {
                Component::Normal(value) => target.push(value),
                _ => return Err(IngestError::UnsafePath(relative_path.as_str().into())),
            }
        }
        Ok(target)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyVerificationResult {
    pub source_hash: String,
    pub destination_hash: String,
    pub byte_size: u64,
    pub final_path: PathBuf,
    pub reused_existing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyVerificationOutcome {
    Verified(CopyVerificationResult),
    Conflict { message: String },
    VerificationFailed { message: String },
    SourceChanged { message: String },
    Cancelled,
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("path is unsafe: {0}")]
    UnsafePath(String),
    #[error("source is unavailable: {0}")]
    SourceUnavailable(String),
    #[error("destination is unavailable: {0}")]
    DestinationUnavailable(String),
    #[error("filesystem operation failed: {0}")]
    Filesystem(#[from] io::Error),
    #[error("pre-flight rejected the request: {0}")]
    Preflight(String),
}

pub trait AvailableSpace {
    fn available_bytes(&self, path: &Path) -> io::Result<u64>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalAvailableSpace;

impl AvailableSpace for LocalAvailableSpace {
    fn available_bytes(&self, path: &Path) -> io::Result<u64> {
        storage::available_bytes(path)
    }
}

pub fn preflight(
    request: &IngestRequest,
    capacity: &impl AvailableSpace,
) -> Result<PreflightReport, IngestError> {
    if request.sources.is_empty() {
        return Err(IngestError::Preflight(
            "select at least one source folder".into(),
        ));
    }
    let mut issues = Vec::new();
    let mut seen_sources = HashSet::new();
    let mut inventories = Vec::new();
    for source in &request.sources {
        let root = canonical_directory(&source.selected_path, true)?;
        if !seen_sources.insert(root.clone()) {
            issues.push(error(
                "duplicate_source",
                "The same source folder was selected twice.",
            ));
            continue;
        }
        inventories.push(discover_source(&root, &source.label, &mut issues)?);
    }

    let mut destination_inputs = Vec::with_capacity(request.backups.len() + 1);
    destination_inputs.push(request.master.clone());
    destination_inputs.extend(request.backups.clone());
    let total_source_bytes = inventories.iter().try_fold(0_u64, |total, source| {
        total
            .checked_add(source.total_bytes)
            .ok_or_else(|| IngestError::Preflight("source size overflow".into()))
    })?;
    let mut seen_destinations = HashSet::new();
    let mut destinations = Vec::new();
    for destination in destination_inputs {
        let root = canonical_directory(&destination.selected_path, false)?;
        if !seen_destinations.insert(root.clone()) {
            issues.push(error(
                "duplicate_destination",
                "Master and backup destinations must be different folders.",
            ));
        }
        for source in &inventories {
            let source_path = Path::new(&source.selected_path);
            if root == source_path {
                issues.push(error(
                    "same_source_destination",
                    "A destination cannot be the same directory as a source.",
                ));
            } else if root.starts_with(source_path) {
                issues.push(error(
                    "destination_inside_source",
                    "A destination cannot be nested inside a source folder.",
                ));
            } else if source_path.starts_with(&root) {
                issues.push(error(
                    "source_inside_destination",
                    "A source cannot be nested inside a destination folder.",
                ));
            }
        }
        let writable = writable_probe(&root);
        if !writable {
            issues.push(error(
                "destination_not_writable",
                format!("Destination is not writable: {}", root.display()),
            ));
        }
        let available_bytes = capacity.available_bytes(&root).ok();
        if available_bytes.is_some_and(|available| available < total_source_bytes) {
            issues.push(error(
                "insufficient_space",
                format!(
                    "Destination has insufficient free space: {}",
                    root.display()
                ),
            ));
        }
        destinations.push(DestinationPreflight {
            role: destination.role,
            selected_path: root.to_string_lossy().into_owned(),
            available_bytes,
            required_bytes: total_source_bytes,
            headroom_bytes: available_bytes
                .and_then(|available| available.checked_sub(total_source_bytes)),
            writable,
        });
    }

    Ok(PreflightReport {
        sources: inventories,
        destinations,
        total_source_bytes,
        issues,
    })
}

pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn verify_hashes(source: &Path, destination: &Path) -> io::Result<bool> {
    Ok(hash_file(source)? == hash_file(destination)?)
}

pub fn copy_and_verify(
    source: &Path,
    destination_root: &Path,
    final_path: &Path,
    mut cancelled: impl FnMut() -> bool,
    mut on_progress: impl FnMut(u64),
) -> Result<CopyVerificationOutcome, IngestError> {
    let initial_source_stamp = source_stamp(source)?;
    ensure_destination_target(destination_root, final_path)?;
    if final_path.exists() {
        if fs::symlink_metadata(final_path)?.file_type().is_symlink() {
            return Ok(CopyVerificationOutcome::Conflict {
                message: "destination file is a symlink".into(),
            });
        }
        let source_hash = hash_file(source)?;
        if source_stamp(source)? != initial_source_stamp {
            return Ok(CopyVerificationOutcome::SourceChanged {
                message: "source changed while checking an existing destination".into(),
            });
        }
        let destination_hash = hash_file(final_path)?;
        return if source_hash == destination_hash {
            Ok(CopyVerificationOutcome::Verified(CopyVerificationResult {
                source_hash,
                destination_hash,
                byte_size: initial_source_stamp.byte_size,
                final_path: final_path.to_path_buf(),
                reused_existing: true,
            }))
        } else {
            Ok(CopyVerificationOutcome::Conflict {
                message: "destination already contains different content".into(),
            })
        };
    }

    let partial_path = partial_path(final_path);
    if partial_path.exists() {
        let metadata = fs::symlink_metadata(&partial_path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Ok(CopyVerificationOutcome::Conflict {
                message: "existing partial path is unsafe".into(),
            });
        }
        fs::remove_file(&partial_path)?;
    }
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial_path)?;
    let mut hasher = Hasher::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut copied = 0_u64;
    loop {
        if cancelled() {
            output.sync_all()?;
            return Ok(CopyVerificationOutcome::Cancelled);
        }
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        copied = copied
            .checked_add(read as u64)
            .ok_or_else(|| IngestError::Preflight("copy size overflow".into()))?;
        on_progress(copied);
    }
    output.sync_all()?;
    drop(output);
    if source_stamp(source)? != initial_source_stamp || copied != initial_source_stamp.byte_size {
        return Ok(CopyVerificationOutcome::SourceChanged {
            message: "source changed during copy".into(),
        });
    }
    let source_hash = hasher.finalize().to_hex().to_string();
    let destination_hash = hash_file(&partial_path)?;
    if source_hash != destination_hash {
        return Ok(CopyVerificationOutcome::VerificationFailed {
            message: "destination verification disagreed with source copy evidence".into(),
        });
    }
    match finalize_partial_without_overwrite(&partial_path, final_path)? {
        FinalizeOutcome::Finalized => {}
        FinalizeOutcome::Conflict => {
            return Ok(CopyVerificationOutcome::Conflict {
                message: "destination appeared while the copy was in progress".into(),
            });
        }
    }
    Ok(CopyVerificationOutcome::Verified(CopyVerificationResult {
        source_hash,
        destination_hash,
        byte_size: copied,
        final_path: final_path.to_path_buf(),
        reused_existing: false,
    }))
}

fn canonical_directory(path: &str, source: bool) -> Result<PathBuf, IngestError> {
    let path = Path::new(path).canonicalize().map_err(|error| {
        if source {
            IngestError::SourceUnavailable(error.to_string())
        } else {
            IngestError::DestinationUnavailable(error.to_string())
        }
    })?;
    if !path.is_dir() {
        return Err(if source {
            IngestError::SourceUnavailable("selected source is not a folder".into())
        } else {
            IngestError::DestinationUnavailable("selected destination is not a folder".into())
        });
    }
    Ok(path)
}

fn discover_source(
    root: &Path,
    label: &str,
    issues: &mut Vec<PreflightIssue>,
) -> Result<SourceInventory, IngestError> {
    let mut files = Vec::new();
    visit_source_directory(root, root, &mut files, issues)?;
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    let total_bytes = files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.byte_size)
            .ok_or_else(|| IngestError::Preflight("source size overflow".into()))
    })?;
    let mut media_types = files
        .iter()
        .map(|file| file.media_type.as_str().to_owned())
        .collect::<Vec<_>>();
    media_types.sort();
    media_types.dedup();
    Ok(SourceInventory {
        label: if label.trim().is_empty() {
            "Source".into()
        } else {
            label.trim().to_owned()
        },
        selected_path: root.to_string_lossy().into_owned(),
        file_count: files.len() as u64,
        total_bytes,
        detected_media_types: media_types,
        files,
    })
}

fn visit_source_directory(
    root: &Path,
    directory: &Path,
    files: &mut Vec<DiscoveredSourceFile>,
    issues: &mut Vec<PreflightIssue>,
) -> Result<(), IngestError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| IngestError::SourceUnavailable(error.to_string()))?;
    let mut entries = entries.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            issues.push(warning(
                "symlink_skipped",
                format!("Skipped symlink in source: {}", path.display()),
            ));
            continue;
        }
        if file_type.is_dir() {
            visit_source_directory(root, &path, files, issues)?;
            continue;
        }
        if !file_type.is_file() {
            issues.push(warning(
                "non_regular_file_skipped",
                format!("Skipped non-regular source entry: {}", path.display()),
            ));
            continue;
        }
        let relative_path = path
            .strip_prefix(root)
            .map_err(|_| IngestError::UnsafePath(path.display().to_string()))?;
        let relative_path = SafeRelativePath::parse(relative_path.to_string_lossy())
            .map_err(|error| IngestError::UnsafePath(error.to_string()))?;
        let metadata = fs::metadata(&path)?;
        let extension = extension_for(&path);
        files.push(DiscoveredSourceFile {
            relative_path: relative_path.as_str().into(),
            byte_size: metadata.len(),
            media_type: classify_extension(extension.as_deref()),
            extension,
            source_path: path,
            modified_at: metadata.modified().ok(),
        });
    }
    Ok(())
}

fn writable_probe(destination: &Path) -> bool {
    let probe = destination.join(format!(".captureos-preflight-{}.tmp", Uuid::new_v4()));
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(file) => {
            drop(file);
            fs::remove_file(probe).is_ok()
        }
        Err(_) => false,
    }
}

fn ensure_destination_target(
    destination_root: &Path,
    final_path: &Path,
) -> Result<(), IngestError> {
    let relative = final_path
        .strip_prefix(destination_root)
        .map_err(|_| IngestError::UnsafePath("destination escaped selected root".into()))?;
    let mut current = destination_root.to_path_buf();
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    for component in parent.components() {
        let Component::Normal(component) = component else {
            return Err(IngestError::UnsafePath("destination path traversal".into()));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(IngestError::UnsafePath(format!(
                    "destination parent is a symlink: {}",
                    current.display()
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(IngestError::UnsafePath(format!(
                    "destination parent is not a directory: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn partial_path(final_path: &Path) -> PathBuf {
    let mut name: OsString = final_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("captureos-output"));
    name.push(".captureos-partial");
    final_path.with_file_name(name)
}

/// Finalize only when the final name is still unoccupied.  A plain `rename`
/// would replace a file that appears between a pre-check and finalization on
/// Unix, which violates ingest's no-overwrite rule.
fn finalize_partial_without_overwrite(
    partial_path: &Path,
    final_path: &Path,
) -> Result<FinalizeOutcome, IngestError> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let partial = CString::new(partial_path.as_os_str().as_bytes())
            .map_err(|_| IngestError::UnsafePath("partial path contained NUL".into()))?;
        let final_name = CString::new(final_path.as_os_str().as_bytes())
            .map_err(|_| IngestError::UnsafePath("destination path contained NUL".into()))?;
        // `RENAME_EXCL` asks the filesystem for an atomic no-replace rename.
        // It is intentionally not replaced with `rename`, whose overwrite
        // semantics would make a concurrent destination race destructive.
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                partial.as_ptr(),
                libc::AT_FDCWD,
                final_name.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result == 0 {
            return Ok(FinalizeOutcome::Finalized);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::AlreadyExists {
            Ok(FinalizeOutcome::Conflict)
        } else {
            Err(error.into())
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        // A same-directory hard link is atomic and fails if the final path
        // exists.  This is the conservative portable fallback; filesystems
        // without safe no-replace finalization surface an error and retain the
        // partial rather than risking an overwrite.
        match fs::hard_link(partial_path, final_path) {
            Ok(()) => {
                fs::remove_file(partial_path)?;
                Ok(FinalizeOutcome::Finalized)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Ok(FinalizeOutcome::Conflict)
            }
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalizeOutcome {
    Finalized,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceStamp {
    byte_size: u64,
    modified_at: Option<SystemTime>,
}

fn source_stamp(path: &Path) -> io::Result<SourceStamp> {
    let metadata = fs::metadata(path)?;
    Ok(SourceStamp {
        byte_size: metadata.len(),
        modified_at: metadata.modified().ok(),
    })
}

fn safe_component(value: &str) -> String {
    let normalized = value.trim();
    let result = normalized
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | ' ' | '_' | '-' | '.' | '+' => character,
            _ => '_',
        })
        .collect::<String>();
    let result = result.trim_matches('.').trim();
    if result.is_empty() || result == ".." {
        "Untitled".into()
    } else {
        result.into()
    }
}

fn error(code: impl Into<String>, message: impl Into<String>) -> PreflightIssue {
    PreflightIssue {
        severity: PreflightSeverity::Error,
        code: code.into(),
        message: message.into(),
    }
}

fn warning(code: impl Into<String>, message: impl Into<String>) -> PreflightIssue {
    PreflightIssue {
        severity: PreflightSeverity::Warning,
        code: code.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, fs};
    use tempfile::tempdir;

    struct FixedCapacity(u64);
    impl AvailableSpace for FixedCapacity {
        fn available_bytes(&self, _: &Path) -> io::Result<u64> {
            Ok(self.0)
        }
    }

    fn request(source: &Path, master: &Path, backups: Vec<&Path>) -> IngestRequest {
        IngestRequest {
            project_name: "Priya + Rahul".into(),
            sources: vec![IngestSourceInput {
                label: "Camera A".into(),
                selected_path: source.to_string_lossy().into_owned(),
            }],
            master: IngestDestinationInput {
                role: IngestDestinationRole::Master,
                selected_path: master.to_string_lossy().into_owned(),
            },
            backups: backups
                .into_iter()
                .map(|path| IngestDestinationInput {
                    role: IngestDestinationRole::Backup,
                    selected_path: path.to_string_lossy().into_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn preflight_discovers_sources_and_rejects_unsafe_path_relationships() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let master = directory.path().join("master");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir_all(&master).unwrap();
        fs::write(source.join("nested/IMG_0001.JPG"), b"camera bytes").unwrap();
        let report = preflight(&request(&source, &master, vec![]), &FixedCapacity(1024)).unwrap();
        assert!(report.can_start());
        assert_eq!(report.sources[0].file_count, 1);
        assert_eq!(report.sources[0].detected_media_types, vec!["jpeg"]);

        let invalid = preflight(&request(&source, &source, vec![]), &FixedCapacity(1024)).unwrap();
        assert!(!invalid.can_start());
        assert!(invalid
            .issues
            .iter()
            .any(|issue| issue.code == "same_source_destination"));
    }

    #[test]
    fn preflight_rejects_destination_inside_source_and_insufficient_space() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = source.join("destination");
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("C0001.MOV"), vec![9_u8; 32]).unwrap();
        let report = preflight(&request(&source, &destination, vec![]), &FixedCapacity(1)).unwrap();
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "destination_inside_source"));
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "insufficient_space"));
        assert_eq!(report.destinations[0].headroom_bytes, None);
    }

    #[test]
    fn preflight_rejects_duplicate_roots_and_source_inside_destination() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("destination");
        let source = destination.join("camera");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("IMG_0001.JPG"), b"bytes").unwrap();
        let mut input = request(&source, &destination, vec![&destination]);
        input.sources.push(IngestSourceInput {
            label: "Camera duplicate".into(),
            selected_path: source.to_string_lossy().into_owned(),
        });
        let report = preflight(&input, &FixedCapacity(100)).unwrap();
        assert!(!report.can_start());
        for expected in [
            "duplicate_source",
            "duplicate_destination",
            "source_inside_destination",
        ] {
            assert!(report.issues.iter().any(|issue| issue.code == expected));
        }
        assert_eq!(report.destinations[0].headroom_bytes, Some(95));
    }

    #[test]
    fn copy_verifies_and_never_mutates_the_source() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.mov");
        let destination = directory.path().join("destination");
        fs::create_dir(&destination).unwrap();
        let original = b"large-ish source media".repeat(128);
        fs::write(&source, &original).unwrap();
        let target = destination.join("Priya/01_SOURCES/Camera_A/source.mov");
        let outcome = copy_and_verify(&source, &destination, &target, || false, |_| {}).unwrap();
        assert!(matches!(outcome, CopyVerificationOutcome::Verified(_)));
        assert_eq!(fs::read(&source).unwrap(), original);
        assert!(verify_hashes(&source, &target).unwrap());
        assert!(!target
            .with_file_name("source.mov.captureos-partial")
            .exists());
    }

    #[test]
    fn existing_identical_destination_is_reused_and_conflict_is_not_overwritten() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.jpg");
        let destination = directory.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"same").unwrap();
        let target = destination.join("source.jpg");
        fs::write(&target, b"same").unwrap();
        let reused = copy_and_verify(&source, &destination, &target, || false, |_| {}).unwrap();
        assert!(
            matches!(reused, CopyVerificationOutcome::Verified(result) if result.reused_existing)
        );
        fs::write(&target, b"different").unwrap();
        let conflict = copy_and_verify(&source, &destination, &target, || false, |_| {}).unwrap();
        assert!(matches!(conflict, CopyVerificationOutcome::Conflict { .. }));
        assert_eq!(fs::read(&target).unwrap(), b"different");
    }

    #[test]
    fn cancelled_copy_leaves_only_a_marked_partial_file() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.mov");
        let destination = directory.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, vec![4_u8; COPY_BUFFER_BYTES * 2]).unwrap();
        let target = destination.join("source.mov");
        let calls = Cell::new(0);
        let outcome = copy_and_verify(
            &source,
            &destination,
            &target,
            || {
                calls.set(calls.get() + 1);
                calls.get() > 1
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(outcome, CopyVerificationOutcome::Cancelled);
        assert!(!target.exists());
        assert!(target
            .with_file_name("source.mov.captureos-partial")
            .exists());
    }

    #[test]
    fn source_change_during_copy_is_not_verified() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.mov");
        let destination = directory.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, vec![7_u8; COPY_BUFFER_BYTES * 2]).unwrap();
        let target = destination.join("source.mov");
        let changed = Cell::new(false);
        let outcome = copy_and_verify(
            &source,
            &destination,
            &target,
            || false,
            |_| {
                if !changed.replace(true) {
                    fs::write(&source, b"changed while copying").unwrap();
                }
            },
        )
        .unwrap();
        assert!(matches!(
            outcome,
            CopyVerificationOutcome::SourceChanged { .. }
        ));
        assert!(!target.exists());
    }

    #[test]
    fn verification_hash_detects_changed_destination_bytes() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.jpg");
        let destination = directory.path().join("destination.jpg");
        fs::write(&source, b"verified source bytes").unwrap();
        fs::write(&destination, b"different destination bytes").unwrap();
        assert!(!verify_hashes(&source, &destination).unwrap());
    }

    #[test]
    fn stale_partial_is_restarted_and_never_reported_as_final() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.mov");
        let destination = directory.path().join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"complete source bytes").unwrap();
        let target = destination.join("source.mov");
        let stale_partial = target.with_file_name("source.mov.captureos-partial");
        fs::write(&stale_partial, b"incomplete stale bytes").unwrap();

        let outcome = copy_and_verify(&source, &destination, &target, || false, |_| {}).unwrap();

        assert!(matches!(outcome, CopyVerificationOutcome::Verified(_)));
        assert_eq!(fs::read(&target).unwrap(), b"complete source bytes");
        assert!(!stale_partial.exists());
    }

    #[test]
    fn layout_keeps_source_namespaces_and_nested_paths_distinct() {
        let destination = Path::new("/destination");
        let layout = DefaultDestinationLayout;
        let camera_a = layout
            .destination_file(
                destination,
                "Priya + Rahul",
                "Camera A",
                "DCIM/DSC_0001.JPG",
            )
            .unwrap();
        let camera_b = layout
            .destination_file(
                destination,
                "Priya + Rahul",
                "Camera B",
                "DCIM/DSC_0001.JPG",
            )
            .unwrap();
        assert_ne!(camera_a, camera_b);
        assert!(camera_a.ends_with("Priya + Rahul/01_SOURCES/Camera A/DCIM/DSC_0001.JPG"));
        assert!(camera_b.ends_with("Priya + Rahul/01_SOURCES/Camera B/DCIM/DSC_0001.JPG"));
    }

    #[test]
    fn finalization_refuses_to_replace_a_file_that_appears_at_final_name() {
        let directory = tempdir().unwrap();
        let partial = directory.path().join("C0001.MOV.captureos-partial");
        let final_path = directory.path().join("C0001.MOV");
        fs::write(&partial, b"new verified bytes").unwrap();
        fs::write(&final_path, b"existing destination bytes").unwrap();

        assert_eq!(
            finalize_partial_without_overwrite(&partial, &final_path).unwrap(),
            FinalizeOutcome::Conflict
        );
        assert_eq!(
            fs::read(&final_path).unwrap(),
            b"existing destination bytes"
        );
        assert_eq!(fs::read(&partial).unwrap(), b"new verified bytes");
    }

    #[cfg(unix)]
    #[test]
    fn source_symlinks_are_skipped_without_following_them() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let source = directory.path().join("source");
        let master = directory.path().join("master");
        let outside = directory.path().join("outside.jpg");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&master).unwrap();
        fs::write(&outside, b"must not be discovered through symlink").unwrap();
        symlink(&outside, source.join("linked.jpg")).unwrap();

        let report = preflight(&request(&source, &master, vec![]), &FixedCapacity(1024)).unwrap();
        assert_eq!(report.sources[0].file_count, 0);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "symlink_skipped"));
    }
}
