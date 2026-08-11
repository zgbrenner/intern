import type { QueueStatus } from '../types';

export const confidence = (value?: number) => value === undefined ? '—' : `${Math.round(value * 100)}%`;
export const statusLabel = (status: QueueStatus, progress?: number) => status === 'review' ? 'Needs review' : status === 'processing' ? `Processing (${progress ?? 0}%)` : status[0].toUpperCase() + status.slice(1);
export const byteCount = (value: number) => new Intl.NumberFormat('en-US').format(value);
