use intern_app::model::manifest::{ModelManifest, embedded_manifest_json};

fn reject_mutation(from: &str, to: &str) {
    let changed = embedded_manifest_json().replacen(from, to, 1);
    assert!(ModelManifest::parse(&changed).is_err());
}

#[test]
fn embedded_manifest_is_the_pinned_qwen_q4_pair() {
    let manifest = ModelManifest::embedded().expect("embedded manifest must validate");
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.model_id, "qwen2.5-vl-3b-instruct-q4-k-m");
    assert_eq!(manifest.files.len(), 2);
    assert_eq!(manifest.files[0].size, 1_929_901_056);
    assert_eq!(manifest.files[1].size, 1_338_428_128);
}

#[test]
fn changed_size_is_rejected() {
    reject_mutation("1929901056", "1929901055");
}

#[test]
fn changed_digest_is_rejected() {
    reject_mutation(
        "d02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12",
        "a02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12",
    );
}

#[test]
fn unsafe_filename_is_rejected() {
    reject_mutation(
        "Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf",
        "../Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf",
    );
}

#[test]
fn non_https_url_is_rejected() {
    reject_mutation("https://huggingface.co", "http://huggingface.co");
}

#[test]
fn duplicate_filename_is_rejected() {
    reject_mutation(
        "mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf",
        "Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf",
    );
}
