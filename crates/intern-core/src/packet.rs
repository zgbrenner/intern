use crate::{DocumentPacket, ExtractedDocument};

const GAP_MARKER: &str = "\n\n[... DOCUMENT GAP ...]\n\n";

pub fn build_document_packet(extracted: ExtractedDocument, image_included: bool) -> DocumentPacket {
    let (head_budget, tail_budget) = if image_included { (8_000, 4_000) } else { (14_000, 8_000) };
    let budget = head_budget + tail_budget;
    let character_count = extracted.text.chars().count();
    let text_segments = if character_count <= budget {
        vec![extracted.text]
    } else {
        let head = extracted.text.chars().take(head_budget).collect::<String>();
        let reversed_tail = extracted.text.chars().rev().take(tail_budget).collect::<String>();
        let tail = reversed_tail.chars().rev().collect::<String>();
        vec![head, tail]
    };
    let text = text_segments.join(GAP_MARKER);
    DocumentPacket { text, text_segments, image_included, parser_warnings: extracted.parser_warnings }
}
