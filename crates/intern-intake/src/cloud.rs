//! Detection of OneDrive/SharePoint sync roots on the local machine.
//!
//! Intern never talks to Microsoft Graph. A "cloud" folder is simply a local
//! path that the Microsoft sync client replicates, so detection is a matter of
//! reading the sync client's own breadcrumbs: environment variables and the
//! per-user registry on Windows, well-known home-directory names elsewhere.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloudProviderKind {
    OneDrivePersonal,
    OneDriveBusiness,
    SharePoint,
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

/// Longest-prefix match, component-wise and case-insensitive, so a SharePoint
/// library mounted inside a OneDrive folder wins over its parent.
pub fn classify(path: &Path, roots: &[CloudRoot]) -> Option<CloudLocation> {
    let components = normalized_components(path);
    let mut best: Option<(usize, &CloudRoot)> = None;
    for root in roots {
        let root_components = normalized_components(&root.root);
        if root_components.is_empty() || root_components.len() > components.len() {
            continue;
        }
        if components[..root_components.len()] == root_components[..]
            && best.is_none_or(|(length, _)| root_components.len() > length)
        {
            best = Some((root_components.len(), root));
        }
    }
    best.map(|(_, root)| CloudLocation {
        kind: root.kind,
        display_name: root.display_name.clone(),
    })
}

fn normalized_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_lowercase()),
            Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().to_lowercase()),
            Component::RootDir => Some(String::new()),
            Component::CurDir | Component::ParentDir => None,
        })
        .collect()
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
    let key = normalized_key(&candidate.root);
    if key.is_empty() {
        return;
    }
    if roots
        .iter()
        .all(|existing| normalized_key(&existing.root) != key)
    {
        roots.push(candidate);
    }
}

fn normalized_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
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
