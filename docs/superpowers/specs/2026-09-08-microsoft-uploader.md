# Verified Microsoft shared intake

Approved requirement: verified matching uploader only; other/unknown uploads
remain untouched. This is implementation of the approved September 8 design,
with the research-driven restriction that creator metadata is not upload proof.

The normative contract and administrator/security boundary are documented in
[Microsoft upload verification](../../microsoft-upload-verification.md).
Microsoft event evidence is mandatory; locally observed arrival, shared sidecars
and user-entered names cannot substitute. Selected folder access does not limit
audit permission scope. The UI requires explicit acknowledgment of that scope.

Implementation uses a queue AdmissionGuard before hashing, extraction, inference
and apply, plus watcher admission before a claim. Native MicrosoftIntake supplies
the guard. Device OAuth uses fixed Microsoft endpoints, delegated work/school
identity and OS-protected refresh tokens. Profile identity, actual audit upload
event, item/revision metadata, sync checksum and local SHA-256 must agree.
Unknowns remain held through sign-out, throttling, ambiguous history and manual
retry. Only initial unchanged uploads in the pilot window are supported.

UX separates administrator public IDs, personal Microsoft sign-in, saved-folder
pairing, processing scope, machine label and local uploader/processor history.
No tenant/client identity is invented. No live Microsoft test is claimed until
an administrator runs the documented pilot matrix. Existing release checks
remain unchanged; no main merge is part of this task.
