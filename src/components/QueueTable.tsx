import { FileText } from 'lucide-react';
import { confidence } from '../lib/format';
import type { QueueItem } from '../types';
import { Icon } from './Icon';
import { StatusCell } from './StatusCell';

export function QueueTable({ items, selectedId, onSelect }: { items: QueueItem[]; selectedId?: string; onSelect(item: QueueItem): void }) {
  return <div className="table-wrap"><table><thead><tr><th>Original filename</th><th>Status</th><th>Proposed filename</th><th>Confidence</th></tr></thead>
    <tbody>{items.map((item) => <tr key={item.id} className={selectedId === item.id ? 'selected' : ''} aria-selected={selectedId === item.id}>
      <td><button type="button" className="row-select" onClick={() => onSelect(item)} aria-label={`Select ${item.originalFilename}`}><Icon icon={FileText} /><span>{item.originalFilename}</span></button></td><td><StatusCell item={item} /></td><td>{item.status === 'waiting' ? '—' : item.proposedFilename ?? '—'}</td><td className={`confidence ${item.status}`}>{item.status === 'waiting' ? '—' : confidence(item.confidence)}</td>
    </tr>)}</tbody></table></div>;
}
