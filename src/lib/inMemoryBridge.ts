import type { DesktopBridge, FileSelection, FolderSelection, SelectionBoundary, SelectionResult, UpdateStatus } from './bridge';
import type { AppSettings, CloudLocation, IntakeStatus, QueueItem, SetupState } from '../types';

/** Exact size of the single pinned model file this build downloads. */
export const PINNED_MODEL_BYTES = 1_280_835_840;

const seedItems: QueueItem[] = [
  { id: 'employment', originalFilename: 'Employment Agreement - John Smith.pdf', status: 'ready', proposedFilename: '2024-04-12 Employment Agreement with John Smith.pdf', confidence: 0.98 },
  { id: 'lease', originalFilename: 'Lease Agreement - 123 Main St.pdf', status: 'review', proposedFilename: '2023-09-15 Lease Agreement between ABC Properties LLC and TenantCo Inc.pdf', confidence: 0.72, description: 'Commercial lease agreement between landlord and tenant for 123 Main St.', evidence: { date: 'Sep 15, 2023', type: 'Lease Agreement', parties: 'ABC Properties LLC; TenantCo Inc.' }, reason: 'Lower confidence due to unclear document type keywords and multiple possible dates.' },
  { id: 'nda', originalFilename: 'NDA - Acme Corp.docx', status: 'ready', proposedFilename: '2024-03-01 Non-Disclosure Agreement with Acme Corp.docx', confidence: 0.95 },
  // Not a spreadsheet: .xlsx is not in SUPPORTED_EXTENSIONS, so a real one is
  // rejected with UNSUPPORTED_FORMAT and can never reach `processing`. The demo
  // queue showed it mid-run at 60% next to a drop zone whose own caption lists
  // the supported formats and omits spreadsheets.
  { id: 'financials', originalFilename: 'Q1 Financials.pdf', status: 'processing', proposedFilename: '2024-03-31 Q1 Financial Statements.pdf', progress: 60 },
  { id: 'service', originalFilename: 'Service Agreement - BlueSky LLC.pdf', status: 'ready', proposedFilename: '2024-02-28 Service Agreement with BlueSky LLC.pdf', confidence: 0.96 },
  { id: 'minutes', originalFilename: 'Board Meeting Minutes - May 7, 2024.docx', status: 'waiting' },
  { id: 'invoice', originalFilename: 'Invoice INV-1001.pdf', status: 'waiting' },
  { id: 'notes', originalFilename: 'Notes from Call - 2024-05-02.txt', status: 'waiting' },
  { id: 'completed', originalFilename: 'Completed lease.pdf', status: 'completed', proposedFilename: '2024-01-22 Lease Agreement.pdf', confidence: 0.93, undoable: true, description: 'Residential lease agreement for a twelve-month term beginning January 22, 2024.' },
];

export interface InMemoryBridgeOptions {
  items?: QueueItem[];
  setup?: Partial<SetupState>;
  downloadStepBytes?: number;
  downloadIntervalMs?: number;
  update?: UpdateStatus;
}

function itemFromFile(file: FileSelection, fixtureBatch = false): QueueItem {
  if (fixtureBatch) {
    if (file.displayName === 'duplicate-invoice-a.pdf') return {
      id: `file-${crypto.randomUUID()}`, originalFilename: file.displayName, status: 'review',
      proposedFilename: '2025-04-30 Invoice from Nimbus Orchard Supply Co.pdf', confidence: 0.82,
      description: 'Invoice INV-2048 dated April 30, 2025 for Atlas Threadworks LLC.',
      evidence: { date: 'Invoice date: April 30, 2025', type: 'INVOICE INV-2048', parties: 'Nimbus Orchard Supply Co.; Atlas Threadworks LLC' },
      reason: 'Needs review because the invoice and due dates are both present.',
    };
    if (file.displayName === 'duplicate-invoice-b.pdf') return {
      id: `file-${crypto.randomUUID()}`, originalFilename: file.displayName, status: 'review',
      proposedFilename: '2025-04-30 Invoice from Nimbus Orchard Supply Co.pdf', confidence: 0.82,
      description: 'Invoice INV-2048 dated April 30, 2025 for Atlas Threadworks LLC.',
      evidence: { date: 'Invoice date: April 30, 2025', type: 'INVOICE INV-2048', parties: 'Nimbus Orchard Supply Co.; Atlas Threadworks LLC' },
      reason: 'Identical content from a different path is retained as a separate review result.',
    };
    if (file.displayName === 'unsupported.csv') return { id: `file-${crypto.randomUUID()}`, originalFilename: file.displayName, status: 'failed', reason: 'Unsupported format skipped: .csv.' };
    if (file.displayName.startsWith('~$')) return { id: `file-${crypto.randomUUID()}`, originalFilename: file.displayName, status: 'failed', reason: 'Office lock file skipped.' };
  }
  return { id: `file-${crypto.randomUUID()}`, originalFilename: file.displayName, status: 'waiting' };
}

function createBridge(options: InMemoryBridgeOptions, fixtureBatch: boolean): DesktopBridge {
  let items = (options.items ?? seedItems).map((item) => ({ ...item }));
  let settings: AppSettings = { destination: '', startMinimized: false, automaticRename: false, intakeFolder: '', intakeEnabled: false, processOthersUploads: false, machineLabel: '' };
  // Deterministic classification so browser dev and e2e runs can exercise the
  // cloud badge without a real sync client: the path only has to mention the
  // provider. Mirrors the DTO the desktop backend returns from folder_classify.
  const classifyPath = async (path: string): Promise<CloudLocation | null> => {
    const lower = path.toLowerCase();
    if (lower.includes('onedrive')) return { provider: 'onedrive_business', displayName: 'OneDrive – Contoso' };
    if (lower.includes('sharepoint')) return { provider: 'sharepoint', displayName: 'Contoso' };
    return null;
  };
  // The pinned model's exact size from src-tauri/resources/model-manifest.json.
  // The previous value, 3_278_329_184, was a model plus a vision projector that
  // this pipeline does not download.
  let setup: SetupState = { state: 'ready', downloadedBytes: PINNED_MODEL_BYTES, totalBytes: PINNED_MODEL_BYTES, ...options.setup };
  const downloadStepBytes = options.downloadStepBytes ?? Math.max(1, Math.ceil(setup.totalBytes / 4));
  const downloadIntervalMs = options.downloadIntervalMs ?? 40;
  let downloadTimer: ReturnType<typeof setInterval> | undefined;
  const idByPath = new Map<string, string>();
  const pathById = new Map<string, string>();
  const update = (id: string, change: Partial<QueueItem>) => { items = items.map((item) => item.id === id ? { ...item, ...change } : item); };
  const finishDownload = () => { if (downloadTimer) clearInterval(downloadTimer); downloadTimer = undefined; setup = { ...setup, state: 'ready', downloadedBytes: setup.totalBytes }; };
  const addFolder = (folder: FolderSelection) => {
    const sourceFiles = folder.files ?? [];
    const folderItems = sourceFiles.flatMap((file) => {
      if (idByPath.has(file.path)) return [];
      const item = itemFromFile(file, fixtureBatch);
      idByPath.set(file.path, item.id);
      pathById.set(item.id, file.path);
      return [item];
    });
    if (sourceFiles.length) { items = [...items, ...folderItems]; return; }
    if (idByPath.has(folder.path)) return;
    const item = { id: `folder-${crypto.randomUUID()}`, originalFilename: `${folder.displayName}/`, status: 'waiting' as const };
    idByPath.set(folder.path, item.id);
    pathById.set(item.id, folder.path);
    items = [...items, item];
  };
  return {
    listItems: async () => items.map((item) => ({ ...item })),
    addFiles: async (files) => {
      for (const file of files) {
        if (idByPath.has(file.path)) continue;
        const item = itemFromFile(file, fixtureBatch);
        idByPath.set(file.path, item.id);
        pathById.set(item.id, file.path);
        items = [...items, item];
      }
    },
    addFolder: async (folder) => addFolder(folder),
    pauseQueue: async () => { items = items.map((item) => item.status === 'processing' ? { ...item, status: 'waiting' as const } : item); },
    resumeQueue: async () => { const item = items.find((entry) => entry.status === 'waiting'); if (item) update(item.id, { status: 'processing', progress: 0 }); },
    cancel: async (id) => update(id, { status: 'failed', progress: undefined, reason: 'Canceled.' }),
    approve: async (id, filename, description) => update(id, { status: 'completed', proposedFilename: filename, description, undoable: true }),
    keepOriginal: async (id) => update(id, { status: 'completed', proposedFilename: undefined, undoable: true }),
    retry: async (id) => update(id, { status: 'waiting', progress: undefined }),
    remove: async (id) => {
      const path = pathById.get(id);
      if (path) idByPath.delete(path);
      pathById.delete(id);
      items = items.filter((item) => item.id !== id);
    },
    undo: async (id) => update(id, { status: 'review', undoable: false }),
    getSettings: async () => ({ ...settings }),
    saveSettings: async (next) => { settings = { ...next }; },
    getSetup: async () => ({ ...setup }),
    startModelDownload: async () => {
      if (setup.state === 'downloading') return;
      setup = { ...setup, state: 'downloading', error: undefined };
      downloadTimer = setInterval(() => {
        const downloadedBytes = Math.min(setup.totalBytes, setup.downloadedBytes + downloadStepBytes);
        setup = { ...setup, downloadedBytes };
        if (downloadedBytes >= setup.totalBytes) finishDownload();
      }, downloadIntervalMs);
    },
    setupCancel: async () => {
      if (downloadTimer) clearInterval(downloadTimer);
      downloadTimer = undefined;
      setup = { ...setup, state: 'required', error: 'MODEL_DOWNLOAD_CANCELED' };
    },
    setupChooseExisting: async () => {
      if (setup.state === 'downloading') throw { code: 'SETUP_BUSY', message: 'a model setup operation is already active' };
      setup = { ...setup, state: 'ready', downloadedBytes: setup.totalBytes, error: undefined };
    },
    clearHistory: async () => { items = items.filter((item) => item.status !== 'completed' && item.status !== 'failed'); },
    discardWaiting: async () => {
      const waiting = items.filter((item) => item.status === 'waiting');
      items = items.filter((item) => item.status !== 'waiting');
      return waiting.length;
    },
    // The browser build has no Tauri runtime and nothing to replace, so it says
    // so rather than pretending to be up to date.
    checkForUpdate: async () => options.update ?? { state: 'unsupported' },
    installUpdate: async () => { throw new Error('Updates are only available in the desktop application'); },
    // A coherent fake mirroring the current in-memory settings: enabled follows
    // intakeEnabled, and the counts are small fixed numbers so the dialog's
    // status block renders deterministically in dev and tests.
    intakeStatus: async (): Promise<IntakeStatus> => {
      const enabled = settings.intakeEnabled;
      const now = Math.floor(Date.now() / 1000);
      const machineName = settings.machineLabel.trim() || 'This machine';
      return {
        enabled,
        watching: enabled,
        folder: settings.intakeFolder,
        machineId: 'dev-machine',
        machineName,
        cloud: await classifyPath(settings.intakeFolder),
        machines: enabled ? [
          { machineId: 'dev-machine', machineName, userName: 'dev', lastSeenAt: now, active: true },
          { machineId: 'demo-peer', machineName: 'Front desk PC', userName: 'colleague', lastSeenAt: now - 90, active: true },
        ] : [],
        heldForOthers: enabled ? 2 : 0,
        claimedByOthers: enabled ? 1 : 0,
        processedHere: enabled ? 3 : 0,
        lastScanAt: enabled ? now - 5 : null,
        error: null,
      };
    },
    scanIntakeNow: async () => { /* Nothing is watching in the browser; the desktop backend wakes its scan loop. */ },
    classifyFolder: (path) => classifyPath(path),
  };
}

export function createInMemoryBridge(options: InMemoryBridgeOptions = {}): DesktopBridge {
  return createBridge(options, false);
}

export function createFixtureBatchBridge(): DesktopBridge {
  return createBridge({ items: [] }, true);
}

type BrowserFile = File & { webkitRelativePath?: string };

function browserFileSelection(file: BrowserFile): FileSelection {
  const displayName = file.webkitRelativePath || file.name;
  return { path: `browser://${displayName}`, displayName };
}

function chooseBrowserFiles(directory: boolean): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement('input');
    input.type = 'file';
    input.multiple = true;
    if (directory) input.setAttribute('webkitdirectory', '');
    input.addEventListener('change', () => { const files = Array.from(input.files ?? []); input.remove(); resolve(files); }, { once: true });
    input.click();
  });
}

function browserFolderSelection(files: File[]): FolderSelection | undefined {
  if (!files.length) return undefined;
  const first = files[0] as BrowserFile;
  const displayName = first.webkitRelativePath?.split('/')[0] || first.name;
  return { path: `browser://${displayName}`, displayName, files: files.map((file) => browserFileSelection(file as BrowserFile)) };
}

/** Development-only conversion of browser File/DataTransfer objects into JSON-safe references. */
export function createBrowserSelectionBoundary(): SelectionBoundary {
  return {
    pickFiles: async () => (await chooseBrowserFiles(false)).map((file) => browserFileSelection(file as BrowserFile)),
    pickFolder: async () => browserFolderSelection(await chooseBrowserFiles(true)),
    pickExistingModelFiles: async () => undefined,
    resolveDrop: async (payload: unknown): Promise<SelectionResult> => {
      const transfer = payload as DataTransfer;
      const files = Array.from(transfer.files ?? []);
      const item = transfer.items?.[0] as (DataTransferItem & { getAsFileSystemHandle?: () => Promise<{ kind: string; name: string }> }) | undefined;
      const handle = await item?.getAsFileSystemHandle?.();
      if (handle?.kind === 'directory') return { folder: { path: `browser://${handle.name}`, displayName: handle.name, files: files.map((file) => browserFileSelection(file as BrowserFile)) } };
      return { files: files.map((file) => browserFileSelection(file as BrowserFile)) };
    },
  };
}
