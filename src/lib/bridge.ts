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

export interface ExistingModelFiles {
  modelPath: string;
  projectorPath: string;
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
}
