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
