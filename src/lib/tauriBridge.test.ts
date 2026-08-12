import { describe, expect, it, vi } from 'vitest';
import {
  TauriBridge,
  createTauriSelectionBoundary,
  type TauriEvent,
  type TauriTransport,
} from './tauriBridge';

function fakeTransport(responses: Record<string, unknown> = {}) {
  const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
  const listeners = new Map<string, (event: TauriEvent<unknown>) => void>();
  const unlisten = new Map<string, ReturnType<typeof vi.fn>>();
  const transport: TauriTransport = {
    invoke: async <T>(command: string, args?: Record<string, unknown>) => {
      calls.push({ command, args });
      return responses[command] as T;
    },
    listen: async <T>(event: string, handler: (event: TauriEvent<T>) => void) => {
      listeners.set(event, handler as (event: TauriEvent<unknown>) => void);
      const stop = vi.fn();
      unlisten.set(event, stop);
      return stop;
    },
  };
  return { transport, calls, listeners, unlisten };
}

describe('TauriBridge', () => {
  it('maps the exact narrow command names and JSON-safe payloads', async () => {
    const fake = fakeTransport({
      queue_list: [],
      settings_get: { destination: '', startMinimized: false, automaticRename: false },
      setup_get: { state: 'required', downloadedBytes: 0, totalBytes: 3278329184 },
    });
    const bridge = new TauriBridge(fake.transport);

    await bridge.listItems();
    await bridge.addFiles([{ path: 'C:\\Docs\\a.pdf', displayName: 'a.pdf' }]);
    await bridge.addFolder({ path: 'C:\\Docs', displayName: 'Docs' });
    await bridge.pauseQueue();
    await bridge.resumeQueue();
    await bridge.cancel('7');
    await bridge.retry('7');
    await bridge.remove('7');
    await bridge.approve('7', '2024-04-12 - Agreement.pdf', 'Agreement description.');
    await bridge.keepOriginal('7');
    await bridge.undo('7');
    await bridge.getSettings();
    await bridge.saveSettings({ destination: 'C:\\Output', startMinimized: false, automaticRename: true });
    await bridge.getSetup();
    await bridge.startModelDownload();
    await bridge.setupCancel();
    await bridge.setupChooseExisting({ modelPath: 'C:\\Models\\intern-q4.gguf', projectorPath: 'C:\\Models\\mmproj.gguf' });
    await bridge.clearHistory();

    expect(fake.calls).toEqual([
      { command: 'queue_list', args: undefined },
      { command: 'queue_add_files', args: { files: [{ path: 'C:\\Docs\\a.pdf', displayName: 'a.pdf' }] } },
      { command: 'queue_add_folder', args: { folder: { path: 'C:\\Docs', displayName: 'Docs' } } },
      { command: 'queue_pause', args: undefined },
      { command: 'queue_resume', args: undefined },
      { command: 'queue_cancel', args: { id: '7' } },
      { command: 'queue_retry', args: { id: '7' } },
      { command: 'queue_remove', args: { id: '7' } },
      { command: 'proposal_approve', args: { id: '7', filename: '2024-04-12 - Agreement.pdf', description: 'Agreement description.' } },
      { command: 'proposal_keep_original', args: { id: '7' } },
      { command: 'operation_undo', args: { id: '7' } },
      { command: 'settings_get', args: undefined },
      { command: 'settings_save', args: { settings: { destination: 'C:\\Output', startMinimized: false, automaticRename: true } } },
      { command: 'setup_get', args: undefined },
      { command: 'setup_start', args: undefined },
      { command: 'setup_cancel', args: undefined },
      { command: 'setup_choose_existing', args: { files: { modelPath: 'C:\\Models\\intern-q4.gguf', projectorPath: 'C:\\Models\\mmproj.gguf' } } },
      { command: 'history_clear', args: undefined },
    ]);
  });

  it('normalizes backend statuses and never exposes a proposal for waiting items', async () => {
    const fake = fakeTransport({
      queue_list: [
        { id: 1, originalFilename: 'queued.pdf', status: 'queued', proposedFilename: 'invented.pdf', confidence: 0.99 },
        { id: 2, originalFilename: 'extracting.pdf', status: 'extracting', progress: 25 },
        { id: 3, originalFilename: 'review.pdf', status: 'needs_review', proposedFilename: 'reviewed.pdf', confidence: 0.72 },
        { id: 4, originalFilename: 'done.pdf', status: 'completed', undoable: true },
        { id: 5, originalFilename: 'canceled.pdf', status: 'canceled', errorCode: 'CANCELED' },
        { id: 6, originalFilename: 'applying.pdf', status: 'applying', progress: 90 },
      ],
    });

    const items = await new TauriBridge(fake.transport).listItems();

    expect(items.map((item) => item.status)).toEqual(['waiting', 'processing', 'review', 'completed', 'failed', 'processing']);
    expect(items[0]).not.toHaveProperty('proposedFilename');
    expect(items[0]).not.toHaveProperty('confidence');
    expect(items[2].proposedFilename).toBe('reviewed.pdf');
    expect(items[1].cancelable).toBe(true);
    expect(items[5].cancelable).toBe(false);
  });

  it('normalizes queue events and unsubscribes every listener exactly once', async () => {
    const fake = fakeTransport();
    const bridge = new TauriBridge(fake.transport);
    const seen = vi.fn();
    const unsubscribe = await bridge.subscribeQueue(seen);

    fake.listeners.get('queue://changed')?.({ event: 'queue://changed', id: 1, payload: { paused: true } });
    fake.listeners.get('queue://progress')?.({ event: 'queue://progress', id: 2, payload: { itemId: 9, stage: 'extracting', current: 3, total: 12 } });

    expect(seen).toHaveBeenNthCalledWith(1, { type: 'changed', paused: true });
    expect(seen).toHaveBeenNthCalledWith(2, { type: 'progress', itemId: '9', stage: 'extracting', progress: 25 });
    unsubscribe();
    unsubscribe();
    expect(fake.unlisten.get('queue://changed')).toHaveBeenCalledTimes(1);
    expect(fake.unlisten.get('queue://progress')).toHaveBeenCalledTimes(1);
  });

  it('rolls back the changed listener when progress subscription fails', async () => {
    const stopChanged = vi.fn();
    const transport: TauriTransport = {
      invoke: async <T>() => undefined as T,
      listen: async (event) => {
        if (event === 'queue://changed') return stopChanged;
        throw new Error('progress listener failed');
      },
    };

    await expect(new TauriBridge(transport).subscribeQueue(vi.fn())).rejects.toThrow('progress listener failed');
    expect(stopChanged).toHaveBeenCalledOnce();
  });

  it('forwards setup progress and releases its listener', async () => {
    const fake = fakeTransport();
    const bridge = new TauriBridge(fake.transport);
    const seen = vi.fn();
    const unsubscribe = await bridge.subscribeSetup(seen);
    const state = { state: 'downloading' as const, downloadedBytes: 128, totalBytes: 512 };

    fake.listeners.get('setup://progress')?.({ event: 'setup://progress', id: 3, payload: state });

    expect(seen).toHaveBeenCalledWith(state);
    unsubscribe();
    unsubscribe();
    expect(fake.unlisten.get('setup://progress')).toHaveBeenCalledTimes(1);
  });

  it('keeps native path selection at the injected boundary', async () => {
    const fake = fakeTransport({
      'plugin:dialog|open': ['C:\\Docs\\One.pdf', 'C:\\Docs\\Two.docx'],
    });
    const selection = createTauriSelectionBoundary(fake.transport);

    expect(await selection.pickFiles()).toEqual([
      { path: 'C:\\Docs\\One.pdf', displayName: 'One.pdf' },
      { path: 'C:\\Docs\\Two.docx', displayName: 'Two.docx' },
    ]);
    fake.calls.length = 0;
    const dropped = await selection.resolveDrop({ paths: ['C:\\Docs\\Dropped.pdf'] });

    expect(dropped).toEqual({ files: [{ path: 'C:\\Docs\\Dropped.pdf', displayName: 'Dropped.pdf' }] });
    expect(fake.calls).toEqual([]);
  });

  it('selects native model and projector paths using two clearly labeled GGUF dialogs', async () => {
    const responses: unknown[] = ['C:\\Models\\intern-q8.gguf', 'C:\\Models\\mmproj-f16.gguf'];
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const transport: TauriTransport = {
      invoke: async <T>(command: string, args?: Record<string, unknown>) => {
        calls.push({ command, args });
        return responses.shift() as T;
      },
      listen: async () => () => undefined,
    };

    const files = await createTauriSelectionBoundary(transport).pickExistingModelFiles();

    expect(files).toEqual({ modelPath: 'C:\\Models\\intern-q8.gguf', projectorPath: 'C:\\Models\\mmproj-f16.gguf' });
    expect(calls).toEqual([
      { command: 'plugin:dialog|open', args: { options: {
        multiple: false,
        directory: false,
        title: 'Choose a Q4 or Q8 model GGUF',
        filters: [{ name: 'GGUF model files', extensions: ['gguf'] }],
      } } },
      { command: 'plugin:dialog|open', args: { options: {
        multiple: false,
        directory: false,
        title: 'Choose the matching mmproj GGUF',
        filters: [{ name: 'GGUF projector files', extensions: ['gguf'] }],
      } } },
    ]);
  });

  it('does not request a projector when model selection is canceled', async () => {
    const fake = fakeTransport({ 'plugin:dialog|open': null });
    const selection = createTauriSelectionBoundary(fake.transport);

    expect(await selection.pickExistingModelFiles()).toBeUndefined();
    expect(fake.calls).toHaveLength(1);
  });

  it('converts native drag-drop paths at the same selection boundary and unsubscribes', async () => {
    const fake = fakeTransport();
    const selection = createTauriSelectionBoundary(fake.transport);
    const seen = vi.fn();
    const unsubscribe = await selection.subscribeDrops(seen);

    fake.listeners.get('tauri://drag-drop')?.({
      event: 'tauri://drag-drop', id: 4,
      payload: { type: 'drop', paths: ['C:\\Docs\\Dropped.pdf', 'C:\\Docs\\Folder'] },
    });

    expect(seen).toHaveBeenCalledWith({ files: [
      { path: 'C:\\Docs\\Dropped.pdf', displayName: 'Dropped.pdf' },
      { path: 'C:\\Docs\\Folder', displayName: 'Folder' },
    ] });
    unsubscribe();
    expect(fake.unlisten.get('tauri://drag-drop')).toHaveBeenCalledTimes(1);
  });
});
