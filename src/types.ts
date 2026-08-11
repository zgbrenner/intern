export type QueueStatus = 'ready' | 'review' | 'processing' | 'waiting' | 'completed' | 'failed';
export type QueueView = 'queue' | 'review' | 'completed';

export interface QueueItem {
  id: string;
  originalFilename: string;
  status: QueueStatus;
  proposedFilename?: string;
  confidence?: number;
  description?: string;
  evidence?: { date?: string; type?: string; parties?: string };
  reason?: string;
  progress?: number;
  undoable?: boolean;
  proposalRevision?: string;
}

export interface AppSettings {
  destination: string;
  startMinimized: boolean;
  automaticRename: boolean;
}

export interface SetupState {
  state: 'ready' | 'required' | 'downloading' | 'failed';
  downloadedBytes: number;
  totalBytes: number;
  error?: string;
}
