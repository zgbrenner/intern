import { Ban, ClipboardCopy, Ellipsis, FileCheck2, FileText, RotateCcw, Trash2, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { Icon } from './Icon';
import type { QueueItem } from '../types';

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
  const approve = () => { if (!filename.trim()) { setError('Filename is required'); return; } onApprove(filename.trim(), description); };
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
  const hasEvidence = Boolean(item.evidence?.date || item.evidence?.type || item.evidence?.parties);
  return <aside ref={inspectorRef} className="inspector" aria-label="Review item" role={drawer ? 'dialog' : 'complementary'} aria-modal={drawer || undefined} onKeyDown={onKeyDown}>
    <div className="inspector-title"><h2>Review item</h2><button type="button" className="icon-button" onClick={onClose} aria-label="Close review"><Icon icon={X} /></button></div>
    <p className="selected-file">{item.originalFilename}</p>
    {editable && <><label>Filename<input ref={filenameRef} aria-label="Filename" value={filename} onChange={(event) => setFilename(event.target.value)} aria-invalid={Boolean(error)} /></label>{error && <p className="form-error" role="alert">{error}</p>}<label>Description<textarea value={description} onChange={(event) => setDescription(event.target.value)} /></label></>}
    {/*
      A renamed file becomes `completed`, which made `editable` false and took
      the description off screen with it. The sentence describing the document
      is the other half of what Intern produces, and it was unreachable the
      moment it was most useful. Read-only here because the proposal is settled,
      with a copy action so it can go somewhere else.
    */}
    {!editable && item.description && <section><h3>Description</h3><p className="settled-description">{item.description}</p>
      <button type="button" className="copy-description" onClick={() => void navigator.clipboard?.writeText(item.description ?? '')}><Icon icon={ClipboardCopy} />Copy description</button></section>}
    {hasEvidence && <section><h3>Evidence</h3><dl><dt>Date</dt><dd>{item.evidence?.date ?? '—'}</dd><dt>Type</dt><dd>{item.evidence?.type ?? '—'}</dd><dt>Parties</dt><dd>{item.evidence?.parties ?? '—'}</dd></dl></section>}
    {item.reason && <section><h3>{item.status === 'failed' ? 'Failure details' : 'Reason for review'}</h3><p>{item.reason}</p></section>}
    <div className="inspector-actions">
      {item.status === 'review' && <><button type="button" className="primary" disabled={busy} onClick={approve}><Icon icon={FileCheck2} />Approve & rename</button><button type="button" disabled={busy} onClick={onKeep}><Icon icon={FileText} />Keep original</button><button type="button" className="icon-button more-actions" disabled={busy} aria-label="More review actions" aria-expanded={moreOpen} onClick={() => setMoreOpen(!moreOpen)}><Icon icon={Ellipsis} /></button>{moreOpen && <div className="review-menu" role="group" aria-label="More review actions"><button type="button" disabled={busy} onClick={onRetry}><Icon icon={RotateCcw} />Retry</button><button type="button" disabled={busy} onClick={onRemove}><Icon icon={Trash2} />Remove</button></div>}</>}
      {item.status === 'ready' && <button type="button" className="primary" disabled={busy} onClick={approve}><Icon icon={FileCheck2} />Apply rename</button>}
      {item.status === 'processing' && item.cancelable !== false && <button type="button" disabled={busy} onClick={onCancel}><Icon icon={Ban} />Cancel processing</button>}
      {item.status === 'failed' && <><button type="button" disabled={busy} onClick={onRetry}><Icon icon={RotateCcw} />Retry item</button><button type="button" disabled={busy} onClick={onRemove}><Icon icon={Trash2} />Remove item</button></>}
      {item.status === 'completed' && item.undoable && <button type="button" disabled={busy} onClick={onUndo}><Icon icon={RotateCcw} />Undo</button>}
    </div>
  </aside>;
}
