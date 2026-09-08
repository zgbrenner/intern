// On Windows the home-scan tests are cfg'd out, which would leave the fake
// probe unused; keep the file warning-free on every platform.
#![allow(dead_code)]

mod common;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[cfg(not(windows))]
use intern_intake::detect_cloud_roots_with;
use intern_intake::{
    CloudProviderKind, CloudRoot, EnvProbe, classify, network_share, relative_to_root, unc_share,
};

#[derive(Default)]
struct FakeEnv {
    vars: HashMap<String, String>,
    directories: HashMap<PathBuf, Vec<PathBuf>>,
}

impl FakeEnv {
    fn with_home(home: &str) -> Self {
        let mut env = Self::default();
        env.vars.insert("HOME".to_string(), home.to_string());
        env
    }

    fn add_dir(&mut self, parent: &str, name: &str) {
        let parent = PathBuf::from(parent);
        let child = parent.join(name);
        self.directories.entry(parent).or_default().push(child);
    }
}

impl EnvProbe for FakeEnv {
    fn var(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned()
    }

    fn subdirectories(&self, path: &Path) -> Vec<PathBuf> {
        self.directories.get(path).cloned().unwrap_or_default()
    }
}

fn root(kind: CloudProviderKind, display_name: &str, path: &str) -> CloudRoot {
    CloudRoot {
        kind,
        display_name: display_name.to_string(),
        root: PathBuf::from(path),
    }
}

#[cfg(not(windows))]
#[test]
fn home_onedrive_folders_map_to_personal_and_business_kinds() {
    let mut env = FakeEnv::with_home("/home/pat");
    env.add_dir("/home/pat", "OneDrive");
    env.add_dir("/home/pat", "OneDrive - Contoso");
    env.add_dir("/home/pat", "Documents");
    env.add_dir("/home/pat/Library/CloudStorage", "OneDrive-Personal");
    env.add_dir("/home/pat/Library/CloudStorage", "OneDrive-Fabrikam");

    let roots = detect_cloud_roots_with(&env);
    let find = |path: &str| {
        roots
            .iter()
            .find(|candidate| candidate.root == Path::new(path))
            .unwrap_or_else(|| panic!("missing root {path}; got {roots:?}"))
    };
    let personal = find("/home/pat/OneDrive");
    assert_eq!(personal.kind, CloudProviderKind::OneDrivePersonal);
    assert_eq!(personal.display_name, "OneDrive – Personal");

    let business = find("/home/pat/OneDrive - Contoso");
    assert_eq!(business.kind, CloudProviderKind::OneDriveBusiness);
    assert_eq!(business.display_name, "OneDrive – Contoso");

    let mac_personal = find("/home/pat/Library/CloudStorage/OneDrive-Personal");
    assert_eq!(mac_personal.kind, CloudProviderKind::OneDrivePersonal);

    let mac_business = find("/home/pat/Library/CloudStorage/OneDrive-Fabrikam");
    assert_eq!(mac_business.kind, CloudProviderKind::OneDriveBusiness);
    assert_eq!(mac_business.display_name, "OneDrive – Fabrikam");

    assert_eq!(
        roots.len(),
        4,
        "the plain Documents folder must not be detected"
    );
}

#[cfg(not(windows))]
#[test]
fn duplicate_detections_of_the_same_path_are_reported_once() {
    let mut env = FakeEnv::with_home("/home/pat");
    env.add_dir("/home/pat", "OneDrive");
    env.add_dir("/home/pat", "OneDrive");
    let roots = detect_cloud_roots_with(&env);
    assert_eq!(roots.len(), 1);
}

#[cfg(not(windows))]
#[test]
fn a_missing_home_yields_no_roots_rather_than_an_error() {
    let roots = detect_cloud_roots_with(&FakeEnv::default());
    assert!(roots.is_empty());
}

#[test]
fn classify_prefers_the_longest_matching_root() {
    let roots = vec![
        root(
            CloudProviderKind::OneDriveBusiness,
            "OneDrive – Contoso",
            "/home/pat/OneDrive - Contoso",
        ),
        root(
            CloudProviderKind::SharePoint,
            "Contoso",
            "/home/pat/OneDrive - Contoso/Shared Library",
        ),
    ];
    let deep = classify(
        Path::new("/home/pat/OneDrive - Contoso/Shared Library/legal/contract.pdf"),
        &roots,
    )
    .unwrap();
    assert_eq!(deep.kind, CloudProviderKind::SharePoint);
    assert_eq!(deep.display_name, "Contoso");

    let shallow = classify(Path::new("/home/pat/OneDrive - Contoso/inbox"), &roots).unwrap();
    assert_eq!(shallow.kind, CloudProviderKind::OneDriveBusiness);
}

#[test]
fn classify_matches_case_insensitively_and_only_on_component_boundaries() {
    let roots = vec![root(
        CloudProviderKind::OneDrivePersonal,
        "OneDrive – Personal",
        "/home/pat/OneDrive",
    )];
    assert!(classify(Path::new("/HOME/Pat/onedrive/taxes.pdf"), &roots).is_some());
    assert!(classify(Path::new("/home/pat/OneDrive"), &roots).is_some());
    assert!(
        classify(Path::new("/home/pat/OneDriveBackup/taxes.pdf"), &roots).is_none(),
        "a sibling folder sharing the prefix string is not inside the root"
    );
    assert!(classify(Path::new("/somewhere/else.pdf"), &roots).is_none());
}

/// `canonicalize` on Windows returns `\\?\C:\...` while the registry records
/// the same root as `C:\...`; the badge and every library-relative path
/// depend on those two spellings agreeing.
#[test]
fn classify_folds_the_verbatim_prefix_drive_case_and_separators() {
    let roots = vec![
        root(
            CloudProviderKind::OneDriveBusiness,
            "OneDrive – Contoso",
            r"C:\Users\Pat\OneDrive - Contoso",
        ),
        root(
            CloudProviderKind::SharePoint,
            "Contoso",
            r"C:\Users\Pat\Contoso\Legal - Documents",
        ),
    ];
    let verbatim = classify(
        Path::new(r"\\?\c:\users\pat\onedrive - contoso\Scans"),
        &roots,
    )
    .unwrap();
    assert_eq!(verbatim.kind, CloudProviderKind::OneDriveBusiness);
    let slashed = classify(
        Path::new("C:/Users/Pat/Contoso/Legal - Documents/Contracts"),
        &roots,
    )
    .unwrap();
    assert_eq!(slashed.kind, CloudProviderKind::SharePoint);
    assert_eq!(slashed.display_name, "Contoso");
}

#[test]
fn a_unc_path_is_a_network_share_named_after_its_share() {
    let roots = vec![root(
        CloudProviderKind::OneDriveBusiness,
        "OneDrive – Contoso",
        r"C:\Users\Pat\OneDrive - Contoso",
    )];
    for path in [
        r"\\fileserver\legal\intake",
        r"\\?\UNC\fileserver\legal\intake\2026",
        r"\\FILESERVER\Legal",
    ] {
        let location = classify(Path::new(path), &roots)
            .unwrap_or_else(|| panic!("{path} should be a network share"));
        assert_eq!(location.kind, CloudProviderKind::NetworkShare);
        assert!(
            location
                .display_name
                .eq_ignore_ascii_case(r"\\fileserver\legal"),
            "{}",
            location.display_name
        );
    }
    assert_eq!(network_share(Path::new(r"\\fileserver")), None);
    assert_eq!(network_share(Path::new("/srv/legal/intake")), None);
    assert_eq!(unc_share(r"\\?\C:\Users\pat\Documents"), None);
    assert_eq!(CloudProviderKind::NetworkShare.as_str(), "network_share");
}

#[test]
fn a_sync_root_outranks_the_network_share_check() {
    // Nobody syncs OneDrive to a UNC path, but the precedence is what makes
    // the badge say the more specific thing when both could apply.
    let roots = vec![root(
        CloudProviderKind::SharePoint,
        "Contoso",
        r"\\fileserver\legal\Contoso - Documents",
    )];
    let location = classify(
        Path::new(r"\\fileserver\legal\Contoso - Documents\Contracts"),
        &roots,
    )
    .unwrap();
    assert_eq!(location.kind, CloudProviderKind::SharePoint);
    assert_eq!(
        relative_to_root(
            Path::new(r"\\?\UNC\fileserver\legal\Contoso - Documents\Contracts\a.pdf"),
            Path::new(r"\\fileserver\legal\Contoso - Documents"),
        )
        .as_deref(),
        Some("Contracts/a.pdf")
    );
}
