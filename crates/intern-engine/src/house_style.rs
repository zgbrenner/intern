//! House style: the spellings a reviewer prefers over the document's own.
//!
//! The document says "Vistage Worldwide, Inc."; the person filing it calls it
//! "Vistage", and every name they fix says so. A rule records that
//! substitution, and naming applies it to every later document. It is applied
//! deterministically, after validation, so the evidence checks still run
//! against what the document says and the model is never asked to spell
//! anything but the document's words. Nothing here changes what Intern
//! believes about a document - only what it calls it.
//!
//! A rule is learned from an edit, never from the model: the name Intern
//! proposed and the name the reviewer approved are compared field by field,
//! and only an edit confined to one party or to the type teaches anything.
//! An edit that touches two fields, the connecting words, or the date is a
//! decision about that document, not a preference about names.

use serde::{Deserialize, Serialize};

use crate::domain::{PartyRelation, ValidatedProposal};
use crate::evidence::is_valid_iso_date;
use crate::naming::{
    DEFAULT_TYPE, sanitize_extension, sanitize_segment, strip_duplicate_extension,
};

/// Which part of a name a rule rewrites.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// A party's name.
    Party,
    /// The document type.
    DocumentType,
}

impl RuleKind {
    pub const ALL: [Self; 2] = [Self::Party, Self::DocumentType];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Party => "party",
            Self::DocumentType => "document_type",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// One spelling the reviewer prefers: `from` as the document writes it,
/// `to` as the reviewer wrote it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HouseRule {
    pub kind: RuleKind,
    pub from: String,
    pub to: String,
}

impl HouseRule {
    pub fn new(kind: RuleKind, from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            kind,
            from: from.into().trim().to_owned(),
            to: to.into().trim().to_owned(),
        }
    }

    /// The form a spelling is matched under: case, punctuation, and spacing
    /// disregarded, so "Vistage Worldwide, Inc." and "VISTAGE WORLDWIDE INC"
    /// are one spelling. Words are never loosened.
    pub fn key(value: &str) -> String {
        value
            .chars()
            .filter(|character| character.is_alphanumeric() || character.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn from_key(&self) -> String {
        Self::key(&self.from)
    }

    pub fn matches(&self, value: &str) -> bool {
        let key = Self::key(value);
        !key.is_empty() && key == self.from_key()
    }

    /// A rule that would change nothing, or that maps to nothing a name can
    /// carry, teaches nothing.
    pub fn is_meaningful(&self) -> bool {
        !self.from_key().is_empty()
            && Self::key(&self.to) != self.from_key()
            && sanitize_segment(&self.to).is_some()
    }
}

/// The rules in force, applied to a validated proposal before naming.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct HouseStyle {
    rules: Vec<HouseRule>,
}

impl HouseStyle {
    pub fn new(rules: Vec<HouseRule>) -> Self {
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn rules(&self) -> &[HouseRule] {
        &self.rules
    }

    /// Rewrites the type and the parties the rules cover. The rules that
    /// fired come back with the proposal, in the order of the fields they
    /// changed, so a reviewer can be told why a name differs from the
    /// document's words.
    pub fn apply(&self, proposal: &ValidatedProposal) -> (ValidatedProposal, Vec<HouseRule>) {
        let mut styled = proposal.clone();
        let mut applied = Vec::new();
        if let Some(document_type) = proposal.document_type.as_deref()
            && let Some(rule) = self.rule_for(RuleKind::DocumentType, document_type)
        {
            styled.document_type = Some(rule.to.clone());
            applied.push(rule.clone());
        }
        let mut parties = Vec::with_capacity(proposal.parties.len());
        for party in &proposal.parties {
            let spelled = match self.rule_for(RuleKind::Party, party) {
                Some(rule) => {
                    applied.push(rule.clone());
                    rule.to.clone()
                }
                None => party.clone(),
            };
            // Two of the document's names can be one of the reviewer's.
            if !parties
                .iter()
                .any(|existing: &String| HouseRule::key(existing) == HouseRule::key(&spelled))
            {
                parties.push(spelled);
            }
        }
        styled.parties = parties;
        (styled, applied)
    }

    fn rule_for(&self, kind: RuleKind, value: &str) -> Option<&HouseRule> {
        self.rules
            .iter()
            .find(|rule| rule.kind == kind && rule.matches(value))
    }
}

/// What one edit teaches: the reviewer changed exactly one party or the
/// type, and nothing else, so that field's spelling is a preference.
///
/// `proposal` is the proposal the proposed name was composed from - after
/// any house style already applied to it - and `proposed` is the name as
/// Intern offered it, collision suffix and all. Both names are read without
/// their extension, their date, and any ` (2)` suffix, so a reviewer who
/// typed a date and fixed a name in one go still teaches the name.
///
/// The names are read with the grammar that composed them - type, connecting
/// word, party, `and`, party - rather than by finding the smallest edit,
/// because the smallest edit lies: "Acme and Vistage" becomes "Acme Corp and
/// Vistage Inc" by inserting text one character into the connector, and a
/// diff would credit it all to one party.
pub fn lesson_from_edit(
    proposal: &ValidatedProposal,
    extension: &str,
    proposed: &str,
    approved: &str,
) -> Option<HouseRule> {
    let extension = sanitize_extension(extension);
    let before = editable_stem(proposed, &extension);
    let after = editable_stem(approved, &extension);
    if before == after || after.is_empty() {
        return None;
    }
    let shape = NameShape::of(proposal, &before, &extension)?;
    let rule = match shape.clause {
        None => HouseRule::new(
            RuleKind::DocumentType,
            proposal.document_type.clone()?,
            after,
        ),
        Some(clause) => {
            let connector = format!(" {} ", clause.connector);
            let type_unchanged = after
                .strip_prefix(shape.type_segment.as_str())
                .and_then(|rest| rest.strip_prefix(connector.as_str()));
            let clause_unchanged = after
                .strip_suffix(clause.text.as_str())
                .and_then(|rest| rest.strip_suffix(connector.as_str()));
            match (type_unchanged, clause_unchanged) {
                (Some(new_clause), _) if new_clause != clause.text => clause.lesson(new_clause)?,
                (_, Some(new_type)) => HouseRule::new(
                    RuleKind::DocumentType,
                    proposal.document_type.clone()?,
                    new_type,
                ),
                // Both the type and the party clause changed, or the
                // connecting word did: a decision about this document.
                _ => return None,
            }
        }
    };
    rule.is_meaningful().then_some(rule)
}

/// The composed name's own grammar, matched against the stem Intern offered,
/// so each part can be told apart in the name the reviewer approved.
struct NameShape {
    type_segment: String,
    clause: Option<PartyClause>,
}

struct PartyClause {
    connector: &'static str,
    text: String,
    /// The parties in the clause, as the proposal spells them, with the
    /// sanitised segment each contributed to the name.
    parties: Vec<(String, String)>,
}

impl NameShape {
    /// `None` when the stem is not one this proposal composes - it was
    /// truncated for length, or came from somewhere else - because guessing
    /// which words are which would teach the wrong thing.
    fn of(proposal: &ValidatedProposal, stem: &str, extension: &str) -> Option<Self> {
        let type_segment = proposal
            .document_type
            .as_deref()
            .map(|value| strip_duplicate_extension(value, extension))
            .and_then(sanitize_segment)
            .unwrap_or_else(|| DEFAULT_TYPE.to_owned());
        let parties = proposal
            .parties
            .iter()
            .filter_map(|party| sanitize_segment(party).map(|segment| (party.clone(), segment)))
            .collect::<Vec<_>>();
        // The same order naming sheds detail in: both parties, one, none.
        let mut candidates = Vec::new();
        if let [first, second, ..] = parties.as_slice()
            && proposal.party_relation == PartyRelation::Between
        {
            candidates.push(PartyClause {
                connector: connector_word(PartyRelation::Between),
                text: format!("{} and {}", first.1, second.1),
                parties: vec![first.clone(), second.clone()],
            });
        }
        if let Some(first) = parties.first() {
            let relation = match proposal.party_relation {
                PartyRelation::Between => PartyRelation::With,
                other => other,
            };
            candidates.push(PartyClause {
                connector: connector_word(relation),
                text: first.1.clone(),
                parties: vec![first.clone()],
            });
        }
        for clause in candidates {
            if stem == format!("{type_segment} {} {}", clause.connector, clause.text) {
                return Some(Self {
                    type_segment,
                    clause: Some(clause),
                });
            }
        }
        (stem == type_segment).then_some(Self {
            type_segment,
            clause: None,
        })
    }
}

impl PartyClause {
    /// The one party the reviewer respelled, if exactly one was.
    fn lesson(&self, approved: &str) -> Option<HouseRule> {
        match self.parties.as_slice() {
            [(from, _)] => Some(HouseRule::new(RuleKind::Party, from.clone(), approved)),
            [(first, first_segment), (second, second_segment)] => {
                let second_changed = approved
                    .strip_prefix(first_segment.as_str())
                    .and_then(|rest| rest.strip_prefix(" and "));
                let first_changed = approved
                    .strip_suffix(second_segment.as_str())
                    .and_then(|rest| rest.strip_suffix(" and "));
                match (second_changed, first_changed) {
                    (Some(to), _) if to != second_segment => {
                        Some(HouseRule::new(RuleKind::Party, second.clone(), to))
                    }
                    (_, Some(to)) => Some(HouseRule::new(RuleKind::Party, first.clone(), to)),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// The word naming puts between the type and the parties.
fn connector_word(relation: PartyRelation) -> &'static str {
    match relation {
        PartyRelation::None => "-",
        stated => stated.as_str(),
    }
}

/// The stem of a filename as a reviewer edits it: no extension, no leading
/// date, no collision suffix.
fn editable_stem(filename: &str, extension: &str) -> String {
    let mut stem = filename.trim();
    if !extension.is_empty()
        && let Some(prefix) = stem
            .len()
            .checked_sub(extension.len() + 1)
            .and_then(|start| stem.get(..start).zip(stem.get(start..)))
            .filter(|(_, suffix)| {
                suffix.starts_with('.') && suffix[1..].eq_ignore_ascii_case(extension)
            })
            .map(|(prefix, _)| prefix)
    {
        stem = prefix;
    }
    if let Some(date) = stem.get(..10)
        && is_valid_iso_date(date)
        && stem[10..]
            .chars()
            .next()
            .is_none_or(|next| !next.is_alphanumeric())
    {
        stem = stem[10..].trim_start();
    }
    strip_collision_suffix(stem).trim().to_owned()
}

/// `Invoice (2)` is `Invoice` with a collision the reviewer never typed.
fn strip_collision_suffix(stem: &str) -> &str {
    let trimmed = stem.trim_end();
    let Some(open) = trimmed.rfind(" (") else {
        return trimmed;
    };
    let inside = &trimmed[open + 2..];
    if inside.ends_with(')')
        && inside.len() > 1
        && inside[..inside.len() - 1]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        trimmed[..open].trim_end()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DateRole, Evidence};
    use crate::naming::compose_filename;

    fn proposal(
        document_type: Option<&str>,
        parties: &[&str],
        relation: PartyRelation,
    ) -> ValidatedProposal {
        ValidatedProposal {
            document_type: document_type.map(str::to_owned),
            document_date: Some("2026-04-01".into()),
            date_role: Some(DateRole::Effective),
            parties: parties.iter().map(|value| (*value).to_owned()).collect(),
            party_relation: relation,
            description: "A description of the document.".into(),
            confidence: 0.9,
            evidence: Evidence::default(),
        }
    }

    fn name(proposal: &ValidatedProposal) -> String {
        compose_filename(proposal, "pdf", &[]).value
    }

    #[test]
    fn a_party_rule_rewrites_the_document_spelling_and_says_so() {
        let style = HouseStyle::new(vec![HouseRule::new(
            RuleKind::Party,
            "Vistage Worldwide, Inc.",
            "Vistage",
        )]);
        let proposal = proposal(
            Some("Statement of Work"),
            &["Ridgeline Cartography LLC", "VISTAGE WORLDWIDE INC"],
            PartyRelation::Between,
        );
        let (styled, applied) = style.apply(&proposal);
        assert_eq!(
            name(&styled),
            "2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage.pdf"
        );
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].to, "Vistage");
        assert_eq!(styled.evidence, proposal.evidence, "evidence is untouched");
    }

    #[test]
    fn a_type_rule_rewrites_the_type_and_no_rule_rewrites_nothing() {
        let style = HouseStyle::new(vec![HouseRule::new(
            RuleKind::DocumentType,
            "Quarterly Operations Review",
            "Meeting Minutes",
        )]);
        let minutes = proposal(
            Some("Quarterly Operations Review"),
            &[],
            PartyRelation::None,
        );
        let (styled, applied) = style.apply(&minutes);
        assert_eq!(styled.document_type.as_deref(), Some("Meeting Minutes"));
        assert_eq!(applied.len(), 1);

        let invoice = proposal(Some("Invoice"), &["Acme Corporation"], PartyRelation::From);
        let (unchanged, applied) = style.apply(&invoice);
        assert_eq!(unchanged, invoice);
        assert!(applied.is_empty());
    }

    #[test]
    fn two_of_the_documents_names_that_become_one_are_named_once() {
        let style = HouseStyle::new(vec![
            HouseRule::new(RuleKind::Party, "Acme Corporation", "Acme"),
            HouseRule::new(RuleKind::Party, "Acme Corp.", "Acme"),
        ]);
        let proposal = proposal(
            Some("Invoice"),
            &["Acme Corporation", "Acme Corp."],
            PartyRelation::Between,
        );
        let (styled, _) = style.apply(&proposal);
        assert_eq!(styled.parties, vec!["Acme"]);
    }

    #[test]
    fn shortening_a_party_in_review_teaches_that_partys_spelling() {
        let proposal = proposal(
            Some("Statement of Work"),
            &["Ridgeline Cartography LLC", "Vistage Worldwide, Inc."],
            PartyRelation::Between,
        );
        let proposed = name(&proposal);
        let lesson = lesson_from_edit(
            &proposal,
            "pdf",
            &proposed,
            "2026-04-01 Statement of Work between Ridgeline Cartography LLC and Vistage.pdf",
        )
        .expect("the second party changed and nothing else did");
        assert_eq!(lesson.kind, RuleKind::Party);
        assert_eq!(lesson.from, "Vistage Worldwide, Inc.");
        assert_eq!(lesson.to, "Vistage");
    }

    #[test]
    fn lengthening_a_party_and_changing_the_date_at_once_still_teaches_the_party() {
        let proposal = proposal(Some("Invoice"), &["Acme"], PartyRelation::From);
        let lesson = lesson_from_edit(
            &proposal,
            "pdf",
            "2026-04-01 Invoice from Acme (2).pdf",
            "2026-05-09 Invoice from Acme Corporation.pdf",
        )
        .expect("the date and the collision suffix are not part of the lesson");
        assert_eq!(lesson.from, "Acme");
        assert_eq!(lesson.to, "Acme Corporation");
    }

    #[test]
    fn renaming_the_type_teaches_the_type_even_when_it_shares_letters() {
        let proposal = proposal(
            Some("Statement of Work"),
            &["Acme", "Vistage"],
            PartyRelation::Between,
        );
        let lesson = lesson_from_edit(
            &proposal,
            "pdf",
            "2026-04-01 Statement of Work between Acme and Vistage.pdf",
            "2026-04-01 SOW between Acme and Vistage.pdf",
        )
        .unwrap();
        assert_eq!(lesson.kind, RuleKind::DocumentType);
        assert_eq!(lesson.from, "Statement of Work");
        assert_eq!(lesson.to, "SOW");

        let prefixed = lesson_from_edit(
            &proposal,
            "pdf",
            "2026-04-01 Statement of Work between Acme and Vistage.pdf",
            "2026-04-01 Signed Statement of Work between Acme and Vistage.pdf",
        )
        .unwrap();
        assert_eq!(prefixed.to, "Signed Statement of Work");
    }

    #[test]
    fn an_edit_that_touches_two_fields_or_the_connectors_teaches_nothing() {
        let proposal = proposal(
            Some("Statement of Work"),
            &["Acme", "Vistage"],
            PartyRelation::Between,
        );
        let proposed = "2026-04-01 Statement of Work between Acme and Vistage.pdf";
        for approved in [
            // Both parties at once.
            "2026-04-01 Statement of Work between Acme Corp and Vistage Inc.pdf",
            // The relation, which is a fact about the document.
            "2026-04-01 Statement of Work with Acme.pdf",
            // The connecting word alone.
            "2026-04-01 Statement of Work for Acme and Vistage.pdf",
            // Only the date.
            "2026-05-01 Statement of Work between Acme and Vistage.pdf",
            // A rewrite from scratch.
            "2026-04-01 Acme deal.pdf",
        ] {
            assert_eq!(
                lesson_from_edit(&proposal, "pdf", proposed, approved),
                None,
                "{approved}"
            );
        }
    }

    #[test]
    fn a_removed_party_and_a_missing_type_teach_nothing() {
        let with_party = proposal(Some("Invoice"), &["Acme Corporation"], PartyRelation::From);
        assert_eq!(
            lesson_from_edit(
                &with_party,
                "pdf",
                "2026-04-01 Invoice from Acme Corporation.pdf",
                "2026-04-01 Invoice from.pdf"
            ),
            None,
            "an empty spelling is not a spelling"
        );
        let untyped = proposal(None, &[], PartyRelation::None);
        assert_eq!(
            lesson_from_edit(
                &untyped,
                "pdf",
                "2026-04-01 Document.pdf",
                "2026-04-01 Board Pack.pdf"
            ),
            None,
            "there is no document spelling to map from"
        );
    }

    #[test]
    fn a_name_intern_did_not_compose_teaches_nothing() {
        let proposal = proposal(Some("Invoice"), &["Acme"], PartyRelation::From);
        assert_eq!(
            lesson_from_edit(
                &proposal,
                "pdf",
                "2026-04-01 Receipt from Acme.pdf",
                "2026-04-01 Receipt from Acme Ltd.pdf"
            ),
            None
        );
    }

    #[test]
    fn keys_disregard_case_punctuation_and_spacing_but_never_words() {
        assert_eq!(
            HouseRule::key("Vistage Worldwide, Inc."),
            HouseRule::key("  VISTAGE   WORLDWIDE INC ")
        );
        assert_ne!(
            HouseRule::key("Vistage"),
            HouseRule::key("Vistage Worldwide")
        );
        let rule = HouseRule::new(RuleKind::Party, "Acme Corp.", "Acme Corp");
        assert!(
            !rule.is_meaningful(),
            "punctuation alone is not a preference"
        );
        assert!(!HouseRule::new(RuleKind::Party, "Acme", "???").is_meaningful());
    }
}
