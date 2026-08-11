use std::collections::HashSet;

use crate::{ComposedName, ValidatedProposal};

const MAX_FILENAME_CHARS: usize = 140;

pub fn compose_filename(
    proposal: &ValidatedProposal,
    extension: &str,
    existing_names: &[&str],
) -> ComposedName {
    let extension = sanitize_extension(extension);
    let mut segments = Vec::new();
    if let Some(value) = proposal.document_date.as_deref().and_then(sanitize_segment) {
        segments.push(value);
    }
    if let Some(value) = proposal.document_type.as_deref().and_then(sanitize_segment) {
        segments.push(value);
    }
    if let Some(subject) = proposal.filename_subject.as_deref() {
        let without_extension = strip_duplicate_extension(subject, &extension);
        if let Some(value) = sanitize_segment(without_extension) {
            segments.push(value);
        }
    }
    let base = if segments.is_empty() {
        "Document".to_owned()
    } else {
        segments.join(" - ")
    };
    let existing = existing_names
        .iter()
        .map(|value| windows_name_key(value))
        .collect::<HashSet<_>>();

    let mut collision_index = 1;
    loop {
        let suffix = if collision_index == 1 {
            String::new()
        } else {
            format!(" ({collision_index})")
        };
        let value = fit_filename(&base, &suffix, &extension);
        if !existing.contains(&windows_name_key(&value)) {
            return ComposedName {
                value,
                collision_index,
            };
        }
        collision_index += 1;
    }
}

fn sanitize_extension(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .take(MAX_FILENAME_CHARS.saturating_sub(2))
        .collect()
}

fn strip_duplicate_extension<'a>(value: &'a str, extension: &str) -> &'a str {
    if extension.is_empty() {
        return value;
    }
    let mut stripped = value;
    let suffix_length = extension.len() + 1;
    loop {
        let Some(suffix_start) = stripped.len().checked_sub(suffix_length) else {
            break;
        };
        let Some(suffix) = stripped.get(suffix_start..) else {
            break;
        };
        if suffix.starts_with('.') && suffix[1..].eq_ignore_ascii_case(extension) {
            let Some(prefix) = stripped.get(..suffix_start) else {
                break;
            };
            stripped = prefix;
        } else {
            break;
        }
    }
    stripped
}

fn sanitize_segment(value: &str) -> Option<String> {
    let mut output = String::new();
    let mut pending_space = false;
    for character in value.chars() {
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if character.is_control()
            || is_bidi_control(character)
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        {
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(character);
    }
    while output.ends_with(' ') || output.ends_with('.') {
        output.pop();
    }
    if output.is_empty() {
        return None;
    }
    if is_reserved_device_name(&output) {
        output.insert(0, '_');
    }
    Some(output)
}

fn is_bidi_control(character: char) -> bool {
    matches!(character as u32, 0x061c | 0x200e..=0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
}

fn is_reserved_device_name(value: &str) -> bool {
    let stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.strip_prefix("COM").is_some_and(is_reserved_number)
        || stem.strip_prefix("LPT").is_some_and(is_reserved_number)
}

fn is_reserved_number(value: &str) -> bool {
    value.len() == 1 && matches!(value.as_bytes()[0], b'1'..=b'9')
}

fn fit_filename(base: &str, suffix: &str, extension: &str) -> String {
    let maximum_extension = MAX_FILENAME_CHARS.saturating_sub(suffix.chars().count() + 2);
    let extension = extension
        .chars()
        .take(maximum_extension)
        .collect::<String>();
    let extension_part = if extension.is_empty() {
        String::new()
    } else {
        format!(".{extension}")
    };
    let reserved = suffix.chars().count() + extension_part.chars().count();
    let available = MAX_FILENAME_CHARS.saturating_sub(reserved);
    let mut truncated = base.chars().take(available).collect::<String>();
    while truncated.ends_with(' ') || truncated.ends_with('.') {
        truncated.pop();
    }
    format!("{truncated}{suffix}{extension_part}")
}

fn windows_name_key(value: &str) -> String {
    value
        .trim_end_matches(|character| matches!(character, ' ' | '.'))
        .to_lowercase()
}
