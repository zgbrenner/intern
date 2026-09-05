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
  /**
   * Write a description record beside every document filed into the
   * destination (`<destination>/.intern/descriptions/`), so a SharePoint
   * column can be filled from it. Needs a destination folder.
   */
  recordDescriptions: boolean;
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
  /** The one-sentence description applied with the rename, when the item still has it. */
  description?: string;
}

export type CloudProvider = 'onedrive_personal' | 'onedrive_business' | 'sharepoint' | 'network_share';

/**
 * A folder recognised as living inside a OneDrive/SharePoint sync root, or
 * reached over the network (a UNC path or a mapped drive).
 */
export interface CloudLocation {
  provider: CloudProvider;
  displayName: string;
}

/** One sync root the sync client keeps on this computer. */
export interface CloudRoot {
  provider: CloudProvider;
  displayName: string;
  path: string;
}

/** What the description records are doing. */
export interface DescriptionsStatus {
  /** The setting, as saved. */
  enabled: boolean;
  /** Where records go, or "" when no destination is configured. */
  folder: string;
  recordedThisSession: number;
  lastRecordedAt: number | null;
  /** The last write that failed, until the next success. */
  lastError: string | null;
}

export interface BackfillResult {
  written: number;
  failed: number;
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
  /** Subfolders the last scan could not read; the rest was still scanned. */
  unreadableFolders: number;
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
