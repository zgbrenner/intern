//! Offline release gate for the Tauri updater payload.
//!
//! This workspace tool is deliberately outside `intern-app`: it reads the
//! committed updater public key and verifies release artifacts without becoming
//! a discoverable or bundled Tauri application binary.

use std::{collections::BTreeMap, env, fs, path::PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;

// Tauri CLI 2.11.4 writes `.sig` as STANDARD.encode(SignatureBox::to_string()).
// The updater manifest carries that encoded file content verbatim; it is not a
// raw Minisign text file.
const MAX_TAURI_SIGNATURE_BYTES: usize = 4096;

#[derive(Deserialize)]
struct TauriConfig {
    plugins: Plugins,
}
#[derive(Deserialize)]
struct Plugins {
    updater: UpdaterConfig,
}
#[derive(Deserialize)]
struct UpdaterConfig {
    pubkey: String,
}
#[derive(Deserialize)]
struct Latest {
    version: String,
    platforms: BTreeMap<String, Platform>,
}
#[derive(Deserialize)]
struct Platform {
    signature: String,
    url: String,
}

struct Input {
    installer: PathBuf,
    signature: PathBuf,
    latest_json: PathBuf,
    tauri_config: PathBuf,
    tag: String,
    repository: String,
}

fn fail(message: impl Into<String>) -> Result<(), String> {
    Err(message.into())
}

fn parse_args() -> Result<Input, String> {
    let mut values = BTreeMap::new();
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        if !flag.starts_with("--") {
            return Err(format!("unexpected argument: {flag}"));
        }
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        if values.insert(flag, value).is_some() {
            return Err("duplicate updater verifier argument".to_owned());
        }
    }
    let take = |name: &str| -> Result<String, String> {
        values
            .get(name)
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or_else(|| format!("{name} is required"))
    };
    let allowed = [
        "--installer",
        "--signature",
        "--latest-json",
        "--tauri-config",
        "--tag",
        "--repository",
    ];
    if values.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unknown updater verifier argument".to_owned());
    }
    Ok(Input {
        installer: take("--installer")?.into(),
        signature: take("--signature")?.into(),
        latest_json: take("--latest-json")?.into(),
        tauri_config: take("--tauri-config")?.into(),
        tag: take("--tag")?,
        repository: take("--repository")?,
    })
}

fn parse_tauri_updater_signature(signature_text: &str) -> Result<Signature, String> {
    if signature_text.is_empty()
        || signature_text.len() > MAX_TAURI_SIGNATURE_BYTES
        || signature_text
            .as_bytes()
            .iter()
            .any(u8::is_ascii_whitespace)
    {
        return Err("Tauri updater signature must be one bounded standard Base64 value".to_owned());
    }
    let signature_box = STANDARD
        .decode(signature_text)
        .map_err(|_| "malformed Tauri updater signature Base64 wrapper".to_owned())?;
    let signature_box = std::str::from_utf8(&signature_box)
        .map_err(|_| "Tauri updater signature wrapper is not UTF-8 Minisign text".to_owned())?;
    if signature_box.trim() != signature_box {
        return Err(
            "Tauri updater signature wrapper must contain an exact Minisign SignatureBox"
                .to_owned(),
        );
    }
    Signature::decode(signature_box).map_err(|_| {
        "Tauri updater signature wrapper does not contain a valid Minisign SignatureBox".to_owned()
    })
}

fn verify(input: &Input) -> Result<(), String> {
    let tag_version = input
        .tag
        .strip_prefix('v')
        .filter(|version| !version.is_empty())
        .ok_or_else(|| "tag must begin with v followed by a version".to_owned())?;
    let installer_name = input
        .installer
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.contains(['/', '\\']))
        .ok_or_else(|| "installer must have a safe leaf filename".to_owned())?;
    let config: TauriConfig = serde_json::from_slice(
        &fs::read(&input.tauri_config)
            .map_err(|error| format!("cannot read Tauri config: {error}"))?,
    )
    .map_err(|error| format!("malformed Tauri config: {error}"))?;
    let key_text = String::from_utf8(
        STANDARD
            .decode(config.plugins.updater.pubkey)
            .map_err(|error| format!("malformed Tauri updater public key: {error}"))?,
    )
    .map_err(|error| format!("Tauri updater public key is not UTF-8: {error}"))?;
    let public_key = PublicKey::decode(&key_text)
        .map_err(|error| format!("malformed Tauri updater public key: {error}"))?;
    let signature_text = fs::read_to_string(&input.signature)
        .map_err(|error| format!("cannot read updater signature: {error}"))?;
    let signature = parse_tauri_updater_signature(&signature_text)?;
    let installer =
        fs::read(&input.installer).map_err(|error| format!("cannot read installer: {error}"))?;
    public_key
        .verify(&installer, &signature, false)
        .map_err(|error| format!("updater signature does not verify: {error}"))?;
    let latest: Latest = serde_json::from_slice(
        &fs::read(&input.latest_json)
            .map_err(|error| format!("cannot read latest.json: {error}"))?,
    )
    .map_err(|error| format!("malformed latest.json: {error}"))?;
    if latest.version != tag_version {
        return fail("latest.json version does not match the release tag");
    }
    let platform = latest
        .platforms
        .get("windows-x86_64")
        .ok_or_else(|| "latest.json omits windows-x86_64".to_owned())?;
    if platform.signature != signature_text {
        return fail("latest.json signature does not match the detached signature");
    }
    let expected_url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        input.repository, input.tag, installer_name
    );
    if platform.url != expected_url {
        return fail("latest.json URL does not name this repository, tag, and installer");
    }
    Ok(())
}

fn main() {
    if let Err(error) = parse_args().and_then(|input| verify(&input)) {
        eprintln!("updater verification failed: {error}");
        std::process::exit(1);
    }
    println!("updater installer, signature, and latest.json verified offline");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const PUBKEY: &str = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";

    fn fixture() -> (tempfile::TempDir, Input) {
        let directory = tempdir().unwrap();
        let installer = directory.path().join("Intern_0.1.0-alpha.3_x64-setup.exe");
        let signature = directory.path().join("installer.sig");
        let latest_json = directory.path().join("latest.json");
        let tauri_config = directory.path().join("tauri.conf.json");
        fs::write(&installer, b"test").unwrap();
        let tauri_signature = STANDARD.encode(SIGNATURE);
        fs::write(&signature, &tauri_signature).unwrap();
        fs::write(
            &tauri_config,
            format!(
                r#"{{"plugins":{{"updater":{{"pubkey":"{}"}}}}}}"#,
                STANDARD.encode(PUBKEY)
            ),
        )
        .unwrap();
        fs::write(&latest_json, format!(r#"{{"version":"0.1.0-alpha.3","platforms":{{"windows-x86_64":{{"signature":{},"url":"https://github.com/zgbrenner/intern/releases/download/v0.1.0-alpha.3/Intern_0.1.0-alpha.3_x64-setup.exe"}}}}}}"#, serde_json::to_string(&tauri_signature).unwrap())).unwrap();
        (
            directory,
            Input {
                installer,
                signature,
                latest_json,
                tauri_config,
                tag: "v0.1.0-alpha.3".to_owned(),
                repository: "zgbrenner/intern".to_owned(),
            },
        )
    }

    #[test]
    fn accepts_the_tauri_base64_wrapped_signed_installer_and_manifest() {
        let (_directory, input) = fixture();
        verify(&input).unwrap();
    }

    #[test]
    fn rejects_tampering_and_every_manifest_binding() {
        let (_directory, input) = fixture();
        let mut wrong_key = STANDARD.decode(PUBKEY.lines().nth(1).unwrap()).unwrap();
        wrong_key[10] ^= 1;
        let changed_key = format!(
            "{}\n{}",
            PUBKEY.lines().next().unwrap(),
            STANDARD.encode(wrong_key)
        );
        fs::write(
            &input.tauri_config,
            format!(
                r#"{{"plugins":{{"updater":{{"pubkey":"{}"}}}}}}"#,
                STANDARD.encode(changed_key)
            ),
        )
        .unwrap();
        assert!(verify(&input).unwrap_err().contains("does not verify"));
        let (_directory, input) = fixture();
        fs::write(&input.installer, b"Test").unwrap();
        assert!(verify(&input).unwrap_err().contains("does not verify"));
        let (_directory, input) = fixture();
        fs::write(&input.signature, "not a signature").unwrap();
        assert!(
            verify(&input)
                .unwrap_err()
                .contains("must be one bounded standard Base64 value")
        );
        let (_directory, input) = fixture();
        fs::write(
            &input.signature,
            STANDARD.encode(SIGNATURE.replacen("y/rU", "z/rU", 1)),
        )
        .unwrap();
        assert!(verify(&input).unwrap_err().contains("does not verify"));
        let (_directory, input) = fixture();
        fs::write(
            &input.latest_json,
            r#"{"version":"0.1.0-alpha.2","platforms":{}}"#,
        )
        .unwrap();
        assert!(
            verify(&input)
                .unwrap_err()
                .contains("version does not match")
        );
        let (_directory, input) = fixture();
        let latest = fs::read_to_string(&input.latest_json).unwrap().replace(
            "https://github.com/zgbrenner/intern",
            "https://github.com/other/intern",
        );
        fs::write(&input.latest_json, latest).unwrap();
        assert!(verify(&input).unwrap_err().contains("URL does not name"));
        let (_directory, input) = fixture();
        let tauri_signature = STANDARD.encode(SIGNATURE);
        let latest = fs::read_to_string(&input.latest_json)
            .unwrap()
            .replace(&tauri_signature, &format!("x{tauri_signature}"));
        fs::write(&input.latest_json, latest).unwrap();
        assert!(
            verify(&input)
                .unwrap_err()
                .contains("signature does not match")
        );
    }

    #[test]
    fn rejects_raw_double_wrapped_and_oversized_signature_representations() {
        let (_directory, input) = fixture();
        fs::write(&input.signature, SIGNATURE).unwrap();
        assert!(
            verify(&input)
                .unwrap_err()
                .contains("must be one bounded standard Base64 value")
        );

        let (_directory, input) = fixture();
        fs::write(
            &input.signature,
            STANDARD.encode(STANDARD.encode(SIGNATURE)),
        )
        .unwrap();
        assert!(
            verify(&input)
                .unwrap_err()
                .contains("does not contain a valid Minisign SignatureBox")
        );

        let (_directory, input) = fixture();
        fs::write(&input.signature, STANDARD.encode(format!("\n{SIGNATURE}"))).unwrap();
        assert!(
            verify(&input)
                .unwrap_err()
                .contains("must contain an exact Minisign SignatureBox")
        );

        let (_directory, input) = fixture();
        fs::write(&input.signature, "A".repeat(MAX_TAURI_SIGNATURE_BYTES + 1)).unwrap();
        assert!(
            verify(&input)
                .unwrap_err()
                .contains("must be one bounded standard Base64 value")
        );
    }
}
