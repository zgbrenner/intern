import { Ellipsis, FileCheck2, FileText, RotateCcw, Trash2, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { Icon } from './Icon';
import type { QueueItem } from '../types';

interface Props { item: QueueItem; onClose(): void; onApprove(filename: string, description: string): void; onKeep(): void; onRetry(): void; onRemove(): void; onUndo(): void }
export function ReviewInspector({ item, onClose, onApprove, onKeep, onRetry, onRemove, onUndo }: Props) {
  const [filename, setFilename] = useState(item.proposedFilename ?? '');
  const [description, setDescription] = useState(item.description ?? '');
  const [error, setError] = useState('');
  const [moreOpen, setMoreOpen] = useState(false);
  useEffect(() => { setFilename(item.proposedFilename ?? ''); setDescription(item.description ?? ''); setError(''); setMoreOpen(false); }, [item.id, item.proposalRevision]);
  const approve = () => { if (!filename.trim()) { setError('Filename is required'); return; } onApprove(filename.trim(), description); };
  return <aside className="inspector" aria-label="Review item" role="complementary">
    <div className="inspector-title"><h2>Review item</h2><button type="button" className="icon-button" onClick={onClose} aria-label="Close review"><Icon icon={X} /></button></div>
    <p className="selected-file">{item.originalFilename}</p>
    <label>Filename<input aria-label="Filename" value={filename} onChange={(event) => setFilename(event.target.value)} aria-invalid={Boolean(error)} /></label>{error && <p className="form-error" role="alert">{error}</p>}
    <label>Description<textarea value={description} onChange={(event) => setDescription(event.target.value)} /></label>
    <section><h3>Evidence</h3><dl><dt>Date</dt><dd>{item.evidence?.date ?? '—'}</dd><dt>Type</dt><dd>{item.evidence?.type ?? '—'}</dd><dt>Parties</dt><dd>{item.evidence?.parties ?? '—'}</dd></dl></section>
    {item.reason && <section><h3>Reason for review</h3><p>{item.reason}</p></section>}
    <div className="inspector-actions">
      {item.status === 'review' && <><button type="button" className="primary" onClick={approve}><Icon icon={FileCheck2} />Approve & rename</button><button type="button" onClick={onKeep}><Icon icon={FileText} />Keep original</button><button type="button" className="icon-button more-actions" aria-label="More review actions" aria-expanded={moreOpen} onClick={() => setMoreOpen(!moreOpen)}><Icon icon={Ellipsis} /></button>{moreOpen && <div className="review-menu" role="group" aria-label="More review actions"><button type="button" onClick={onRetry}><Icon icon={RotateCcw} />Retry</button><button type="button" onClick={onRemove}><Icon icon={Trash2} />Remove</button></div>}</>}
      {item.status === 'completed' && item.undoable && <button type="button" onClick={onUndo}><Icon icon={RotateCcw} />Undo</button>}
    </div>
  </aside>;
}
