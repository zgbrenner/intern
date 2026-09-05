import type { AppSettings, BackfillResult, CloudLocation, CloudRoot, DescriptionsStatus, HistoryEntry, IntakeStatus, QueueItem, SetupState } from '../types';

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
  /**
   * Native "save as" for the history CSV. Resolves with the chosen path, or
   * undefined when the dialog is canceled. Optional: the browser boundary has
   * no native save dialog, and the in-memory export ignores the path anyway.
   */
  pickHistoryExportPath?(): Promise<string | undefined>;
}

/**
 * The published user guide. It lives here, on the bridge, rather than in a
 * component: the desktop build hands this exact string to the operating
 * system's browser, and the Tauri capability in
 * `src-tauri/capabilities/default.json` is scoped to this origin alone. One
 * constant keeps the two in step, and keeps every caller from being able to
 * ask the shell to open something else.
 */
export const GUIDE_URL = 'https://zgbrenner.github.io/intern/guide.html';

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
  /** Finished rename/undo operations, newest first (capped at 500). */
  historyList(): Promise<HistoryEntry[]>;
  /**
   * Write the rename history to `path` as CSV. Resolves with the number of
   * operations written. The path must be absolute in the desktop app; the
   * in-memory bridge ignores it.
   */
  historyExport(path: string): Promise<number>;
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
  /** Current shared-intake watcher status. Resolves with zeros when intake is disabled. */
  intakeStatus(): Promise<IntakeStatus>;
  /** Wake the intake watcher for an immediate scan. No-op when intake is disabled. */
  scanIntakeNow(): Promise<void>;
  /**
   * Say whether a folder lives inside a OneDrive/SharePoint sync root.
   *
   * Purely a local path lookup against the sync client's configuration - no
   * network request is made and nothing about the folder leaves the machine.
   */
  classifyFolder(path: string): Promise<CloudLocation | null>;
  /**
   * The OneDrive accounts and SharePoint libraries the sync client keeps on
   * this computer, so Settings can offer them instead of making a person hunt
   * for the folder under their profile. A local lookup of the sync client's
   * own configuration; no network request is made.
   */
  cloudRoots(): Promise<CloudRoot[]>;
  /** What the description records are doing: on or off, where, and the last failure. */
  descriptionsStatus(): Promise<DescriptionsStatus>;
  /**
   * Write a description record for every document already filed and not
   * undone, for a records folder switched on after the fact. Rejected with
   * DESCRIPTIONS_DISABLED until the setting is saved on.
   */
  descriptionsBackfill(): Promise<BackfillResult>;
  /**
   * Open the published guide (`GUIDE_URL`) in the user's own browser.
   *
   * Deliberately takes no URL. Inside Tauri a bare `<a target="_blank">` has
   * nowhere to go, so this has to reach the shell - and a method that accepted
   * any address would hand the webview a general-purpose "open anything"
   * capability for the sake of one help link.
   */
  openGuide(): Promise<void>;
}

/**
 * Optional capability, duck-typed like QueueEventSource: bridges that can push
 * intake status changes expose it; callers feature-detect `subscribeIntake`.
 */
export interface IntakeEventSource {
  subscribeIntake(handler: (status: IntakeStatus) => void): () => void;
}

/**
 * Optional capability, duck-typed like IntakeEventSource: bridges that can
 * push description-record status changes expose it.
 */
export interface DescriptionsEventSource {
  subscribeDescriptions(handler: (status: DescriptionsStatus) => void): () => void;
}

export type UpdateStatus =
  | { state: 'current'; currentVersion: string }
  | { state: 'available'; currentVersion: string; version: string; notes?: string; date?: string }
  /** Running outside the desktop shell, where there is nothing to update. */
  | { state: 'unsupported' };
