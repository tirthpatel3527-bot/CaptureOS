//! Storage-volume policy and platform observations.
//!
//! A `StorageVolume` describes a mounted filesystem, while a selected folder is an
//! `IndexRoot`. This module never treats a selected folder name as volume metadata.

use media_model::{MountState, StorageClassification, StorageVolume};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeObservation {
    /// An OS-derived identity used to reuse one CaptureOS volume record while mounted.
    pub filesystem_identity: String,
    /// A human-readable volume or device label, never a selected folder basename.
    pub display_name: String,
    /// Filesystem mount root, not an index root.
    pub mount_location: Option<String>,
    pub capacity_bytes: Option<u64>,
    pub filesystem_type: Option<String>,
    pub classification: StorageClassification,
}

pub trait VolumeInspector {
    fn inspect(&self, path: &Path) -> io::Result<VolumeObservation>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct LocalVolumeInspector;

impl VolumeInspector for LocalVolumeInspector {
    fn inspect(&self, path: &Path) -> io::Result<VolumeObservation> {
        inspect_local_volume(path)
    }
}

pub fn inspect_local_volume(path: &Path) -> io::Result<VolumeObservation> {
    let metadata = fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let filesystem_identity = format!("unix-device:{}", metadata.dev());
        let mount_root = mount_root_for_device(path, metadata.dev());
        let mut observation = VolumeObservation {
            filesystem_identity,
            display_name: fallback_display_name(&mount_root),
            mount_location: Some(mount_root.to_string_lossy().into_owned()),
            capacity_bytes: None,
            filesystem_type: None,
            classification: StorageClassification::Unknown,
        };
        #[cfg(target_os = "macos")]
        apply_macos_metadata(path, &mut observation);
        Ok(observation)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Ok(VolumeObservation {
            filesystem_identity: format!("local-root:{}", path.to_string_lossy()),
            display_name: "Local volume".into(),
            mount_location: None,
            capacity_bytes: None,
            filesystem_type: None,
            classification: StorageClassification::Unknown,
        })
    }
}

/// Returns filesystem-reported available bytes for pre-flight capacity checks.
/// It is intentionally an observation, not a reservation.
pub fn available_bytes(path: &Path) -> io::Result<u64> {
    #[cfg(target_os = "macos")]
    {
        use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let mut filesystem = MaybeUninit::<libc::statfs>::zeroed();
        let result = unsafe { libc::statfs(path.as_ptr(), filesystem.as_mut_ptr()) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        let filesystem = unsafe { filesystem.assume_init() };
        filesystem
            .f_bavail
            .checked_mul(filesystem.f_bsize as u64)
            .ok_or_else(|| io::Error::other("available space overflow"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let mut filesystem = MaybeUninit::<libc::statvfs>::zeroed();
        let result = unsafe { libc::statvfs(path.as_ptr(), filesystem.as_mut_ptr()) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        let filesystem = unsafe { filesystem.assume_init() };
        (filesystem.f_bavail as u64)
            .checked_mul(filesystem.f_frsize as u64)
            .ok_or_else(|| io::Error::other("available space overflow"))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "available space is unavailable on this platform",
        ))
    }
}

#[cfg(unix)]
fn mount_root_for_device(path: &Path, device: u64) -> PathBuf {
    use std::os::unix::fs::MetadataExt;

    let mut current = path.to_path_buf();
    while let Some(parent) = current.parent() {
        match fs::metadata(parent) {
            Ok(metadata) if metadata.dev() == device => current = parent.to_path_buf(),
            _ => break,
        }
    }
    current
}

fn fallback_display_name(mount_root: &Path) -> String {
    if mount_root == Path::new("/") {
        "System volume".into()
    } else {
        mount_root
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .map(|value| format!("Volume {value}"))
            .unwrap_or_else(|| "Local volume".into())
    }
}

#[cfg(target_os = "macos")]
fn apply_macos_metadata(path: &Path, observation: &mut VolumeObservation) {
    if let Some(metadata) = macos_volume_metadata(path) {
        if let Some(name) = metadata.name.filter(|value| !value.is_empty()) {
            observation.display_name = name;
        } else if let Some(device) = metadata.device.filter(|value| !value.is_empty()) {
            observation.display_name = device;
        }
        if let Some(mount_root) = metadata.mount_root.filter(|value| !value.is_empty()) {
            observation.mount_location = Some(mount_root);
        }
        observation.capacity_bytes = metadata.capacity_bytes;
        observation.filesystem_type = metadata.filesystem_type;
        observation.classification = match observation.filesystem_type.as_deref() {
            Some("smbfs" | "nfs" | "webdav") => StorageClassification::Network,
            _ => StorageClassification::Unknown,
        };
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct MacosVolumeMetadata {
    name: Option<String>,
    device: Option<String>,
    mount_root: Option<String>,
    capacity_bytes: Option<u64>,
    filesystem_type: Option<String>,
}

#[cfg(target_os = "macos")]
fn macos_volume_metadata(path: &Path) -> Option<MacosVolumeMetadata> {
    use std::{
        ffi::{CStr, CString},
        mem::MaybeUninit,
        os::unix::ffi::OsStrExt,
    };

    #[repr(C)]
    struct NameBuffer {
        length: u32,
        name: libc::attrreference_t,
        data: [u8; 1024],
    }

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_NAME,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut name_buffer = MaybeUninit::<NameBuffer>::zeroed();
    let name_result = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&mut attributes as *mut libc::attrlist).cast(),
            name_buffer.as_mut_ptr().cast(),
            std::mem::size_of::<NameBuffer>(),
            0,
        )
    };
    let name = if name_result == 0 {
        let name_buffer = unsafe { name_buffer.assume_init() };
        let offset = name_buffer.name.attr_dataoffset;
        let length = name_buffer.name.attr_length as usize;
        let reference = (&name_buffer.name as *const libc::attrreference_t).cast::<u8>();
        let base = (&name_buffer as *const NameBuffer).cast::<u8>();
        let reference_offset = (reference as usize).checked_sub(base as usize);
        match (reference_offset, usize::try_from(offset).ok()) {
            (Some(reference_offset), Some(offset))
                if offset <= std::mem::size_of::<NameBuffer>() - reference_offset
                    && length <= std::mem::size_of::<NameBuffer>() - reference_offset - offset =>
            {
                let bytes = unsafe { std::slice::from_raw_parts(reference.add(offset), length) };
                CStr::from_bytes_until_nul(bytes)
                    .ok()
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            }
            _ => None,
        }
    } else {
        None
    };

    let mut filesystem = MaybeUninit::<libc::statfs>::zeroed();
    let stat_result = unsafe { libc::statfs(path.as_ptr(), filesystem.as_mut_ptr()) };
    if stat_result != 0 {
        return Some(MacosVolumeMetadata {
            name,
            device: None,
            mount_root: None,
            capacity_bytes: None,
            filesystem_type: None,
        });
    }
    let filesystem = unsafe { filesystem.assume_init() };
    let device = c_string(&filesystem.f_mntfromname);
    let mount_root = c_string(&filesystem.f_mntonname);
    let filesystem_type = c_string(&filesystem.f_fstypename);
    Some(MacosVolumeMetadata {
        name,
        device,
        mount_root,
        capacity_bytes: filesystem.f_blocks.checked_mul(filesystem.f_bsize as u64),
        filesystem_type,
    })
}

#[cfg(target_os = "macos")]
fn c_string(value: &[libc::c_char]) -> Option<String> {
    use std::ffi::CStr;

    unsafe { CStr::from_ptr(value.as_ptr()) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

pub fn is_online(volume: &StorageVolume) -> bool {
    volume.mount_state == MountState::Online
}

pub fn identity_label(volume: &StorageVolume) -> String {
    volume
        .filesystem_identity
        .clone()
        .unwrap_or_else(|| volume.id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn local_observation_never_uses_the_selected_folder_as_volume_metadata() {
        let directory = tempdir().unwrap();
        let selected_root = directory.path().join("explicit-index-root");
        fs::create_dir(&selected_root).unwrap();

        let observation = inspect_local_volume(&selected_root).unwrap();

        assert_ne!(observation.display_name, "explicit-index-root");
        assert_ne!(
            observation.mount_location.as_deref(),
            selected_root.to_str()
        );
    }
}
