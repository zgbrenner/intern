/**
 * The pipeline reports review reasons and failures as stable machine codes -
 * "DATE_UNSUPPORTED", "SOURCE_LOCKED" - and until now the inspector showed
 * those codes to the person deciding what to do about them. Each sentence here
 * says what actually happened and, where one exists, what the person can do.
 *
 * A code with no entry passes through unchanged: an unmapped code is a bug to
 * notice, not something to dress up as prose.
 */
const SENTENCES: Record<string, string> = {
  DATE_MISSING: 'No date that defines the document was found.',
  DATE_UNSUPPORTED: 'The proposed date does not appear verbatim in the document.',
  TYPE_MISSING: 'No specific document type was identified.',
  TYPE_UNSUPPORTED: 'The proposed document type does not appear verbatim in the document.',
  TYPE_INFERRED: 'The model gave no document type, so the document\'s own title was used; check that it names the document.',
  PARTY_UNSUPPORTED: 'A proposed party could not be found in the document.',
  DESCRIPTION_UNSUPPORTED: 'The description asserts something the document does not contain.',
  DESCRIPTION_INVALID: 'The description was not a single usable sentence.',
  LOW_CONFIDENCE: 'The model reported low confidence in its own proposal.',
  MODEL_REQUESTED_REVIEW: 'The model asked for a person to look at this one.',
  PARSER_WARNING: 'Extraction reported a problem that could corrupt what was read.',
  FILE_CHANGED: 'The file changed after it was analyzed, so the result no longer describes it.',
  SOURCE_LOCKED: 'Another program is still holding this file open — often a sync client mid-upload. Retry once it settles.',
  DESTINATION_UNAVAILABLE: 'The destination folder is unavailable, or already has a file with this name.',
  MOVE_VERIFICATION_FAILED: 'The rename could not be verified as intact, so it was not finalized.',
  SOURCE_DELETE_FAILED: 'The renamed copy is safe, but the original file could not be removed.',
  PROPOSAL_MISSING: 'Analysis finished without a usable proposal.',
  IO_ERROR: 'A file operation failed.',
  // Raised before analysis when the content was filed before; the name it was
  // filed under is normally given instead, as "Duplicate of ...". The bare code
  // only reaches here once the record of that filing is gone.
  DUPLICATE: 'This document\'s content was filed once already. Retry to process it anyway, or remove it.',
};

/**
 * Translates a reason string - a single code, or the pipeline's comma-joined
 * list of codes - into sentences. Anything that is not a known code is kept
 * verbatim, including free text like "Duplicate of X".
 */
export function humanizeReason(reason: string): string {
  const parts = reason.split(',').map((part) => part.trim()).filter((part) => part.length > 0);
  // Only a list in which every entry is a known code is a code list; free text
  // can legitimately contain commas ("Duplicate of ... Worldwide, Inc ...")
  // and must come through with them intact.
  if (parts.length === 0 || !parts.every((part) => SENTENCES[part.toUpperCase()] !== undefined)) {
    return reason;
  }
  return parts.map((part) => SENTENCES[part.toUpperCase()]).join(' ');
}
