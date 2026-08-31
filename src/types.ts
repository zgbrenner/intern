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
  cancelable?: boolean;
  undoable?: boolean;
  proposalRevision?: string;
}

export interface AppSettings {
  destination: string;
  startMinimized: boolean;
  automaticRename: boolean;
  /** Watched intake folder path; "" = none configured. */
  intakeFolder: string;
  intakeEnabled: boolean;
  /** false = only process documents uploaded from this machine ("mine" scope). */
  processOthersUploads: boolean;
  /** Overrides the hostname shown to other machines; "" = use hostname. */
  machineLabel: string;
  /** Keep Intern in the system tray when the window is closed. */
  runInBackground: boolean;
  /** Start Intern automatically when the user signs in. */
  startAtLogin: boolean;
}

/** One finished rename/undo operation from the durable receipt journal. */
export interface HistoryEntry {
  receiptId: string;
  queueItemId: string;
  /** Unix seconds when the operation reached its terminal stage. */
  at: number;
  direction: 'apply' | 'undo';
  kind: 'rename' | 'verified_copy';
  /** Only terminal receipts are listed. */
  stage: 'complete' | 'rolled_back';
  originalPath: string;
  newPath: string;
}

/** A folder recognised as living inside a OneDrive/SharePoint sync root. */
export interface CloudLocation {
  provider: 'onedrive_personal' | 'onedrive_business' | 'sharepoint';
  displayName: string;
}

export interface IntakeMachine {
  machineId: string;
  machineName: string;
  userName: string;
  lastSeenAt: number;
  active: boolean;
}

export interface IntakeStatus {
  enabled: boolean;
  watching: boolean;
  folder: string;
  machineId: string;
  machineName: string;
  cloud: CloudLocation | null;
  machines: IntakeMachine[];
  heldForOthers: number;
  syncConflicts: number;
  awaitingHydration: number;
  claimedByOthers: number;
  processedHere: number;
  lastScanAt: number | null;
  error: string | null;
}

export interface SetupState {
  state: 'ready' | 'required' | 'downloading' | 'failed';
  downloadedBytes: number;
  totalBytes: number;
  error?: string;
}
