//! What the OCR pass decides between readings of one page. The decisions are
//! here rather than beside the Tesseract adapter because they are the part
//! that has to hold on the platform this ships on, where no Tesseract fixture
//! runs.

use intern_worker::extract::{
    CONFIDENT_READING, OcrResult, better_reading, orientation_search_is_worthwhile,
};

/// Measured on the corpus: a page read in the orientation OSD asked for
/// scored 44 while the same page read as-is scored 95, with both readings
/// returning eleven words. Volume cannot choose between them; confidence
/// can.
#[test]
fn a_confidently_misdetected_rotation_loses_to_the_page_as_it_was() {
    let oriented = OcrResult::new("O71 TIVL3Y MOGVAW ZLYVNO", 44.1).with_rotation(180);
    let unrotated = OcrResult::new("PACKING SLIP PS-311 DATE JULY 15 2025", 95.2);

    let chosen = better_reading(oriented, unrotated);

    assert_eq!(chosen.text, "PACKING SLIP PS-311 DATE JULY 15 2025");
    assert_eq!(chosen.rotation_degrees, 0);
}

#[test]
fn a_genuinely_rotated_page_keeps_the_rotation_that_read_it() {
    let oriented = OcrResult::new("DELIVERY RECEIPT DR-771", 92.0).with_rotation(270);
    let unrotated = OcrResult::new("gibberish", 31.0);

    let chosen = better_reading(oriented, unrotated);

    assert_eq!(chosen.text, "DELIVERY RECEIPT DR-771");
    assert_eq!(chosen.rotation_degrees, 270);
}

/// A tie keeps the detected orientation rather than silently preferring the
/// unrotated read, so behaviour on a blank page stays predictable.
#[test]
fn an_equal_score_keeps_the_detected_orientation() {
    let oriented = OcrResult::new("", 0.0).with_rotation(90);
    let unrotated = OcrResult::new("", 0.0);

    assert_eq!(better_reading(oriented, unrotated).rotation_degrees, 90);
}

/// Mean word confidence says nothing about how much was read. A rotated page
/// that yields three confident tokens beat a page of three hundred words read
/// just under the confidence bar, and the document came back as three tokens.
#[test]
fn a_sparse_high_confidence_reading_does_not_displace_a_dense_one() {
    let dense = OcrResult::new(
        "Settlement Agreement and Mutual Release ".repeat(50).trim(),
        74.9,
    );
    let sparse = OcrResult::new("INVOICE 4 2", 80.0).with_rotation(90);

    let chosen = better_reading(dense.clone(), sparse);

    assert_eq!(chosen.text, dense.text);
    assert_eq!(chosen.rotation_degrees, 0);
}

/// Density is a floor, not a preference: a fuller reading that is also more
/// confident still wins.
#[test]
fn a_denser_and_more_confident_reading_still_wins() {
    let incumbent = OcrResult::new("REMITTANCE", 60.0);
    let challenger = OcrResult::new("Remittance advice for invoice 4471", 91.0).with_rotation(180);

    let chosen = better_reading(incumbent, challenger);

    assert_eq!(chosen.text, "Remittance advice for invoice 4471");
    assert_eq!(chosen.rotation_degrees, 180);
}

/// A blank page - the back of every sheet of a duplex scan - reads as no
/// words at all, which scores zero confidence. Zero is "not confident", so
/// the orientation search used to buy three more recognition passes and
/// three more full-page PNG encodes to look at the same blank page from
/// three more angles.
#[test]
fn a_blank_page_costs_one_recognition_pass() {
    let blank = OcrResult::new("", 0.0);

    assert!(!orientation_search_is_worthwhile(&blank));
    // What the search used to ask, and why it kept going.
    assert!(blank.mean_confidence < CONFIDENT_READING);
}

#[test]
fn an_unconvincing_reading_is_still_worth_another_orientation() {
    assert!(orientation_search_is_worthwhile(&OcrResult::new(
        "O71 TIVL3Y MOGVAW",
        44.1
    )));
}

#[test]
fn a_confident_reading_is_never_read_again() {
    assert!(!orientation_search_is_worthwhile(&OcrResult::new(
        "PACKING SLIP PS-311",
        95.2
    )));
}
