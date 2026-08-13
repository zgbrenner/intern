import type { AppSettings, QueueItem, SetupState } from '../types';

/** A JSON-safe local document reference that Task 6 can pass to Tauri. */
export interface FileSelection {
  path: string;
  displayName: string;
}

export interface FolderSelection {
  path: string;
  displayName: string;
  files?: FileSelection[];
}

export interface SelectionResult {
  files?: FileSelection[];
  folder?: FolderSelection;
}

/** Intern needs one file: the text model. There is no vision projector. */
export interface ExistingModelFiles {
  modelPath: string;
}

/** Platform-specific selection is injected; the Tauri bridge remains path-only. */
export interface SelectionBoundary {
  pickFiles(): Promise<FileSelection[]>;
  pickFolder(): Promise<FolderSelection | undefined>;
  pickExistingModelFiles(): Promise<ExistingModelFiles | undefined>;
  resolveDrop(payload: unknown): Promise<SelectionResult>;
}

export interface DesktopBridge {
  listItems(): Promise<QueueItem[]>;
  addFiles(files: FileSelection[]): Promise<void>;
  addFolder(folder: FolderSelection): Promise<void>;
  pauseQueue(): Promise<void>;
  resumeQueue(): Promise<void>;
  cancel(id: string): Promise<void>;
  approve(id: string, filename: string, description: string): Promise<void>;
  keepOriginal(id: string): Promise<void>;
  retry(id: string): Promise<void>;
  remove(id: string): Promise<void>;
  undo(id: string): Promise<void>;
  getSettings(): Promise<AppSettings>;
  saveSettings(settings: AppSettings): Promise<void>;
  getSetup(): Promise<SetupState>;
  startModelDownload(): Promise<void>;
  setupCancel(): Promise<void>;
  setupChooseExisting(files: ExistingModelFiles): Promise<void>;
  clearHistory(): Promise<void>;
  /**
   * Abandon every item still waiting, for a folder chosen by mistake.
   *
   * Resolves with how many were dropped. Items being processed, awaiting a
   * decision, or already renamed are untouched.
   */
  discardWaiting(): Promise<number>;
  /**
   * Ask GitHub whether a newer signed release exists.
   *
   * This is the only network request Intern makes apart from the one-off model
   * download, it happens only when someone presses the button in Settings, and
   * it sends nothing but a request for the release manifest. No filenames, no
   * document contents, no identifier of any kind.
   */
  checkForUpdate(): Promise<UpdateStatus>;
  /** Download and install the update found by the last check. */
  installUpdate(): Promise<void>;
}

export type UpdateStatus =
  | { state: 'current'; currentVersion: string }
  | { state: 'available'; currentVersion: string; version: string; notes?: string; date?: string }
  /** Running outside the desktop shell, where there is nothing to update. */
  | { state: 'unsupported' };
