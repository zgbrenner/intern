import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { listen as tauriListen } from '@tauri-apps/api/event';
import type { AppSettings, BackfillResult, CloudLocation, CloudRoot, DescriptionsStatus, HistoryEntry, HostedModelStatus, HostedModelTestResult, IntakeStatus, QueueItem, SetupState } from '../types';
import { GUIDE_URL } from './bridge';
import type {
  DescriptionsEventSource,
  DesktopBridge,
  ExistingModelFiles,
  FileSelection,
  FolderSelection,
  IntakeEventSource,
  SelectionBoundary,
  SelectionResult,
  UpdateStatus,
} from './bridge';
import { humanizeReason } from './reasons';

/**
 * The update found by the last check, held so that installing it cannot race a
 * second lookup and install something other than what the user was shown.
 */
let pendingUpdate: { version: string; body?: string; date?: string; downloadAndInstall(): Promise<void> } | undefined;

export interface TauriEvent<T> {
  event: string;
  id: number;
  payload: T;
}

export interface TauriTransport {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, handler: (event: TauriEvent<T>) => void): Promise<() => void>;
}

const defaultTransport: TauriTransport = {
  invoke: (command, args) => tauriInvoke(command, args),
  listen: (event, handler) => tauriListen(event, handler),
};

type BackendStatus =
  | 'queued'
  | 'extracting'
  | 'analyzing'
  | 'ready'
  | 'needs_review'
  | 'failed'
  | 'canceled'
  | 'applying'
  | 'completed';

interface QueueItemDto {
  id: string | number;
  originalFilename: string;
  status: BackendStatus;
  proposedFilename?: string;
  confidence?: number;
  description?: string;
  evidence?: { date?: string; type?: string; parties?: string };
  reason?: string;
  errorCode?: string;
  progress?: number;
  undoable?: boolean;
  proposalRevision?: string | number;
  suggestedDate?: string;
}

interface HistoryEntryDto {
  receiptId: string | number;
  queueItemId: string | number;
  at: number;
  direction: HistoryEntry['direction'];
  kind: HistoryEntry['kind'];
  stage: HistoryEntry['stage'];
  originalPath: string;
  newPath: string;
  description?: string | null;
}

interface ChangedPayload { paused?: boolean }
interface ProgressPayload { itemId: string | number; stage: string; current: number; total?: number }

export type QueueBridgeEvent =
  | { type: 'changed'; paused?: boolean }
  | { type: 'progress'; itemId: string; stage: string; progress?: number };

export interface QueueEventSource {
  subscribeQueue(listener: (event: QueueBridgeEvent) => void): Promise<() => void>;
}

export interface SetupEventSource {
  subscribeSetup(listener: (state: SetupState) => void): Promise<() => void>;
}

export interface TauriSelectionBoundary extends SelectionBoundary {
  subscribeDrops(listener: (selection: SelectionResult) => void): Promise<() => void>;
}

export class TauriBridge implements DesktopBridge, QueueEventSource, SetupEventSource, IntakeEventSource, DescriptionsEventSource {
  constructor(private readonly transport: TauriTransport = defaultTransport) {}

  async listItems(): Promise<QueueItem[]> {
    const items = await this.transport.invoke<QueueItemDto[]>('queue_list');
    return items.map(normalizeItem);
  }

  addFiles(files: FileSelection[]): Promise<void> {
    return this.transport.invoke('queue_add_files', { files });
  }

  addFolder(folder: FolderSelection): Promise<void> {
    return this.transport.invoke('queue_add_folder', { folder });
  }

  pauseQueue(): Promise<void> { return this.transport.invoke('queue_pause'); }
  resumeQueue(): Promise<void> { return this.transport.invoke('queue_resume'); }
  cancel(id: string): Promise<void> { return this.transport.invoke('queue_cancel', { id }); }
  retry(id: string): Promise<void> { return this.transport.invoke('queue_retry', { id }); }
  remove(id: string): Promise<void> { return this.transport.invoke('queue_remove', { id }); }

  approve(id: string, filename: string, description: string): Promise<void> {
    return this.transport.invoke('proposal_approve', { id, filename, description });
  }

  keepOriginal(id: string): Promise<void> {
    return this.transport.invoke('proposal_keep_original', { id });
  }

  undo(id: string): Promise<void> { return this.transport.invoke('operation_undo', { id }); }
  getSettings(): Promise<AppSettings> { return this.transport.invoke('settings_get'); }

  saveSettings(settings: AppSettings): Promise<void> {
    return this.transport.invoke('settings_save', { settings });
  }

  getSetup(): Promise<SetupState> { return this.transport.invoke('setup_get'); }
  startModelDownload(): Promise<void> { return this.transport.invoke('setup_start'); }
  setupCancel(): Promise<void> { return this.transport.invoke('setup_cancel'); }
  setupChooseExisting(files: ExistingModelFiles): Promise<void> {
    return this.transport.invoke('setup_choose_existing', { files });
  }
  clearHistory(): Promise<void> { return this.transport.invoke('history_clear'); }

  async historyList(): Promise<HistoryEntry[]> {
    const entries = await this.transport.invoke<HistoryEntryDto[]>('history_list');
    return entries.map(({ description, ...entry }) => ({
      ...entry,
      receiptId: String(entry.receiptId),
      queueItemId: String(entry.queueItemId),
      ...(typeof description === 'string' && description.trim() ? { description } : {}),
    }));
  }

  historyExport(path: string): Promise<number> {
    return this.transport.invoke('history_export', { path });
  }

  discardWaiting(): Promise<number> { return this.transport.invoke('queue_discard_waiting'); }

  intakeStatus(): Promise<IntakeStatus> { return this.transport.invoke('intake_status'); }
  scanIntakeNow(): Promise<void> { return this.transport.invoke('intake_scan_now'); }

  async classifyFolder(path: string): Promise<CloudLocation | null> {
    // The DTO is camelCase end to end; only guard against an absent value so
    // callers can rely on `null` rather than `undefined`.
    return await this.transport.invoke<CloudLocation | null | undefined>('folder_classify', { path }) ?? null;
  }

  async cloudRoots(): Promise<CloudRoot[]> {
    return await this.transport.invoke<CloudRoot[] | null | undefined>('cloud_roots') ?? [];
  }

  descriptionsStatus(): Promise<DescriptionsStatus> { return this.transport.invoke('descriptions_status'); }
  descriptionsBackfill(): Promise<BackfillResult> { return this.transport.invoke('descriptions_backfill'); }
  hostedModelStatus(): Promise<HostedModelStatus> { return this.transport.invoke('hosted_model_status'); }
  hostedModelSetKey(key: string): Promise<void> { return this.transport.invoke('hosted_model_set_key', { key }); }
  hostedModelClearKey(): Promise<void> { return this.transport.invoke('hosted_model_clear_key'); }
  hostedModelTest(settings: AppSettings): Promise<HostedModelTestResult> { return this.transport.invoke('hosted_model_test', { settings }); }

  // Same shape as subscribeIntake: synchronous unsubscribe over an async
  // listen, dropping events until the listener is registered.
  subscribeDescriptions(handler: (status: DescriptionsStatus) => void): () => void {
    let active = true;
    let stop: (() => void) | undefined;
    void this.transport.listen<DescriptionsStatus>('descriptions://changed', ({ payload }) => {
      if (active) handler(payload);
    }).then((unlisten) => {
      if (active) stop = unlisten;
      else unlisten();
    }).catch(() => { /* No event stream in this runtime; callers fall back to asking. */ });
    return () => {
      if (!active) return;
      active = false;
      stop?.();
    };
  }

  /**
   * Hands the guide's address to the operating system's browser through the
   * opener plugin, the same way every other plugin command is reached here -
   * `transport.invoke`, no npm plugin package, so the browser build never
   * imports desktop-only code. The URL is the constant from the bridge
   * contract, which is also what the capability scope names, so this cannot
   * open anything else.
   */
  async openGuide(): Promise<void> {
    await this.transport.invoke('plugin:opener|open_url', { url: GUIDE_URL });
  }

  // The updater plugin is loaded lazily so that importing this module never
  // pulls in update machinery for a session that never asks for it, and so the
  // browser build - which has no Tauri runtime at all - does not fail to load.
  async checkForUpdate(): Promise<UpdateStatus> {
    const { check } = await import('@tauri-apps/plugin-updater');
    const { getVersion } = await import('@tauri-apps/api/app');
    const currentVersion = await getVersion();
    const update = await check();
    if (!update) return { state: 'current', currentVersion };
    pendingUpdate = update;
    return { state: 'available', currentVersion, version: update.version, notes: update.body, date: update.date };
  }

  async installUpdate(): Promise<void> {
    if (!pendingUpdate) throw new Error('No update has been found to install');
    // downloadAndInstall verifies the signature against the public key in
    // tauri.conf.json before it writes anything. An update signed by any other
    // key is rejected here, not after installation.
    // On Windows this hands off to the NSIS installer, which closes Intern to
    // replace it, so there is no relaunch call here to fail after the process
    // has already gone.
    await pendingUpdate.downloadAndInstall();
  }

  async subscribeQueue(listener: (event: QueueBridgeEvent) => void): Promise<() => void> {
    const changed = await this.transport.listen<ChangedPayload>('queue://changed', ({ payload }) => {
      listener({ type: 'changed', ...(typeof payload.paused === 'boolean' ? { paused: payload.paused } : {}) });
    });
    let progress: () => void;
    try {
      progress = await this.transport.listen<ProgressPayload>('queue://progress', ({ payload }) => {
        const progress = payload.total && payload.total > 0
          ? Math.max(0, Math.min(100, (payload.current / payload.total) * 100))
          : undefined;
        listener({
          type: 'progress',
          itemId: String(payload.itemId),
          stage: payload.stage,
          ...(progress === undefined ? {} : { progress }),
        });
      });
    } catch (error) {
      changed();
      throw error;
    }
    const stops = [changed, progress];
    let active = true;
    return () => {
      if (!active) return;
      active = false;
      stops.forEach((stop) => stop());
    };
  }

  // Synchronous unsubscribe per the IntakeEventSource contract: the Tauri
  // listen call is still async underneath, so events are dropped until it
  // resolves and the returned stop function tears down whichever state exists
  // when it is called - before or after the listener registered.
  subscribeIntake(handler: (status: IntakeStatus) => void): () => void {
    let active = true;
    let stop: (() => void) | undefined;
    void this.transport.listen<IntakeStatus>('intake://changed', ({ payload }) => {
      if (active) handler(payload);
    }).then((unlisten) => {
      if (active) stop = unlisten;
      else unlisten();
    }).catch(() => { /* No event stream in this runtime; callers fall back to polling intakeStatus. */ });
    return () => {
      if (!active) return;
      active = false;
      stop?.();
    };
  }

  async subscribeSetup(listener: (state: SetupState) => void): Promise<() => void> {
    const stop = await this.transport.listen<SetupState>('setup://progress', ({ payload }) => listener(payload));
    let active = true;
    return () => {
      if (!active) return;
      active = false;
      stop();
    };
  }
}

export function createTauriSelectionBoundary(transport: TauriTransport = defaultTransport): TauriSelectionBoundary {
  return {
    pickFiles: async () => selections(await openDialog(transport, false)),
    pickFolder: async () => {
      const paths = await openDialog(transport, true);
      const path = paths[0];
      return path ? { path, displayName: displayName(path) } : undefined;
    },
    pickExistingModelFiles: async () => {
      const modelPath = await openGgufDialog(transport, 'Choose the model GGUF', 'GGUF model files');
      return modelPath ? { modelPath } : undefined;
    },
    pickHistoryExportPath: async () => {
      const result = await transport.invoke<unknown>('plugin:dialog|save', {
        options: {
          title: 'Export rename history',
          defaultPath: 'intern-history.csv',
          filters: [{ name: 'CSV', extensions: ['csv'] }],
        },
      });
      return typeof result === 'string' ? result : undefined;
    },
    resolveDrop: async (payload: unknown): Promise<SelectionResult> => {
      const drop = payload as { paths?: unknown; kind?: unknown };
      const paths = stringPaths(drop?.paths);
      if (drop?.kind === 'folder' && paths[0]) {
        return { folder: { path: paths[0], displayName: displayName(paths[0]) } };
      }
      return { files: selections(paths) };
    },
    subscribeDrops: async (listener) => {
      const stop = await transport.listen<{ type?: string; paths?: unknown }>('tauri://drag-drop', ({ payload }) => {
        if (payload.type === 'drop') listener({ files: selections(stringPaths(payload.paths)) });
      });
      let active = true;
      return () => {
        if (!active) return;
        active = false;
        stop();
      };
    },
  };
}

export function isTauriRuntime(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

function normalizeItem(item: QueueItemDto): QueueItem {
  const status = normalizeStatus(item.status);
  const waiting = status === 'waiting';
  return {
    id: String(item.id),
    originalFilename: item.originalFilename,
    status,
    ...(waiting ? {} : {
      ...(item.proposedFilename === undefined ? {} : { proposedFilename: item.proposedFilename }),
      ...(item.confidence === undefined ? {} : { confidence: item.confidence }),
    }),
    ...(item.description === undefined ? {} : { description: item.description }),
    ...(item.evidence === undefined ? {} : { evidence: item.evidence }),
    ...(item.reason === undefined && item.errorCode === undefined
      ? {}
      : { reason: humanizeReason(item.reason ?? item.errorCode ?? '') }),
    ...(item.progress === undefined ? {} : { progress: item.progress }),
    ...(status === 'processing' ? { cancelable: item.status !== 'applying' } : {}),
    ...(item.undoable === undefined ? {} : { undoable: item.undoable }),
    ...(item.proposalRevision === undefined ? {} : { proposalRevision: String(item.proposalRevision) }),
    ...(item.suggestedDate === undefined ? {} : { suggestedDate: item.suggestedDate }),
  };
}

function normalizeStatus(status: BackendStatus): QueueItem['status'] {
  switch (status) {
    case 'queued': return 'waiting';
    case 'extracting':
    case 'analyzing':
    case 'applying': return 'processing';
    case 'ready': return 'ready';
    case 'needs_review': return 'review';
    case 'completed': return 'completed';
    case 'failed':
    case 'canceled': return 'failed';
  }
}

async function openDialog(transport: TauriTransport, directory: boolean): Promise<string[]> {
  const result = await transport.invoke<unknown>('plugin:dialog|open', {
    options: { multiple: !directory, directory },
  });
  return stringPaths(result);
}

async function openGgufDialog(transport: TauriTransport, title: string, filterName: string): Promise<string | undefined> {
  const result = await transport.invoke<unknown>('plugin:dialog|open', {
    options: {
      multiple: false,
      directory: false,
      title,
      filters: [{ name: filterName, extensions: ['gguf'] }],
    },
  });
  return stringPaths(result)[0];
}

function stringPaths(value: unknown): string[] {
  if (typeof value === 'string') return [value];
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is string => typeof entry === 'string');
}

function selections(paths: string[]): FileSelection[] {
  return paths.map((path) => ({ path, displayName: displayName(path) }));
}

function displayName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}
