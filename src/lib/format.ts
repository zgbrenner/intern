import type { QueueStatus } from '../types';

export const confidence = (value?: number) => value === undefined ? '—' : `${Math.round(value * 100)}%`;
export const statusLabel = (status: QueueStatus, progress?: number) => status === 'review' ? 'Needs review' : status === 'processing' ? `Processing (${progress ?? 0}%)` : status[0].toUpperCase() + status.slice(1);
export const byteCount = (value: number) => new Intl.NumberFormat('en-US').format(value);

const MEBIBYTE = 1024 ** 2;
const GIBIBYTE = 1024 ** 3;
/// A download size a person can weigh a decision against, in the same binary
/// units the README and the release notes quote.
export const byteSize = (value: number) => value >= GIBIBYTE
  ? `${(value / GIBIBYTE).toFixed(2)} GiB`
  : `${Math.max(1, Math.round(value / MEBIBYTE))} MiB`;
