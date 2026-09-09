//! Inferences the document's own wording supports, made after the model has
//! answered and the answer has been checked.
//!
//! Three of them, each replacing a model answer the corpus showed to be weak
//! with something read straight off the page:
//!
//! * **The date role.** The model picks the right date and then calls it
//!   `effective` whatever the document says - the corpus scored six roles
//!   right out of thirteen. The line the chosen date stands on usually says
//!   what kind of date it is ("Invoice Date:", "Date of this Notice:",
//!   "effective as of", "Signed on"), and that wording is more reliable than
//!   the model's label.
//! * **A title-derived document type.** A document with a title has a type,
//!   and the model still occasionally answers none. When the first heading
//!   names a kind of document - minutes, a journal, a receipt - it is used,
//!   and the proposal goes to review saying so.
//! * **The issuer of an invoice.** Asked for one party, the model sometimes
//!   answers two with `between`, leading with whoever was billed. A "Bill To"
//!   line settles which one issued it.
//!
//! Nothing here invents text: a role comes from a line that states the date,
//! a type from a heading, an issuer from a party the model already named.

use crate::distill::DocumentDigest;
use crate::domain::{DateRole, PartyRelation};
use crate::evidence::{date_match_positions, normalize, normalize_loosely};

/// Roles in the order one wins when a line carries several cues: the more
/// specific reading first, so "notice of termination dated" reads as a notice
/// and "will end effective" as a termination rather than an effective date.
const ROLE_PRIORITY: [DateRole; 8] = [
    DateRole::Invoice,
    DateRole::Notice,
    DateRole::Termination,
    DateRole::Amendment,
    DateRole::Filing,
    DateRole::Issuance,
    DateRole::Effective,
    DateRole::Execution,
];

const TERMINATION_CUES: &[&str] = &["terminat", "will end", "ends on", "separation date"];
const INVOICE_CUES: &[&str] = &[
    "invoice date",
    "date of invoice",
    "invoice dated",
    "invoiced on",
    "bill date",
    "billing date",
    "statement date",
];
const NOTICE_CUES: &[&str] = &[
    "date of this notice",
    "notice date",
    "date of notice",
    "notice is given",
    "notice given",
    "notice is hereby given",
    "date of this letter",
    "letter date",
];
const AMENDMENT_CUES: &[&str] = &[
    "amendment date",
    "amended as of",
    "amendment is dated",
    "amendment is effective",
    "amendment effective",
    "amendment is made",
    "amendment made as of",
];
const FILING_CUES: &[&str] = &[
    "filed on",
    "filing date",
    "date filed",
    "date of filing",
    "recorded on",
    "recording date",
];
const ISSUANCE_CUES: &[&str] = &[
    "issued on",
    "issue date",
    "date of issue",
    "date issued",
    "issuance date",
    "journal date",
    "report date",
    "date of report",
    "order date",
    "po date",
    "ship date",
    "shipped on",
    "date shipped",
    "delivery date",
    "delivered on",
    "receipt date",
    "prepared on",
    "published on",
    "publication date",
    "meeting date",
    "meeting held on",
    "held on",
    "date of meeting",
    "minutes of",
];
const EFFECTIVE_CUES: &[&str] = &[
    "effective",
    "commencement",
    "commencing",
    "commence",
    "start date",
    "starts on",
    "begins on",
    "beginning on",
    "in force",
    "entered into as of",
    "made as of",
    "made and entered into",
    "dated as of",
    "term begins",
];
const EXECUTION_CUES: &[&str] = &[
    "signed on",
    "signed this",
    "signed as of",
    "executed on",
    "executed this",
    "executed as of",
    "date signed",
    "signature date",
    "duly executed",
    "witness whereof",
];
/// A label that says "this is the date" without saying what kind.
const GENERIC_DATE_CUES: &[&str] = &["date:", "dated", "date of this"];

/// How far before a date the wording that names its role can sit. Long
/// enough for "This First Amendment to Consulting Agreement (this
/// "Amendment") is dated as of", short enough that a cue from an earlier
/// sentence does not leak in.
const CUE_WINDOW: usize = 96;

/// What kind of date the chosen one is, read from the lines that state it.
///
/// Every non-reference statement of the date is examined; when several lines
/// state it in different roles the most specific wins, so a date that is both
/// "effective as of" and "signed on" reads as effective, the way the prompt
/// ranks them. A line that merely labels the date ("Date:", "Dated") falls
/// back to what the document type implies. `None` when the wording says
/// nothing, in which case the model's own answer stands.
pub fn infer_date_role(
    digest: &DocumentDigest,
    date: &str,
    document_type: Option<&str>,
) -> Option<DateRole> {
    let mut found: Vec<DateRole> = Vec::new();
    let mut generic = false;
    for segment in &digest.segments {
        for line in wrapped_lines(segment) {
            let normalized = normalize(&line);
            for position in date_match_positions(date, &normalized) {
                if crate::validate::reference_introduced(&normalized, position) {
                    continue;
                }
                match role_from_wording(&window_before(&normalized, position)) {
                    Some(role) => {
                        if !found.contains(&role) {
                            found.push(role);
                        }
                    }
                    None => {
                        if is_generic_label(&window_before(&normalized, position)) {
                            generic = true;
                        }
                    }
                }
            }
        }
    }
    let by_wording = ROLE_PRIORITY
        .iter()
        .copied()
        .find(|role| found.contains(role));
    let kind = TypeKind::of(document_type);
    match (by_wording, kind) {
        // An amendment's own date is its amendment date whatever verb the
        // sentence used to state it.
        (Some(DateRole::Effective | DateRole::Execution) | None, TypeKind::Amendment)
            if by_wording.is_some() || generic =>
        {
            Some(DateRole::Amendment)
        }
        (Some(role), _) => Some(role),
        (None, kind) if generic => kind.default_role(),
        (None, _) => None,
    }
}

/// A segment's lines with wrapped sentences rejoined. A PDF breaks "is
/// dated" from "as of September 14, 2025" wherever the margin falls, and the
/// wording that names a date's role must be read across that break. A line
/// is joined to the one before it only when the one before did not end a
/// sentence, a label, or a clause, and it itself carries a sentence on - it
/// starts in lower case, with a number, or with a month - so a header
/// block's "To:" and "From:" lines stay apart, and "Date of this Notice:
/// December 29, 2026" does not lend its cue to the sentence under it.
pub(crate) fn wrapped_lines(segment: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in segment.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match lines.last_mut() {
            Some(previous)
                if !previous.ends_with(['.', ':', ';', '!', '?']) && continues_sentence(line) =>
            {
                previous.push(' ');
                previous.push_str(line);
            }
            _ => lines.push(line.to_owned()),
        }
    }
    lines
}

const MONTHS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

fn continues_sentence(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    if first.is_lowercase() || first.is_ascii_digit() {
        return true;
    }
    let first_word = line
        .split(|character: char| !character.is_alphabetic())
        .next()
        .unwrap_or_default()
        .to_lowercase();
    MONTHS.iter().any(|month| {
        *month == first_word || (first_word.len() >= 3 && month.starts_with(&first_word))
    })
}

/// Whether the wording before a date merely labels it as the date: "Date:",
/// "Dated", "Date of this ...", or a bare "DATE" the way a stamped form
/// writes it.
fn is_generic_label(window: &str) -> bool {
    GENERIC_DATE_CUES.iter().any(|cue| window.contains(cue))
        || window
            .trim_end()
            .rsplit(|character: char| !character.is_alphanumeric())
            .next()
            .is_some_and(|word| word == "date")
}

fn window_before(normalized: &str, position: usize) -> String {
    let mut start = position.saturating_sub(CUE_WINDOW);
    while !normalized.is_char_boundary(start) {
        start -= 1;
    }
    normalized[start..position].to_owned()
}

fn role_from_wording(window: &str) -> Option<DateRole> {
    let has = |cues: &[&str]| cues.iter().any(|cue| window.contains(cue));
    if has(INVOICE_CUES) {
        return Some(DateRole::Invoice);
    }
    if has(NOTICE_CUES) {
        return Some(DateRole::Notice);
    }
    if has(TERMINATION_CUES) {
        return Some(DateRole::Termination);
    }
    if has(AMENDMENT_CUES) {
        return Some(DateRole::Amendment);
    }
    if has(FILING_CUES) {
        return Some(DateRole::Filing);
    }
    if has(ISSUANCE_CUES) {
        return Some(DateRole::Issuance);
    }
    if has(EFFECTIVE_CUES) {
        return Some(DateRole::Effective);
    }
    if has(EXECUTION_CUES) {
        return Some(DateRole::Execution);
    }
    None
}

/// The broad family a document type belongs to, for the defaults a bare
/// "Date:" label falls back to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypeKind {
    Amendment,
    Notice,
    Invoice,
    /// Orders, slips, receipts, reports, minutes, journals, memos,
    /// certificates: things that are issued or written out on a date.
    Issued,
    /// Agreements and their relatives: things that take effect on a date.
    Agreement,
    Unknown,
}

impl TypeKind {
    fn of(document_type: Option<&str>) -> Self {
        let Some(document_type) = document_type else {
            return Self::Unknown;
        };
        let lowered = document_type.to_lowercase();
        let has = |words: &[&str]| words.iter().any(|word| lowered.contains(word));
        if has(&["amendment", "addendum", "modification"]) {
            Self::Amendment
        } else if has(&["notice", "notification"]) {
            Self::Notice
        } else if has(&["invoice", "bill", "statement of account", "credit note"]) {
            Self::Invoice
        } else if has(&[
            "order",
            "slip",
            "receipt",
            "report",
            "minutes",
            "journal",
            "memo",
            "certificate",
            "quote",
            "quotation",
            "estimate",
            "email",
            "e-mail",
            "letter",
            "log",
            "review",
            "summary",
            "agenda",
            "plan",
            "statement",
        ]) {
            Self::Issued
        } else if has(&[
            "agreement",
            "contract",
            "lease",
            "statement of work",
            "terms",
            "policy",
            "license",
            "licence",
            "deed",
            "warranty",
            "guarantee",
            "waiver",
            "release",
            "consent",
        ]) {
            Self::Agreement
        } else {
            Self::Unknown
        }
    }

    fn default_role(self) -> Option<DateRole> {
        match self {
            Self::Amendment => Some(DateRole::Amendment),
            Self::Notice => Some(DateRole::Notice),
            Self::Invoice => Some(DateRole::Invoice),
            Self::Issued => Some(DateRole::Issuance),
            Self::Agreement => Some(DateRole::Effective),
            Self::Unknown => None,
        }
    }
}

/// The document's own title completing the model's type. "Journal" on a
/// document headed "Moonlit Archive Project Journal" is that title, whole:
/// the prompt asks for the document's words for its type, and the title is
/// where they are. Completed only when the title ends with the type and adds
/// a few plain words to it - not an exhibit label, not a party's name, not a
/// leftover "No." from a stripped number - so a completion is always the
/// document's title and never a guess.
pub fn complete_type_from_title(
    document_type: &str,
    digest: &DocumentDigest,
    parties: &[String],
) -> String {
    let Some(title) = infer_document_type(digest) else {
        return document_type.to_owned();
    };
    let title_words = normalize(&title)
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let type_words = normalize(document_type)
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if type_words.is_empty()
        || title_words.len() <= type_words.len()
        || !title_words.ends_with(&type_words)
    {
        return document_type.to_owned();
    }
    let extra = &title_words[..title_words.len() - type_words.len()];
    let party_words = parties
        .iter()
        .flat_map(|party| {
            normalize(party)
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let plain = extra.len() <= 3
        && extra.iter().all(|word| {
            word.chars().count() > 1
                && word.chars().all(char::is_alphabetic)
                && !NOT_A_TITLE.contains(&word.as_str())
                && !party_words.contains(word)
        });
    if plain {
        title
    } else {
        document_type.to_owned()
    }
}

/// Words that make a heading the name of a kind of document.
const TYPE_NOUNS: &[&str] = &[
    "agreement",
    "amendment",
    "addendum",
    "contract",
    "lease",
    "invoice",
    "receipt",
    "order",
    "slip",
    "statement",
    "notice",
    "letter",
    "memorandum",
    "memo",
    "minutes",
    "journal",
    "report",
    "certificate",
    "policy",
    "proposal",
    "quote",
    "quotation",
    "estimate",
    "resolution",
    "agenda",
    "plan",
    "checklist",
    "form",
    "application",
    "specification",
    "review",
    "assessment",
    "audit",
    "license",
    "licence",
    "permit",
    "deed",
    "affidavit",
    "declaration",
    "complaint",
    "motion",
    "subpoena",
    "waiver",
    "release",
    "consent",
    "authorization",
    "authorisation",
    "warranty",
    "guarantee",
    "bond",
    "log",
    "transcript",
    "brief",
    "opinion",
    "ruling",
    "judgment",
    "decree",
    "manual",
    "guide",
    "handbook",
    "protocol",
    "procedure",
    "bill",
    "ticket",
    "itinerary",
    "budget",
    "forecast",
    "ledger",
    "roster",
    "register",
    "inventory",
    "newsletter",
    "bulletin",
    "announcement",
    "summary",
    "evaluation",
    "questionnaire",
    "survey",
    "registration",
    "renewal",
    "termination",
    "offer",
    "engagement",
    "confirmation",
    "acknowledgement",
    "acknowledgment",
    // Initialisms that are document kinds in their own right.
    "nda",
    "sow",
    "msa",
    "mou",
    "loi",
    "rfp",
    "rfq",
    "sla",
    "eula",
    "dpa",
];

/// Headings that head a part of a document, never the document.
const NOT_A_TITLE: &[&str] = &[
    "confidential",
    "exhibit",
    "schedule",
    "appendix",
    "annex",
    "attachment",
    "table of contents",
    "contents",
    "page",
    "draft",
    "article",
    "section",
    "recitals",
    "whereas",
    "definitions",
    "signature",
    "signatures",
    "in witness whereof",
    "background",
    "introduction",
    "re:",
    "subject:",
    "to:",
    "from:",
    "cc:",
    "date:",
    "privileged",
    "sample",
    "copy",
];

/// Words kept lowercase inside a title-cased heading.
const SMALL_WORDS: &[&str] = &[
    "a", "an", "and", "as", "at", "by", "for", "from", "in", "of", "on", "or", "the", "to", "with",
];

/// Short all-capital words that stay capitals in a title: initialisms a
/// filing clerk would never write as "Nda".
const INITIALISMS: &[&str] = &[
    "nda", "sow", "po", "wo", "msa", "loi", "mou", "rfp", "rfq", "sla", "ip", "hr", "it", "llc",
    "inc", "ltd", "plc", "gmbh", "lp", "llp", "usa", "uk", "eu",
];

/// The document type its title states, when the model gave none.
///
/// Only the first heading or two are considered - a title sits at the top -
/// and only a short one that names a kind of document. Identifiers, page
/// fragments, and markdown marks are trimmed away, so "DELIVERY RECEIPT
/// DR-771" becomes "Delivery Receipt" and "MOONLIT ARCHIVE PROJECT JOURNAL -
/// PAGE 1" becomes "Moonlit Archive Project Journal". A heading that names a
/// part ("EXHIBIT A", "CONFIDENTIAL") is skipped, and a document whose first
/// headings name nothing gets no type from here.
pub fn infer_document_type(digest: &DocumentDigest) -> Option<String> {
    digest
        .outline
        .iter()
        .take(2)
        .find_map(|heading| title_type(heading))
}

fn title_type(heading: &str) -> Option<String> {
    let cleaned = clean_title(heading);
    let lowered = cleaned.to_lowercase();
    if lowered.is_empty()
        || NOT_A_TITLE
            .iter()
            .any(|part| lowered == *part || lowered.starts_with(&format!("{part} ")))
    {
        return None;
    }
    let words: Vec<&str> = lowered.split_whitespace().collect();
    if words.is_empty() || words.len() > 8 {
        return None;
    }
    let names_a_kind = words.iter().any(|word| {
        let word = word.trim_matches(|character: char| !character.is_alphanumeric());
        TYPE_NOUNS.contains(&word)
    });
    if !names_a_kind {
        return None;
    }
    Some(title_case(&cleaned))
}

/// Strips markdown marks, a trailing page fragment, identifiers, and dangling
/// punctuation from a heading.
fn clean_title(heading: &str) -> String {
    let mut text = heading.trim().trim_start_matches('#').trim().to_owned();
    let lowered = text.to_lowercase();
    for marker in [" - page ", " – page ", " — page ", " page "] {
        if let Some(index) = lowered.rfind(marker)
            && lowered[index + marker.len()..]
                .trim()
                .chars()
                .all(|character| character.is_ascii_digit() || character.is_whitespace())
        {
            text.truncate(index);
        }
    }
    let kept: Vec<&str> = text
        .split_whitespace()
        .filter(|token| !token.chars().any(|character| character.is_ascii_digit()))
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .collect();
    kept.join(" ")
        .trim_end_matches([':', '-', '–', '—', ',', ';', '.'])
        .trim()
        .to_owned()
}

/// Title case for an all-capitals heading; a mixed-case heading is left as
/// the document wrote it.
fn title_case(value: &str) -> String {
    let all_capitals = value
        .chars()
        .filter(|character| character.is_alphabetic())
        .all(char::is_uppercase);
    if !all_capitals {
        return value.to_owned();
    }
    value
        .split_whitespace()
        .enumerate()
        .map(|(index, word)| {
            let lowered = word.to_lowercase();
            let core = lowered.trim_matches(|character: char| !character.is_alphanumeric());
            if INITIALISMS.contains(&core) {
                word.to_uppercase()
            } else if index > 0 && SMALL_WORDS.contains(&core) {
                lowered
            } else {
                capitalize(&lowered)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize(word: &str) -> String {
    let mut characters = word.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// Document types that have one party - the one that issued them.
///
/// "statement" on its own is an account statement; it is matched whole,
/// because "Statement of Work" contains the word and is an agreement between
/// two parties, and the corpus showed the second party of one being dropped
/// as if it were a customer on a bill.
const ISSUED_TYPES: &[&str] = &[
    "invoice",
    "receipt",
    "bill",
    "statement of account",
    "account statement",
    "bank statement",
    "billing statement",
    "packing slip",
    "purchase order",
    "work order",
    "sales order",
    "quote",
    "quotation",
    "estimate",
    "delivery",
    "credit note",
    "remittance",
];
const CUSTOMER_CUES: &[&str] = &[
    "bill to",
    "billed to",
    "sold to",
    "ship to",
    "invoice to",
    "customer",
    "client",
    "buyer",
    "purchaser",
    "attn",
    "attention",
    "prepared for",
    "deliver to",
    "consignee",
];
const ISSUER_CUES: &[&str] = &[
    "remit to",
    "remittance",
    "payable to",
    "from:",
    "vendor",
    "supplier",
    "seller",
    "issued by",
    "prepared by",
];

/// For an invoice-like document the model gave two parties and "between",
/// the party that issued it, alone, "from". The bill-to, sold-to, or ship-to
/// line names the customer; the other party issued it. Unchanged when the
/// document does not settle the question.
pub fn repair_issued_relation(
    document_type: Option<&str>,
    parties: Vec<String>,
    relation: PartyRelation,
    digest: &DocumentDigest,
) -> (Vec<String>, PartyRelation) {
    let issued_type = document_type.is_some_and(|value| {
        let lowered = value.to_lowercase();
        lowered.trim() == "statement" || ISSUED_TYPES.iter().any(|kind| lowered.contains(kind))
    });
    if !issued_type || parties.len() != 2 || relation != PartyRelation::Between {
        return (parties, relation);
    }
    let lines: Vec<String> = digest
        .segments
        .iter()
        .flat_map(|segment| segment.lines())
        .map(normalize_loosely)
        .collect();
    let on_line_with = |party: &str, cues: &[&str]| {
        let party = normalize_loosely(party);
        !party.is_empty()
            && lines
                .iter()
                .any(|line| line.contains(&party) && cues.iter().any(|cue| line.contains(cue)))
    };
    let customers: Vec<bool> = parties
        .iter()
        .map(|party| on_line_with(party, CUSTOMER_CUES))
        .collect();
    let issuers: Vec<bool> = parties
        .iter()
        .map(|party| on_line_with(party, ISSUER_CUES))
        .collect();
    // Every cue nominates an issuer: an issuer line nominates its own party,
    // a customer line nominates the other one. One nominee, however many
    // cues agree on it, settles the question; two nominees is a document
    // that contradicts itself, and the model's answer stands. (An earlier
    // version wanted exactly one cue, and an invoice with both a "Bill To"
    // and a "Remit To" line - the clearest case there is - was left as
    // "between".)
    let mut nominees = Vec::new();
    for index in 0..2 {
        if issuers[index] {
            nominees.push(index);
        }
        if customers[index] {
            nominees.push(1 - index);
        }
    }
    nominees.sort_unstable();
    nominees.dedup();
    let issuer = match nominees.as_slice() {
        [only] => Some(*only),
        _ => None,
    };
    match issuer {
        Some(index) => (vec![parties[index].clone()], PartyRelation::From),
        None => (parties, relation),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distill::{DigestBudget, distill, source_from_text};

    fn digest_of(text: &str) -> DocumentDigest {
        distill(&source_from_text(text), DigestBudget::default())
    }

    /// Replay of the recorded corpus: the model read the statement of work as
    /// "between" its two parties, and the issued-document repair, matching
    /// "statement", dropped the client as if it were the bill-to on an
    /// invoice.
    #[test]
    fn a_statement_of_work_is_not_an_issued_document() {
        let digest = digest_of(
            "STATEMENT OF WORK NO. 4\nThis Statement of Work is entered into by and between \
             Ridgeline Cartography LLC (\"Provider\") and Vistage Worldwide, Inc. (\"Client\").",
        );
        let parties = vec![
            "Ridgeline Cartography LLC".to_owned(),
            "Vistage Worldwide, Inc.".to_owned(),
        ];
        let (kept, relation) = repair_issued_relation(
            Some("Statement of Work"),
            parties.clone(),
            PartyRelation::Between,
            &digest,
        );
        assert_eq!(kept, parties);
        assert_eq!(relation, PartyRelation::Between);

        let statement = digest_of("STATEMENT\nCustomer: Acme Corporation\nRemit to: First Bank");
        let (kept, relation) = repair_issued_relation(
            Some("Statement"),
            vec!["First Bank".to_owned(), "Acme Corporation".to_owned()],
            PartyRelation::Between,
            &statement,
        );
        assert_eq!(kept, vec!["First Bank"]);
        assert_eq!(relation, PartyRelation::From);
    }

    /// Replay of the recorded corpus: the vendor invoice names its customer
    /// on a "Bill To" line and itself on a "Remit To" line, and the repair
    /// refused to choose because two cues spoke instead of one.
    #[test]
    fn cues_that_agree_on_the_issuer_settle_it() {
        let digest = digest_of(
            "INVOICE\nAcme Corporation, 500 Foundry Road\nBill To: Vistage Worldwide, Inc., Accounts Payable\n\
             Remit To: Acme Corporation, Account 4471-9920",
        );
        let (kept, relation) = repair_issued_relation(
            Some("Invoice"),
            vec![
                "Acme Corporation".to_owned(),
                "Vistage Worldwide, Inc.".to_owned(),
            ],
            PartyRelation::Between,
            &digest,
        );
        assert_eq!(kept, vec!["Acme Corporation"]);
        assert_eq!(relation, PartyRelation::From);

        // Cues that name both parties as the issuer leave the model's answer.
        let contradictory =
            digest_of("INVOICE\nBill To: Acme Corporation\nBill To: Vistage Worldwide, Inc.");
        let (kept, _) = repair_issued_relation(
            Some("Invoice"),
            vec![
                "Acme Corporation".to_owned(),
                "Vistage Worldwide, Inc.".to_owned(),
            ],
            PartyRelation::Between,
            &contradictory,
        );
        assert_eq!(kept.len(), 2);
    }

    /// Replay of the recorded corpus: the amendment's PDF wraps "is dated"
    /// and "as of September 14, 2025" onto different lines, and the role was
    /// read from the second line alone.
    #[test]
    fn the_role_is_read_across_a_wrapped_line() {
        let digest = digest_of(
            "FIRST AMENDMENT TO CONSULTING AGREEMENT\nThis First Amendment to Consulting Agreement (this \"Amendment\") is dated\n\
             as of September 14, 2025, and amends the Consulting Agreement dated\nJanuary 12, 2023.",
        );
        assert_eq!(
            infer_date_role(
                &digest,
                "2025-09-14",
                Some("First Amendment to Consulting Agreement")
            ),
            Some(DateRole::Amendment)
        );
        // A line that ends a sentence, or one that starts a new one, is not
        // joined to its neighbour: the notice date's label keeps its cue.
        assert_eq!(
            wrapped_lines(
                "Date of this Notice: December 29, 2026\nYour employment will end effective\nJanuary 31, 2027.\nTo: John Smith\nFrom: Harriet Voss"
            ),
            vec![
                "Date of this Notice: December 29, 2026".to_owned(),
                "Your employment will end effective January 31, 2027.".to_owned(),
                "To: John Smith".to_owned(),
                "From: Harriet Voss".to_owned(),
            ]
        );
    }

    /// Replay of the recorded corpus: the 100-page journal is headed
    /// "MOONLIT ARCHIVE PROJECT JOURNAL" and the model said "Journal".
    #[test]
    fn the_title_completes_a_type_it_ends_with_and_nothing_else() {
        let journal =
            digest_of("MOONLIT ARCHIVE PROJECT JOURNAL - PAGE 1\nJournal date: July 1, 2025");
        assert_eq!(
            complete_type_from_title("Journal", &journal, &[]),
            "Moonlit Archive Project Journal"
        );
        assert_eq!(
            complete_type_from_title("Project Journal", &journal, &[]),
            "Moonlit Archive Project Journal"
        );
        assert_eq!(
            complete_type_from_title("Minutes", &journal, &[]),
            "Minutes"
        );

        // A stripped number leaves "No", which is not part of any type.
        let numbered = digest_of("STATEMENT OF WORK NO. 4\nEffective April 1, 2026.");
        assert_eq!(
            complete_type_from_title("Statement of Work", &numbered, &[]),
            "Statement of Work"
        );
        // An exhibit label is not a type, and neither is a party's name.
        let exhibit = digest_of("EXHIBIT A STATEMENT OF WORK\nEffective April 1, 2026.");
        assert_eq!(
            complete_type_from_title("Statement of Work", &exhibit, &[]),
            "Statement of Work"
        );
        let branded = digest_of("ACME CORPORATION INVOICE\nInvoice date: May 1, 2025");
        assert_eq!(
            complete_type_from_title("Invoice", &branded, &["Acme Corporation".to_owned()]),
            "Invoice"
        );
    }

    /// Each line is the shape one corpus fixture states its date in, with the
    /// role the corpus says is right.
    #[test]
    fn the_role_is_read_from_the_line_the_date_stands_on() {
        let cases: &[(&str, &str, Option<&str>, DateRole)] = &[
            (
                "INVOICE\nInvoice Number: INV-7741\nInvoice Date: January 5, 2026\nPayment Due Date: February 4, 2026",
                "2026-01-05",
                Some("Invoice"),
                DateRole::Invoice,
            ),
            (
                "NOTICE OF TERMINATION\nDate of this Notice: December 29, 2026\nYour employment will end effective January 31, 2027.",
                "2026-12-29",
                Some("Notice of Termination"),
                DateRole::Notice,
            ),
            (
                "NOTICE OF TERMINATION\nDate of this Notice: December 29, 2026\nYour employment with the Company will end effective January 31, 2027 (the \"Separation Date\").",
                "2027-01-31",
                Some("Notice of Termination"),
                DateRole::Termination,
            ),
            (
                "FIRST AMENDMENT TO CONSULTING AGREEMENT\nThis First Amendment to Consulting Agreement (this \"Amendment\") is dated as of September 14, 2025, and amends the Consulting Agreement dated January 12, 2023.",
                "2025-09-14",
                Some("First Amendment to Consulting Agreement"),
                DateRole::Amendment,
            ),
            (
                "STATEMENT OF WORK\n4.1 Effective Date.\nThis Statement of Work is effective as of April 1, 2026 and continues through March 31, 2027.\nExecuted on April 9, 2026.",
                "2026-04-01",
                Some("Statement of Work"),
                DateRole::Effective,
            ),
            (
                "STATEMENT OF WORK\nThis Statement of Work is effective as of April 1, 2026.\nExecuted on April 9, 2026.",
                "2026-04-09",
                Some("Statement of Work"),
                DateRole::Execution,
            ),
            (
                "ORDER FORM\nSubscription Start Date | February 1, 2026\nSigned on January 14, 2026 by authorized representatives.",
                "2026-02-01",
                Some("Order Form"),
                DateRole::Effective,
            ),
            (
                "MOONLIT ARCHIVE PROJECT JOURNAL - PAGE 1\nJournal date: July 1, 2025\nFictional observation 001",
                "2025-07-01",
                Some("Project Journal"),
                DateRole::Issuance,
            ),
            (
                "CERTIFICATE OF GOOD STANDING\nFiled on March 3, 2026 with the Secretary of State.",
                "2026-03-03",
                Some("Certificate of Good Standing"),
                DateRole::Filing,
            ),
        ];
        for (text, date, document_type, expected) in cases {
            assert_eq!(
                infer_date_role(&digest_of(text), date, *document_type),
                Some(*expected),
                "{text}"
            );
        }
    }

    /// A bare "Date:" says nothing about the kind of date; the kind of
    /// document does.
    #[test]
    fn a_bare_date_label_falls_back_to_what_the_document_type_implies() {
        let cases: &[(&str, &str, Option<&str>, Option<DateRole>)] = &[
            (
                "PURCHASE ORDER PO-310\nDATE JULY 14 2025\nEMBER POST MANUFACTURING LLC",
                "2025-07-14",
                Some("Purchase Order"),
                Some(DateRole::Issuance),
            ),
            (
                "# Quarterly Operations Review\n\n**Date:** May 7, 2025\n\nThe committee reviewed inventory.",
                "2025-05-07",
                Some("Meeting Minutes"),
                Some(DateRole::Issuance),
            ),
            (
                "SERVICES AGREEMENT\nDated January 8, 2025\n\nAcme Corporation and Vistage Worldwide, Inc.",
                "2025-01-08",
                Some("Services Agreement"),
                Some(DateRole::Effective),
            ),
            (
                "INVOICE\nDate: May 1, 2025\nTotal due: $1,248.00",
                "2025-05-01",
                Some("Invoice"),
                Some(DateRole::Invoice),
            ),
            (
                "NOTICE OF DEFAULT\nDated: May 1, 2025",
                "2025-05-01",
                Some("Notice of Default"),
                Some(DateRole::Notice),
            ),
            // Nothing to go on: the model's answer stands.
            ("SOMETHING\nDate: May 1, 2025", "2025-05-01", None, None),
            (
                "SOMETHING\nWe met on May 1, 2025 and agreed nothing.",
                "2025-05-01",
                Some("Services Agreement"),
                None,
            ),
        ];
        for (text, date, document_type, expected) in cases {
            assert_eq!(
                infer_date_role(&digest_of(text), date, *document_type),
                *expected,
                "{text}"
            );
        }
    }

    #[test]
    fn a_referenced_agreements_date_line_carries_no_role_for_this_document() {
        // The chosen date is stated only beside another agreement's name;
        // that occurrence is a reference, and no role is read from it.
        let digest = digest_of(
            "STATEMENT OF WORK\nIssued under the Master Services Agreement dated June 2, 2023\nThis Statement of Work is effective as of April 1, 2026.",
        );
        assert_eq!(
            infer_date_role(&digest, "2023-06-02", Some("Statement of Work")),
            None
        );
    }

    #[test]
    fn a_title_that_names_a_kind_of_document_becomes_its_type() {
        let cases: &[(&str, Option<&str>)] = &[
            (
                "# Quarterly Operations Review\n\n**Date:** May 7, 2025\n\nThe committee reviewed inventory.",
                Some("Quarterly Operations Review"),
            ),
            (
                "MOONLIT ARCHIVE PROJECT JOURNAL - PAGE 1\nJournal date: July 1, 2025\nFictional observation 001",
                Some("Moonlit Archive Project Journal"),
            ),
            (
                "DELIVERY RECEIPT DR-771\nJUNE 12 2025\nPINE ECHO COURIERS LLC",
                Some("Delivery Receipt"),
            ),
            (
                "PURCHASE ORDER PO-310\nDATE JULY 14 2025",
                Some("Purchase Order"),
            ),
            (
                "FIRST AMENDMENT TO CONSULTING AGREEMENT\nThis amendment is dated as of September 14, 2025.",
                Some("First Amendment to Consulting Agreement"),
            ),
            (
                "MUTUAL NDA\nThis Mutual NDA is effective March 3, 2025.",
                Some("Mutual NDA"),
            ),
            // A heading that names a part of a document, not the document.
            ("EXHIBIT A\nSome text follows here.", None),
            ("CONFIDENTIAL\nNotes from the call.", None),
            // The first heading is a part; the title is the second.
            (
                "CONFIDENTIAL\n\nSETTLEMENT AGREEMENT\n\nThis Settlement Agreement is made as of July 22, 2026.",
                Some("Settlement Agreement"),
            ),
            // No heading names a kind of document.
            (
                "ACME CORPORATION\n123 Foundry Road\nRowan called Priya.",
                None,
            ),
            // Prose is not a title.
            (
                "The parties met on Tuesday and agreed to circulate a draft agreement soon.",
                None,
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(
                infer_document_type(&digest_of(text)).as_deref(),
                *expected,
                "{text}"
            );
        }
    }

    #[test]
    fn title_case_keeps_initialisms_and_small_words_in_their_place() {
        assert_eq!(title_case("NOTICE OF TERMINATION"), "Notice of Termination");
        assert_eq!(title_case("MUTUAL NDA"), "Mutual NDA");
        assert_eq!(
            title_case("Quarterly Operations Review"),
            "Quarterly Operations Review"
        );
        assert_eq!(title_case("OF COUNSEL LETTER"), "Of Counsel Letter");
    }

    const INVOICE: &str = "INVOICE\nAcme Corporation\n500 Foundry Road\nInvoice Number: INV-7741\nInvoice Date: January 5, 2026\nBill To: Vistage Worldwide, Inc.\nAnnual platform subscription $42,000\n";

    #[test]
    fn an_invoice_with_two_parties_keeps_the_issuer_alone_as_from() {
        let digest = digest_of(INVOICE);
        let (parties, relation) = repair_issued_relation(
            Some("Invoice"),
            vec![
                "Vistage Worldwide, Inc.".to_owned(),
                "Acme Corporation".to_owned(),
            ],
            PartyRelation::Between,
            &digest,
        );
        assert_eq!(parties, vec!["Acme Corporation".to_owned()]);
        assert_eq!(relation, PartyRelation::From);
    }

    #[test]
    fn a_remit_to_line_names_the_issuer_directly() {
        let digest = digest_of(
            "INVOICE\nRemit to: Acme Corporation\nInvoice Date: January 5, 2026\nVistage Worldwide, Inc.\n",
        );
        let (parties, relation) = repair_issued_relation(
            Some("Invoice"),
            vec![
                "Vistage Worldwide, Inc.".to_owned(),
                "Acme Corporation".to_owned(),
            ],
            PartyRelation::Between,
            &digest,
        );
        assert_eq!(parties, vec!["Acme Corporation".to_owned()]);
        assert_eq!(relation, PartyRelation::From);
    }

    #[test]
    fn the_relation_is_left_alone_when_the_document_does_not_settle_it() {
        let digest = digest_of(
            "INVOICE\nInvoice Date: January 5, 2026\nAcme Corporation\nVistage Worldwide, Inc.\n",
        );
        let parties = vec![
            "Vistage Worldwide, Inc.".to_owned(),
            "Acme Corporation".to_owned(),
        ];
        let (kept, relation) = repair_issued_relation(
            Some("Invoice"),
            parties.clone(),
            PartyRelation::Between,
            &digest,
        );
        assert_eq!(kept, parties);
        assert_eq!(relation, PartyRelation::Between);
        // Not an issued document: never touched.
        let (kept, relation) = repair_issued_relation(
            Some("Services Agreement"),
            parties.clone(),
            PartyRelation::Between,
            &digest_of(INVOICE),
        );
        assert_eq!(kept, parties);
        assert_eq!(relation, PartyRelation::Between);
    }
}
