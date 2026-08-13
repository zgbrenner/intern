import { confidence } from '../lib/format';
import type { QueueItem } from '../types';
import { FileKindIcon } from './FileKindIcon';
import { StatusCell } from './StatusCell';

export function QueueTable({ items, selectedId, onSelect }: { items: QueueItem[]; selectedId?: string; onSelect(item: QueueItem, trigger: HTMLButtonElement): void }) {
  return <div className="table-wrap"><table><thead><tr><th>Original filename</th><th>Status</th><th>Proposed filename</th><th>Confidence</th></tr></thead>
    <tbody>{items.map((item) => <tr key={item.id} className={selectedId === item.id ? 'selected' : ''} aria-selected={selectedId === item.id}>
      <td><button type="button" className="row-select" data-item-id={item.id} onClick={(event) => onSelect(item, event.currentTarget)} aria-label={`Select ${item.originalFilename}`}><FileKindIcon filename={item.originalFilename} /><span>{item.originalFilename}</span></button></td><td><StatusCell item={item} /></td>
      {/*
        The description is half of what Intern produces and it used to be
        visible only inside the review panel, for items still awaiting a
        decision. Once a file was renamed the sentence vanished from the
        product entirely. Showing it under the proposed name costs no column
        width, which matters because the table already scrolls horizontally on
        a 1200px window.
      */}
      <td>{item.status === 'waiting' ? '—' : <>{item.proposedFilename ?? '—'}{item.description && <span className="row-description">{item.description}</span>}</>}</td><td className={`confidence ${item.status}`}>{item.status === 'waiting' ? '—' : confidence(item.confidence)}</td>
    </tr>)}</tbody></table></div>;
}
