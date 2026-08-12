import { CheckCircle2, CircleAlert, Clock3, LoaderCircle, XCircle } from 'lucide-react';
import { Icon } from './Icon';
import { statusLabel } from '../lib/format';
import type { QueueItem } from '../types';

const glyphs = { ready: CheckCircle2, review: CircleAlert, waiting: Clock3, processing: LoaderCircle, completed: CheckCircle2, failed: XCircle };
export function StatusCell({ item }: { item: QueueItem }) {
  return <span className={`status ${item.status}`}><Icon icon={glyphs[item.status]} />{statusLabel(item.status, item.progress)}</span>;
}
