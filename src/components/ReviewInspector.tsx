import { Ban, CalendarPlus, ClipboardCopy, Ellipsis, FileCheck2, FileText, RotateCcw, Trash2, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { ConfidenceMeter } from './ConfidenceMeter';
import { FileKindIcon } from './FileKindIcon';
import { Icon } from './Icon';
import { StatusCell } from './StatusCell';
import { leadingDate, withLeadingDate } from '../lib/filenames';
import type { QueueItem } from '../types';

/** What the inspector says when a name has no date; the backend says the same in DATE_REQUIRED. */
export const DATE_REQUIRED_MESSAGE = 'Every rename needs a date. Start the filename with the document\'s date as YYYY-MM-DD.';

/**
 * The pipeline joins several parties into one string with semicolons. Shown as
 * one run of text it reads like a database field; split back out, each party
 * is its own quotation and a reader can check them one at a time.
 */
function quotations(value?: string): string[] {
  return (value ?? '').split(';').map((part) => part.trim()).filter((part) => part.length > 0);
}

interface Props { item: QueueItem; drawer: boolean; busy?: boolean; onClose(): void; onApprove(filename: string, description: string): void; onKeep(): void; onCancel(): void; onRetry(): void; onRemove(): void; onUndo(): void }
export function ReviewInspector({ item, drawer, busy, onClose, onApprove, onKeep, onCancel, onRetry, onRemove, onUndo }: Props) {
  const [filename, setFilename] = useState(item.proposedFilename ?? '');
  const [description, setDescription] = useState(item.description ?? '');
  const [error, setError] = useState('');
  const [moreOpen, setMoreOpen] = useState(false);
  const inspectorRef = useRef<HTMLElement>(null);
  const filenameRef = useRef<HTMLInputElement>(null);
  useEffect(() => { setFilename(item.proposedFilename ?? ''); setDescription(item.description ?? ''); setError(''); setMoreOpen(false); }, [item.id, item.proposalRevision]);
  useEffect(() => {
    if (!drawer) return;
    const filenameInput = filenameRef.current;
    if (filenameInput && !filenameInput.disabled) filenameInput.focus();
    else inspectorRef.current?.querySelector<HTMLElement>('button:not(:disabled)')?.focus();
  }, [drawer, item.id]);
  const dated = leadingDate(filename) !== undefined;
  const approve = () => {
    if (!filename.trim()) { setError('Filename is required'); return; }
    if (!dated) { setError(DATE_REQUIRED_MESSAGE); return; }
    onApprove(filename.trim(), description);
  };
  // The model's date, when Intern could not find it written in the document
  // and so left it out of the name. Offered until the name carries a date.
  const suggestedDate = item.suggestedDate && !dated ? item.suggestedDate : undefined;
  const useSuggestedDate = () => { setFilename(withLeadingDate(filename, suggestedDate!)); setError(''); };
  const acceptSuggestedDate = () => onApprove(withLeadingDate(filename, suggestedDate!), description);
  // When the name has no date and the model offered none worth showing, the
  // document's own dates - and, last, the file's - are one click each.
  const dateChoices = !dated ? (item.datesInDocument ?? []) : [];
  const fileDate = !dated ? item.fileModifiedDate : undefined;
  const chooseDate = (date: string) => { setFilename(withLeadingDate(filename, date)); setError(''); };
  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (!drawer) return;
    if (event.key === 'Escape') { event.preventDefault(); onClose(); return; }
    if (event.key !== 'Tab') return;
    const focusable = [...(inspectorRef.current?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [tabindex]:not([tabindex="-1"])') ?? [])];
    const first = focusable.at(0);
    const last = focusable.at(-1);
    if (!first || !last) return;
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  };
  const editable = item.status === 'ready' || item.status === 'review';
  // Each row is a claim the proposed filename makes, paired with the text in
  // the document that supports it. A bare definition list read as metadata
  // about the file; attributing a quotation to the part of the name it
  // justifies is what makes it evidence a person can check.
  const evidence = [
    { label: 'Date', quotes: quotations(item.evidence?.date) },
    { label: 'Type', quotes: quotations(item.evidence?.type) },
    { label: 'Parties', quotes: quotations(item.evidence?.parties) },
  ].filter((entry) => entry.quotes.length > 0);
  return <aside ref={inspectorRef} className="inspector" aria-label="Review item" role={drawer ? 'dialog' : 'complementary'} aria-modal={drawer || undefined} onKeyDown={onKeyDown}>
    <div className="inspector-title"><h2>Review item</h2><button type="button" className="icon-button" onClick={onClose} aria-label="Close review"><Icon icon={X} /></button></div>
    <div className="source-file">
      <p className="field-label">Current name</p>
      <p className="selected-file"><FileKindIcon filename={item.originalFilename} />{item.originalFilename}</p>
      <StatusCell item={item} />
    </div>
    {editable && <section className="proposal">
      <h3>Proposed rename</h3>
      <label>Filename<input ref={filenameRef} aria-label="Filename" className="filename-input" value={filename} onChange={(event) => setFilename(event.target.value)} aria-invalid={Boolean(error)} /></label>
      {error && <p className="form-error" role="alert">{error}</p>}
      {!error && filename.trim() && !dated && !suggestedDate && <p className="check-hint date-hint">{DATE_REQUIRED_MESSAGE}</p>}
      {/*
        The gate above would otherwise be a dead end for the commonest review:
        the model read a date the document never states verbatim, so the name
        has none. The model's reading is shown, said to be unverified, and
        accepted in one click - into the field, or straight through to the
        rename.
      */}
      {(dateChoices.length > 0 || fileDate) && <div className="date-choices" role="group" aria-label="Dates to choose from">
        <p className="field-label">{dateChoices.length > 0 ? 'Dates the document states' : 'The document states no date'}</p>
        <div className="chips">
          {dateChoices.map((date) => <button type="button" key={date} className="chip" disabled={busy} onClick={() => chooseDate(date)}>{date}</button>)}
          {fileDate && <button type="button" className="chip chip--file" disabled={busy} title="The file's last-modified date on this computer, not a date from the document" onClick={() => chooseDate(fileDate)}>{fileDate} · file date</button>}
        </div>
      </div>}
      {suggestedDate && <div className="suggestion" role="group" aria-label="Suggested date">
        <p><Icon icon={CalendarPlus} />The model read <strong>{suggestedDate}</strong> as the document's date, but Intern could not find it written in the document. Every rename needs a date.</p>
        <div className="suggestion-actions">
          <button type="button" disabled={busy} onClick={useSuggestedDate}>Use this date</button>
          <button type="button" className="primary" disabled={busy} onClick={acceptSuggestedDate}>Use date &amp; rename</button>
        </div>
      </div>}
      {item.confidence !== undefined && <p className={`confidence-readout confidence ${item.status}`}><span className="field-label">Confidence</span><ConfidenceMeter value={item.confidence} status={item.status} variant="panel" /></p>}
      <label>Description<textarea value={description} onChange={(event) => setDescription(event.target.value)} /></label>
    </section>}
    {/*
      A renamed file becomes `completed`, which made `editable` false and took
      the description off screen with it. The sentence describing the document
      is the other half of what Intern produces, and it was unreachable the
      moment it was most useful. Read-only here because the proposal is settled,
      with a copy action so it can go somewhere else.
    */}
    {!editable && item.description && <section><h3>Description</h3><p className="settled-description">{item.description}</p>
      <button type="button" className="copy-description" onClick={() => void navigator.clipboard?.writeText(item.description ?? '')}><Icon icon={ClipboardCopy} />Copy description</button></section>}
    {evidence.length > 0 && <section className="evidence"><h3>Evidence</h3>
      <p className="section-lead">Text found in the document that supports this name.</p>
      <dl className="evidence-list">{evidence.map(({ label, quotes }) => <div className="evidence-item" key={label}>
        <dt className="field-label">{label}</dt>
        <dd>{quotes.map((quote) => <q key={quote}>{quote}</q>)}</dd>
      </div>)}</dl></section>}
    {item.reason && <section className={`note note--${item.status === 'failed' ? 'failed' : 'review'}`}><h3>{item.status === 'failed' ? 'Failure details' : 'Reason for review'}</h3><p>{item.reason}</p></section>}
    <div className="inspector-actions">
      {item.status === 'review' && <><button type="button" className="primary" disabled={busy} onClick={approve}><Icon icon={FileCheck2} />Approve & rename</button><button type="button" className="secondary-action" disabled={busy} onClick={onKeep}><Icon icon={FileText} />Keep original</button><button type="button" className="icon-button more-actions" disabled={busy} aria-label="More review actions" aria-expanded={moreOpen} onClick={() => setMoreOpen(!moreOpen)}><Icon icon={Ellipsis} /></button>{moreOpen && <div className="review-menu" role="group" aria-label="More review actions"><button type="button" disabled={busy} onClick={onRetry}><Icon icon={RotateCcw} />Retry</button><button type="button" disabled={busy} onClick={onRemove}><Icon icon={Trash2} />Remove</button></div>}</>}
      {item.status === 'ready' && <button type="button" className="primary" disabled={busy} onClick={approve}><Icon icon={FileCheck2} />Apply rename</button>}
      {item.status === 'processing' && item.cancelable !== false && <button type="button" disabled={busy} onClick={onCancel}><Icon icon={Ban} />Cancel processing</button>}
      {item.status === 'failed' && <><button type="button" disabled={busy} onClick={onRetry}><Icon icon={RotateCcw} />Retry item</button><button type="button" disabled={busy} onClick={onRemove}><Icon icon={Trash2} />Remove item</button></>}
      {item.status === 'completed' && item.undoable && <button type="button" disabled={busy} onClick={onUndo}><Icon icon={RotateCcw} />Undo</button>}
    </div>
  </aside>;
}
