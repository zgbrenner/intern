//! Detection of OneDrive/SharePoint sync roots and network shares on the
//! local machine.
//!
//! Intern never talks to Microsoft Graph. A "cloud" folder is simply a local
//! path that the Microsoft sync client replicates, so detection is a matter of
//! reading the sync client's own breadcrumbs: environment variables and the
//! per-user registry on Windows, well-known home-directory names elsewhere.
//! A network share is recognised from the path itself - a UNC path, or on
//! Windows a drive letter that the operating system reports as remote.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloudProviderKind {
    OneDrivePersonal,
    OneDriveBusiness,
    SharePoint,
    /// A folder reached over the network rather than through a sync client:
    /// a UNC path such as `\\fileserver\legal`, or a mapped drive letter.
    NetworkShare,
}

impl CloudProviderKind {
    /// The wire spelling shared with the desktop app's DTOs and the
    /// description records written beside filed documents.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OneDrivePersonal => "onedrive_personal",
            Self::OneDriveBusiness => "onedrive_business",
            Self::SharePoint => "sharepoint",
            Self::NetworkShare => "network_share",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloudRoot {
    pub kind: CloudProviderKind,
    pub display_name: String,
    pub root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloudLocation {
    pub kind: CloudProviderKind,
    pub display_name: String,
}

/// Seam over the process environment so detection is testable without a real
/// OneDrive installation.
pub trait EnvProbe {
    fn var(&self, name: &str) -> Option<String>;
    /// Immediate subdirectories of `path`; empty when it cannot be read.
    fn subdirectories(&self, path: &Path) -> Vec<PathBuf>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemEnv;

impl EnvProbe for SystemEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    }

    fn subdirectories(&self, path: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect()
    }
}

pub fn detect_cloud_roots() -> Vec<CloudRoot> {
    detect_cloud_roots_with(&SystemEnv)
}

pub fn detect_cloud_roots_with(env: &dyn EnvProbe) -> Vec<CloudRoot> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    {
        windows_registry::append_registry_roots(&mut roots);
        append_windows_env_roots(env, &mut roots);
    }
    #[cfg(not(windows))]
    append_home_roots(env, &mut roots);
    roots
}

/// Where a folder lives: inside a sync root when one contains it (the
/// longest match wins, so a SharePoint library mounted inside a OneDrive
/// folder is reported as SharePoint), otherwise on a network share when the
/// path says so, otherwise nowhere special.
pub fn classify(path: &Path, roots: &[CloudRoot]) -> Option<CloudLocation> {
    if let Some(root) = matching_root(path, roots) {
        return Some(CloudLocation {
            kind: root.kind,
            display_name: root.display_name.clone(),
        });
    }
    network_share(path).map(|share| CloudLocation {
        kind: CloudProviderKind::NetworkShare,
        display_name: share,
    })
}

/// The sync root containing `path`, if any: longest-prefix match,
/// component-wise and case-insensitive.
pub fn matching_root<'a>(path: &Path, roots: &'a [CloudRoot]) -> Option<&'a CloudRoot> {
    let components = path_components(path);
    let mut best: Option<(usize, &CloudRoot)> = None;
    for root in roots {
        let root_components = path_components(&root.root);
        if root_components.is_empty() || root_components.len() > components.len() {
            continue;
        }
        if components[..root_components.len()] == root_components[..]
            && best.is_none_or(|(length, _)| root_components.len() > length)
        {
            best = Some((root_components.len(), root));
        }
    }
    best.map(|(_, root)| root)
}

/// `path` relative to `root`, `/`-separated and in the path's own casing, or
/// `None` when `root` does not contain it. The comparison folds case and the
/// verbatim prefix the same way `matching_root` does, so a canonical
/// `\\?\C:\...` path resolves against a registry root written `C:\...`.
pub fn relative_to_root(path: &Path, root: &Path) -> Option<String> {
    let root_components = path_components(root);
    let text = path.to_string_lossy();
    let (unc, rest) = strip_verbatim(&text);
    let mut parts: Vec<&str> = Vec::new();
    if unc {
        parts.push(UNC_MARKER);
    } else if rest.starts_with(['\\', '/']) {
        parts.push("");
    }
    parts.extend(
        rest.split(['\\', '/'])
            .filter(|part| !part.is_empty() && *part != "."),
    );
    if parts.len() < root_components.len()
        || parts[..root_components.len()]
            .iter()
            .zip(&root_components)
            .any(|(part, expected)| part.to_lowercase() != *expected)
    {
        return None;
    }
    let relative = parts[root_components.len()..].join("/");
    (!relative.is_empty()).then_some(relative)
}

/// The share a path reaches over the network, as `\\server\share`, when it is
/// one: a UNC path in either its plain or its verbatim spelling, or on
/// Windows a drive letter the operating system reports as a network drive.
pub fn network_share(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    if let Some(share) = unc_share(&text) {
        return Some(share);
    }
    #[cfg(windows)]
    {
        let (_, rest) = strip_verbatim(&text);
        let mut characters = rest.chars();
        if let (Some(letter), Some(':')) = (characters.next(), characters.next())
            && letter.is_ascii_alphabetic()
        {
            return windows_network::remote_drive(letter.to_ascii_uppercase());
        }
    }
    None
}

/// `\\server\share` for a UNC path written as `\\server\share\...` or as the
/// verbatim `\\?\UNC\server\share\...` that `canonicalize` produces.
pub fn unc_share(text: &str) -> Option<String> {
    let (unc, rest) = strip_verbatim(text);
    if !unc {
        return None;
    }
    let mut parts = rest.split(['\\', '/']).filter(|part| !part.is_empty());
    let server = parts.next()?;
    let share = parts.next()?;
    Some(format!(r"\\{server}\{share}"))
}

const UNC_MARKER: &str = r"\\";

/// A path as a lowercase list of components with Windows' verbatim prefix
/// and drive-letter case folded away, so the registry's
/// `C:\Users\pat\OneDrive` and `canonicalize`'s `\\?\C:\Users\pat\OneDrive`
/// are the same root. Both separators split, because a sync client and a
/// file dialog can disagree on which one to write. UNC paths carry a marker
/// so `\\server\share` and a relative `server\share` never coincide; POSIX
/// absolute paths carry an empty root marker for the same reason.
pub(crate) fn path_components(path: &Path) -> Vec<String> {
    let text = path.to_string_lossy();
    let (unc, rest) = strip_verbatim(&text);
    let mut components: Vec<String> = Vec::new();
    if unc {
        components.push(UNC_MARKER.to_owned());
    } else if rest.starts_with(['\\', '/']) {
        components.push(String::new());
    }
    for part in rest.split(['\\', '/']) {
        if part.is_empty() || part == "." {
            continue;
        }
        components.push(part.to_lowercase());
    }
    components
}

/// Splits off Windows' `\\?\` / `\\.\` prefixes. Returns whether the path is
/// a UNC path and the remainder after the prefix (for a UNC path, starting at
/// the server name).
fn strip_verbatim(text: &str) -> (bool, &str) {
    for prefix in [r"\\?\UNC\", r"\\.\UNC\"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return (true, rest);
        }
    }
    for prefix in [r"\\?\", r"\\.\"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return (false, rest);
        }
    }
    if let Some(rest) = text.strip_prefix(r"\\") {
        return (true, rest);
    }
    (false, text)
}

#[cfg(not(windows))]
fn append_home_roots(env: &dyn EnvProbe, roots: &mut Vec<CloudRoot>) {
    let Some(home) = env.var("HOME") else {
        return;
    };
    let home = PathBuf::from(home);
    for directory in env.subdirectories(&home) {
        push_onedrive_directory(&directory, roots);
    }
    for directory in env.subdirectories(&home.join("Library").join("CloudStorage")) {
        push_onedrive_directory(&directory, roots);
    }
}

#[cfg(not(windows))]
fn push_onedrive_directory(directory: &Path, roots: &mut Vec<CloudRoot>) {
    let Some(name) = directory.file_name().and_then(|value| value.to_str()) else {
        return;
    };
    if !name.starts_with("OneDrive") {
        return;
    }
    let (kind, display_name) = onedrive_identity(name);
    push_unique(
        roots,
        CloudRoot {
            kind,
            display_name,
            root: directory.to_path_buf(),
        },
    );
}

#[cfg(not(windows))]
fn onedrive_identity(name: &str) -> (CloudProviderKind, String) {
    let suffix = name["OneDrive".len()..]
        .trim_start_matches(['-', '–', '—', '_', ' '])
        .trim();
    if suffix.is_empty() || suffix.eq_ignore_ascii_case("personal") {
        (
            CloudProviderKind::OneDrivePersonal,
            "OneDrive – Personal".to_string(),
        )
    } else {
        (
            CloudProviderKind::OneDriveBusiness,
            format!("OneDrive – {suffix}"),
        )
    }
}

#[cfg(windows)]
fn append_windows_env_roots(env: &dyn EnvProbe, roots: &mut Vec<CloudRoot>) {
    let sources = [
        (
            "OneDriveConsumer",
            CloudProviderKind::OneDrivePersonal,
            "OneDrive – Personal",
        ),
        (
            "OneDriveCommercial",
            CloudProviderKind::OneDriveBusiness,
            "OneDrive – Work",
        ),
        (
            "OneDrive",
            CloudProviderKind::OneDrivePersonal,
            "OneDrive – Personal",
        ),
    ];
    for (variable, kind, display_name) in sources {
        if let Some(value) = env.var(variable) {
            push_unique(
                roots,
                CloudRoot {
                    kind,
                    display_name: display_name.to_string(),
                    root: PathBuf::from(value),
                },
            );
        }
    }
}

fn push_unique(roots: &mut Vec<CloudRoot>, candidate: CloudRoot) {
    let key = path_components(&candidate.root);
    if key.is_empty() {
        return;
    }
    if roots
        .iter()
        .all(|existing| path_components(&existing.root) != key)
    {
        roots.push(candidate);
    }
}

/// Raw Win32 drive and network queries: whether a drive letter is a mapped
/// network drive, and which share it maps to.
#[cfg(windows)]
mod windows_network {
    #![allow(unsafe_code)]

    use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

    use windows_sys::Win32::{
        Foundation::NO_ERROR, NetworkManagement::WNet::WNetGetConnectionW,
        Storage::FileSystem::GetDriveTypeW,
    };

    /// `GetDriveTypeW`'s answer for a network drive. The constant lives in
    /// windows-sys' WindowsProgramming module, which is not worth a feature
    /// for one number.
    const DRIVE_REMOTE: u32 = 4;

    /// The share behind a mapped network drive letter, as
    /// `\\server\share (Z:)`, or the bare letter when the mapping cannot be
    /// read; `None` for a local drive.
    pub(super) fn remote_drive(letter: char) -> Option<String> {
        let root: Vec<u16> = OsStr::new(&format!("{letter}:\\"))
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: `root` is a NUL-terminated UTF-16 string that outlives the
        // call, which only reads it.
        let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
        if kind != DRIVE_REMOTE {
            return None;
        }
        let local: Vec<u16> = OsStr::new(&format!("{letter}:"))
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut buffer = vec![0u16; 1024];
        let mut length = buffer.len() as u32;
        // SAFETY: `local` is NUL-terminated and outlives the call; `buffer`
        // holds `length` writable UTF-16 units, as promised to the API.
        let status =
            unsafe { WNetGetConnectionW(local.as_ptr(), buffer.as_mut_ptr(), &mut length) };
        if status == NO_ERROR {
            let end = buffer
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(buffer.len());
            let remote = String::from_utf16_lossy(&buffer[..end]);
            if !remote.trim().is_empty() {
                return Some(format!("{} ({letter}:)", remote.trim()));
            }
        }
        Some(format!("{letter}: (network drive)"))
    }
}

/// The one unsafe island in this crate, mirroring `intern-core`'s
/// `windows_file` module: raw Win32 registry reads to discover OneDrive
/// accounts and SharePoint/Teams library mounts.
#[cfg(windows)]
mod windows_registry {
    #![allow(unsafe_code)]

    use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::PathBuf};

    use windows_sys::Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ, RegCloseKey,
            RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW,
        },
    };

    use super::{CloudProviderKind, CloudRoot, push_unique};

    struct RegKey(HKEY);

    impl Drop for RegKey {
        fn drop(&mut self) {
            // SAFETY: the handle was opened by RegOpenKeyExW and is closed
            // exactly once here.
            unsafe { RegCloseKey(self.0) };
        }
    }

    impl RegKey {
        fn open(parent: HKEY, subkey: &str) -> Option<RegKey> {
            let wide = wide(subkey);
            let mut handle: HKEY = std::ptr::null_mut();
            // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives
            // the call, and `handle` receives the opened key on success.
            let status = unsafe { RegOpenKeyExW(parent, wide.as_ptr(), 0, KEY_READ, &mut handle) };
            (status == ERROR_SUCCESS).then_some(RegKey(handle))
        }

        fn open_sub(&self, subkey: &str) -> Option<RegKey> {
            RegKey::open(self.0, subkey)
        }

        fn subkey_names(&self) -> Vec<String> {
            let mut names = Vec::new();
            for index in 0.. {
                let mut buffer = [0u16; 512];
                let mut length = buffer.len() as u32;
                // SAFETY: `buffer`/`length` describe writable storage owned by
                // this frame; the remaining out-parameters are optional nulls.
                let status = unsafe {
                    RegEnumKeyExW(
                        self.0,
                        index,
                        buffer.as_mut_ptr(),
                        &mut length,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                if status != ERROR_SUCCESS {
                    break;
                }
                names.push(String::from_utf16_lossy(&buffer[..length as usize]));
            }
            names
        }

        fn value_names(&self) -> Vec<String> {
            let mut names = Vec::new();
            for index in 0.. {
                let mut buffer = [0u16; 1024];
                let mut length = buffer.len() as u32;
                // SAFETY: `buffer`/`length` describe writable storage owned by
                // this frame; type and data out-parameters are optional nulls.
                let status = unsafe {
                    RegEnumValueW(
                        self.0,
                        index,
                        buffer.as_mut_ptr(),
                        &mut length,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                };
                if status != ERROR_SUCCESS {
                    break;
                }
                names.push(String::from_utf16_lossy(&buffer[..length as usize]));
            }
            names
        }

        fn string_value(&self, name: &str) -> Option<String> {
            let wide_name = wide(name);
            let mut kind = 0u32;
            let mut size = 0u32;
            // SAFETY: size probe — a null data pointer with a valid size
            // pointer asks only for the required byte count.
            let status = unsafe {
                RegQueryValueExW(
                    self.0,
                    wide_name.as_ptr(),
                    std::ptr::null_mut(),
                    &mut kind,
                    std::ptr::null_mut(),
                    &mut size,
                )
            };
            if status != ERROR_SUCCESS || (kind != REG_SZ && kind != REG_EXPAND_SZ) || size == 0 {
                return None;
            }
            let mut buffer = vec![0u8; size as usize];
            let mut written = size;
            // SAFETY: `buffer` holds `written` writable bytes as promised to
            // the API; all pointers outlive the call.
            let status = unsafe {
                RegQueryValueExW(
                    self.0,
                    wide_name.as_ptr(),
                    std::ptr::null_mut(),
                    &mut kind,
                    buffer.as_mut_ptr(),
                    &mut written,
                )
            };
            if status != ERROR_SUCCESS {
                return None;
            }
            let units: Vec<u16> = buffer[..written as usize]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            let text = String::from_utf16_lossy(&units)
                .trim_end_matches('\0')
                .to_string();
            (!text.is_empty()).then_some(text)
        }

        fn dword_value(&self, name: &str) -> Option<u32> {
            let wide_name = wide(name);
            let mut kind = 0u32;
            let mut buffer = [0u8; 4];
            let mut written = buffer.len() as u32;
            // SAFETY: `buffer` holds four writable bytes; all pointers are
            // valid for the duration of the call.
            let status = unsafe {
                RegQueryValueExW(
                    self.0,
                    wide_name.as_ptr(),
                    std::ptr::null_mut(),
                    &mut kind,
                    buffer.as_mut_ptr(),
                    &mut written,
                )
            };
            (status == ERROR_SUCCESS && kind == REG_DWORD && written == 4)
                .then(|| u32::from_le_bytes(buffer))
        }
    }

    pub(super) fn append_registry_roots(roots: &mut Vec<CloudRoot>) {
        let Some(accounts) =
            RegKey::open(HKEY_CURRENT_USER, r"Software\Microsoft\OneDrive\Accounts")
        else {
            return;
        };
        for account_name in accounts.subkey_names() {
            let Some(account) = accounts.open_sub(&account_name) else {
                continue;
            };
            let business = account
                .dword_value("Business")
                .is_some_and(|value| value != 0)
                || account_name.to_ascii_lowercase().starts_with("business");
            if let Some(folder) = account.string_value("UserFolder") {
                let kind = if business {
                    CloudProviderKind::OneDriveBusiness
                } else {
                    CloudProviderKind::OneDrivePersonal
                };
                let display_name = match account.string_value("DisplayName") {
                    Some(name) => format!("OneDrive – {name}"),
                    None if business => "OneDrive – Work".to_string(),
                    None => "OneDrive – Personal".to_string(),
                };
                push_unique(
                    roots,
                    CloudRoot {
                        kind,
                        display_name,
                        root: PathBuf::from(folder),
                    },
                );
            }
            let Some(tenants) = account.open_sub("Tenants") else {
                continue;
            };
            for tenant_name in tenants.subkey_names() {
                let Some(tenant) = tenants.open_sub(&tenant_name) else {
                    continue;
                };
                for mount in tenant.value_names() {
                    if mount.is_empty() {
                        continue;
                    }
                    push_unique(
                        roots,
                        CloudRoot {
                            kind: CloudProviderKind::SharePoint,
                            display_name: tenant_name.clone(),
                            root: PathBuf::from(&mount),
                        },
                    );
                }
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(Some(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_fold_the_verbatim_prefix_case_and_separators() {
        assert_eq!(
            path_components(Path::new(r"\\?\C:\Users\Pat\OneDrive - Contoso")),
            path_components(Path::new(r"c:/users/pat/onedrive - contoso/"))
        );
        assert_eq!(
            path_components(Path::new(r"\\?\UNC\Server\Share\Legal")),
            path_components(Path::new(r"\\server\share\legal"))
        );
        assert_ne!(
            path_components(Path::new(r"\\server\share")),
            path_components(Path::new(r"server\share")),
            "a UNC path is not the relative path with the same words"
        );
        assert_ne!(
            path_components(Path::new("/home/pat")),
            path_components(Path::new("home/pat"))
        );
    }

    #[test]
    fn unc_shares_are_read_from_plain_and_verbatim_spellings() {
        assert_eq!(
            unc_share(r"\\fileserver\legal\intake\scan.pdf").as_deref(),
            Some(r"\\fileserver\legal")
        );
        assert_eq!(
            unc_share(r"\\?\UNC\fileserver\legal\intake").as_deref(),
            Some(r"\\fileserver\legal")
        );
        assert_eq!(
            unc_share(r"\\fileserver"),
            None,
            "a server alone is not a share"
        );
        assert_eq!(unc_share(r"\\?\C:\Users\pat"), None);
        assert_eq!(unc_share(r"C:\Users\pat"), None);
        assert_eq!(unc_share("/home/pat"), None);
    }

    #[test]
    fn relative_paths_keep_their_own_casing_below_the_root() {
        assert_eq!(
            relative_to_root(
                Path::new(r"\\?\C:\Users\Pat\Contoso\Legal - Documents\Contracts\2026\SOW.pdf"),
                Path::new(r"c:\users\pat\contoso\Legal - Documents"),
            )
            .as_deref(),
            Some("Contracts/2026/SOW.pdf")
        );
        assert_eq!(
            relative_to_root(
                Path::new("/home/pat/OneDrive/a.pdf"),
                Path::new("/home/pat/OneDrive")
            )
            .as_deref(),
            Some("a.pdf")
        );
        assert_eq!(
            relative_to_root(
                Path::new("/home/pat/OneDrive"),
                Path::new("/home/pat/OneDrive")
            ),
            None,
            "the root itself has no relative path"
        );
        assert_eq!(
            relative_to_root(
                Path::new("/home/pat/OneDriveBackup/a.pdf"),
                Path::new("/home/pat/OneDrive")
            ),
            None
        );
    }
}
