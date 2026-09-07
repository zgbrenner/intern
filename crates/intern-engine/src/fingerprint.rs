//! Text fingerprints: does this document say what one already filed says?
//!
//! A content hash catches the same bytes twice. It does not catch the same
//! document scanned twice, exported twice, or saved once more by a program
//! that rewrote its metadata - and those are the duplicates people actually
//! make. A simhash over the extracted text does: similar text gives similar
//! bits, so two fingerprints a few bits apart are one document and two a
//! dozen or more apart are two.
//!
//! The fingerprint is the same on every machine and in every build (its
//! hash is FNV-1a, spelled out below rather than borrowed from the standard
//! library, whose hasher promises nothing across versions), because the
//! shared filed index carries it from one teammate's machine to another's.

use crate::domain::DocumentSource;

/// Fingerprints this many bits apart or fewer are one document. Two texts
/// that have nothing to do with each other sit about thirty-two bits apart,
/// and eight or fewer happens by chance less than once in a billion pairs;
/// a second scan of one page with a handful of misread characters lands
/// well inside this.
pub const NEAR_DUPLICATE_DISTANCE: u32 = 6;
/// Fewer characters than this, once normalised, and a fingerprint would say
/// little: a note of a dozen words looks like every other note of a dozen
/// words.
pub const MIN_CHARACTERS: usize = 200;
/// Character shingles rather than word shingles: an OCR engine misreading
/// one letter costs five features out of hundreds, not three word triples
/// out of dozens, so a re-scan stays close and the threshold can stay tight.
const SHINGLE: usize = 5;

/// The fingerprint of a text, or `None` when the text is too short to have
/// one worth comparing.
pub fn text_fingerprint(text: &str) -> Option<u64> {
    let normalised = normalise(text);
    if normalised.len() < MIN_CHARACTERS {
        return None;
    }
    let mut weights = [0i32; 64];
    for shingle in normalised.windows(SHINGLE) {
        let feature = fnv1a(shingle);
        for (bit, weight) in weights.iter_mut().enumerate() {
            if (feature >> bit) & 1 == 1 {
                *weight += 1;
            } else {
                *weight -= 1;
            }
        }
    }
    Some(
        weights
            .iter()
            .enumerate()
            .filter(|(_, weight)| **weight > 0)
            .fold(0u64, |bits, (bit, _)| bits | (1 << bit)),
    )
}

/// Lowercase letters and digits with single spaces between words: layout,
/// punctuation, and case are not what makes two documents different.
fn normalise(text: &str) -> Vec<char> {
    let mut output = Vec::with_capacity(text.len());
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_alphanumeric() {
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            pending_space = false;
            output.extend(character.to_lowercase());
        } else {
            pending_space = true;
        }
    }
    output
}

/// The fingerprint of everything the extractor read from a document.
pub fn source_fingerprint(source: &DocumentSource) -> Option<u64> {
    let text = source
        .pages
        .iter()
        .map(|page| page.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    text_fingerprint(&text)
}

/// How many bits two fingerprints differ in.
pub fn hamming(left: u64, right: u64) -> u32 {
    (left ^ right).count_ones()
}

pub fn is_near_duplicate(left: u64, right: u64) -> bool {
    hamming(left, right) <= NEAR_DUPLICATE_DISTANCE
}

/// The stored form: sixteen lowercase hex digits.
pub fn encode(fingerprint: u64) -> String {
    format!("{fingerprint:016x}")
}

pub fn decode(value: &str) -> Option<u64> {
    (value.len() == 16)
        .then(|| u64::from_str_radix(value, 16).ok())
        .flatten()
}

fn fnv1a(characters: &[char]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for character in characters {
        for byte in (*character as u32).to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGREEMENT: &str = "CONSULTING AGREEMENT\n\nThis Consulting Agreement is entered into and \
        effective as of April 1, 2026 by and between Ridgeline Cartography LLC, a Colorado limited \
        liability company, and Vistage Worldwide, Inc., a Delaware corporation. The Consultant will \
        deliver the 2026 member-map engagement described in Exhibit A, including quarterly territory \
        reviews, data refreshes for each chapter, and a final report due no later than March 31, 2027. \
        Fees are payable within thirty days of each invoice. Either party may terminate this Agreement \
        on sixty days written notice. Signed on March 28, 2026 by authorized representatives.";

    #[test]
    fn the_same_text_has_the_same_fingerprint_and_a_short_text_has_none() {
        assert_eq!(text_fingerprint(AGREEMENT), text_fingerprint(AGREEMENT));
        assert_eq!(text_fingerprint("Paid in full. Thanks, Dana."), None);
        let fingerprint = text_fingerprint(AGREEMENT).unwrap();
        assert_eq!(decode(&encode(fingerprint)), Some(fingerprint));
        assert_eq!(decode("not a fingerprint"), None);
    }

    /// A second scan of the same page: Tesseract misreads a few words, the
    /// layout adds a line break, the case changes. Still the one document.
    #[test]
    fn a_rescan_with_a_few_misread_words_is_a_near_duplicate() {
        let rescan = AGREEMENT
            .replace("EFFECTIVE", "EFFECTIWE")
            .replace("effective as of", "effectiwe as of")
            .replace("Cartography", "Cartograpny")
            .replace("thirty days", "thirty  days")
            .replace("Signed on", "signed on");
        let original = text_fingerprint(AGREEMENT).unwrap();
        let again = text_fingerprint(&rescan).unwrap();
        assert!(
            is_near_duplicate(original, again),
            "distance {}",
            hamming(original, again)
        );
    }

    #[test]
    fn a_different_document_is_far_away() {
        let invoice = "INVOICE INV-2048\nInvoice date: April 30, 2025\nDue date: May 30, 2025\n\
            Nimbus Orchard Supply Co.\nBill to Atlas Threadworks LLC\nItem: orchard ladders, twelve \
            units at forty dollars each. Item: pruning shears, thirty units at twelve dollars each. \
            Item: delivery to the Fictional Harbor depot. Subtotal, tax at eight percent, total due \
            $1,248.00. Please remit by cheque or transfer quoting the invoice number.";
        let original = text_fingerprint(AGREEMENT).unwrap();
        let other = text_fingerprint(invoice).unwrap();
        assert!(
            hamming(original, other) > 3 * NEAR_DUPLICATE_DISTANCE,
            "distance {}",
            hamming(original, other)
        );
    }

    /// Pinned so that a fingerprint written by one build reads the same in
    /// the next: the shared index stores these across machines.
    #[test]
    fn the_fingerprint_is_stable_across_builds() {
        assert_eq!(
            encode(text_fingerprint(AGREEMENT).unwrap()),
            "16a8c1f81f18a84f"
        );
    }
}
