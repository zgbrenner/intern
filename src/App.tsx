import { useEffect, useRef, useState } from 'react';
import { AppHeader } from './components/AppHeader';
import { DropZone } from './components/DropZone';
import { HistoryDialog } from './components/HistoryDialog';
import { QueueTable } from './components/QueueTable';
import { ReviewInspector } from './components/ReviewInspector';
import { SettingsDialog } from './components/SettingsDialog';
import { SetupScreen } from './components/SetupScreen';
import { Sidebar } from './components/Sidebar';
import { ViewEmpty } from './components/ViewEmpty';
import { GUIDE_URL } from './lib/bridge';
import type { DesktopBridge, SelectionBoundary, SelectionResult } from './lib/bridge';
import { createInMemoryBridge } from './lib/inMemoryBridge';
import type { SetupEventSource } from './lib/tauriBridge';
import { useMediaQuery } from './lib/useMediaQuery';
import { useQueue } from './features/queue/useQueue';
import type { AppSettings, QueueItem, QueueView, SetupState } from './types';

export function App({ bridge: suppliedBridge, selection }: { bridge?: DesktopBridge; selection?: SelectionBoundary }) {
  const bridgeRef = useRef<DesktopBridge>(suppliedBridge ?? createInMemoryBridge());
  const seededSelection = useRef(false);
  const settingsTrigger = useRef<HTMLElement | null>(null);
  const historyTrigger = useRef<HTMLElement | null>(null);
  const reviewTrigger = useRef<{ element: HTMLButtonElement; itemId: string } | null>(null);
  const focusRestoreVersion = useRef(0);
  const bridge = suppliedBridge ?? bridgeRef.current;
  const { items, paused, setPaused, refresh, execute } = useQueue(bridge);
  const [view, setView] = useState<QueueView>('queue');
  const [selectedId, setSelectedId] = useState<string>();
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [settings, setSettings] = useState<AppSettings>({ destination: '', startMinimized: false, automaticRename: false, intakeFolder: '', intakeEnabled: false, processOthersUploads: false, machineLabel: '', runInBackground: false, startAtLogin: false, recordDescriptions: false });
  const [setup, setSetup] = useState<SetupState | undefined>(suppliedBridge ? undefined : { state: 'ready', downloadedBytes: 0, totalBytes: 0 });
  const [setupAction, setSetupAction] = useState<'start' | 'cancel' | 'choose'>();
  const [setupError, setSetupError] = useState('');
  const [actionPending, setActionPending] = useState(false);
  const [actionMessage, setActionMessage] = useState('');
  const [actionError, setActionError] = useState('');
  const narrowInspector = useMediaQuery('(max-width: 1100px)');

  useEffect(() => {
    void bridge.getSettings().then(setSettings);
    void bridge.getSetup().then(setSetup).catch((error) => setSetupError(describeSetupError(error)));
  }, [bridge]);
  useEffect(() => {
    const source = bridge as DesktopBridge & Partial<SetupEventSource>;
    if (!source.subscribeSetup) return;
    let active = true;
    let stop: (() => void) | undefined;
    void source.subscribeSetup((next) => { if (active) setSetup(next); }).then((unsubscribe) => {
      if (active) stop = unsubscribe;
      else unsubscribe();
    });
    return () => { active = false; stop?.(); };
  }, [bridge]);
  useEffect(() => {
    if (setup?.state !== 'downloading') return;
    let active = true;
    let timer: number | undefined;
    const poll = async () => {
      try {
        const next = await bridge.getSetup();
        if (!active) return;
        setSetup(next);
        if (next.state === 'downloading') timer = window.setTimeout(() => { void poll(); }, 250);
      } catch (error) {
        if (active) setSetupError(describeSetupError(error));
      }
    };
    timer = window.setTimeout(() => { void poll(); }, 250);
    return () => { active = false; if (timer !== undefined) window.clearTimeout(timer); };
  }, [bridge, setup?.state]);
  useEffect(() => {
    if (!seededSelection.current && items.length) {
      seededSelection.current = true;
      const firstReview = items.find((item) => item.status === 'review');
      if (firstReview) setSelectedId(firstReview.id);
    }
  }, [items]);
  const filtered = items.filter((item) => view === 'queue' ? item.status !== 'completed' : view === 'review' ? item.status === 'review' : item.status === 'completed');
  const selected = items.find((item) => item.id === selectedId);
  const drawerOpen = Boolean(selected && narrowInspector);
  const readyItems = items.filter((item) => item.status === 'ready' && item.proposedFilename);
  // Only items that have not started. Anything mid-flight, awaiting a decision,
  // or already renamed is deliberately out of reach of the discard action.
  const waitingItems = items.filter((item) => item.status === 'waiting');
  const queueStatus = queueStatusAnnouncement(items, paused);
  const select = (item: QueueItem, trigger: HTMLButtonElement) => { seededSelection.current = true; focusRestoreVersion.current += 1; reviewTrigger.current = { element: trigger, itemId: item.id }; setSelectedId(item.id); };
  const restoreQueueFocus = () => {
    const invocation = reviewTrigger.current;
    const version = ++focusRestoreVersion.current;
    reviewTrigger.current = null;
    queueMicrotask(() => {
      if (focusRestoreVersion.current !== version) return;
      const refreshedTrigger = [...document.querySelectorAll<HTMLButtonElement>('.row-select')]
        .find((button) => button.dataset.itemId === invocation?.itemId);
      // Prefer the primary action by name rather than "the first button in the
      // panel". That positional fallback silently moved the moment the toolbar
      // gained a second control, sending focus to a destructive Discard button
      // instead of Apply all ready.
      const target = invocation?.element.isConnected
        ? invocation.element
        : refreshedTrigger
          ?? document.querySelector<HTMLButtonElement>('.queue-panel .queue-actions button.primary')
          ?? document.querySelector<HTMLButtonElement>('.queue-panel button');
      target?.focus();
    });
  };
  const closeReview = () => {
    setSelectedId(undefined);
    restoreQueueFocus();
  };
  useEffect(() => {
    if (!selectedId || selected) return;
    setSelectedId(undefined);
    restoreQueueFocus();
  }, [selected, selectedId]);
  // Promise<unknown>: some commands report what they did - discardWaiting
  // resolves with a count - and the result is not needed here.
  const runQueueAction = async (run: () => Promise<unknown>, success: string) => {
    if (actionPending) return false;
    setActionPending(true);
    setActionError('');
    setActionMessage('');
    try {
      await run();
      await refresh();
      setActionMessage(success);
      return true;
    } catch (error) {
      try { await refresh(); } catch { /* Preserve the original command error. */ }
      setActionError(describeActionError(error));
      return false;
    } finally {
      setActionPending(false);
    }
  };
  const refreshAndClear = async (run: () => Promise<void>, success: string) => {
    const selectionVersion = focusRestoreVersion.current;
    if (!await runQueueAction(run, success)) return;
    if (focusRestoreVersion.current !== selectionVersion) return;
    setSelectedId(undefined);
    restoreQueueFocus();
  };
  const applyAllReady = async () => {
    if (actionPending) return;
    const selectionVersion = focusRestoreVersion.current;
    const selectedAtStart = selected;
    setActionPending(true);
    setActionError('');
    setActionMessage('');
    const failed = new Map<string, unknown>();
    let applied = 0;
    try {
      for (const item of readyItems) {
        try { await bridge.approve(item.id, item.proposedFilename!, item.description ?? ''); applied += 1; }
        catch (error) { failed.set(item.id, error); }
      }
      await refresh();
      if (failed.size) {
        const firstError = failed.values().next().value;
        setActionError(`${applied} ${applied === 1 ? 'rename' : 'renames'} applied. ${failed.size} could not be applied. ${describeActionError(firstError)}`);
      } else {
        setActionMessage(`${applied} ${applied === 1 ? 'rename' : 'renames'} applied.`);
      }
      if (focusRestoreVersion.current === selectionVersion && selectedAtStart?.status === 'ready' && !failed.has(selectedAtStart.id)) {
        setSelectedId(undefined);
        restoreQueueFocus();
      }
    } catch (error) {
      setActionError(`The queue could not refresh. ${describeActionError(error)}`);
    } finally {
      setActionPending(false);
    }
  };
  // Help leaves the app on purpose. Inside Tauri the webview has nowhere to
  // put a new tab, so the bridge hands the address to the system browser; if
  // that hand-off is refused the address itself is shown, because a person can
  // always type it.
  const openGuide = async () => {
    setActionError('');
    try { await bridge.openGuide(); }
    catch { setActionError(`The guide could not be opened. You can reach it at ${GUIDE_URL}.`); }
  };
  const openSettings = (trigger: HTMLButtonElement) => { focusRestoreVersion.current += 1; settingsTrigger.current = trigger; setSettingsOpen(true); };
  const closeSettings = () => { setSettingsOpen(false); settingsTrigger.current?.focus(); };
  const openHistory = (trigger: HTMLButtonElement) => { focusRestoreVersion.current += 1; historyTrigger.current = trigger; setHistoryOpen(true); };
  const closeHistory = () => { setHistoryOpen(false); historyTrigger.current?.focus(); };
  const applySelection = (result: SelectionResult) => {
    const focusAfter = async (run: () => Promise<void>, displayName: string) => {
      await run();
      const refreshed = await bridge.listItems();
      const target = [...refreshed].reverse().find((item) => item.originalFilename === displayName);
      if (target) { focusRestoreVersion.current += 1; reviewTrigger.current = null; setSelectedId(target.id); }
    };
    if (result.folder) {
      const focused = result.folder.files?.at(-1)?.displayName ?? `${result.folder.displayName}/`;
      void execute(() => focusAfter(() => bridge.addFolder(result.folder!), focused));
      return;
    }
    if (result.files?.length) {
      const focused = result.files[result.files.length - 1];
      void execute(() => focusAfter(() => bridge.addFiles(result.files!), focused.displayName));
    }
  };
  const runSetupAction = async (action: 'start' | 'cancel' | 'choose', run: () => Promise<boolean | void>) => {
    if (setupAction) return;
    setSetupAction(action);
    setSetupError('');
    try {
      if (await run() === false) return;
      setSetup(await bridge.getSetup());
    } catch (error) {
      setSetupError(describeSetupError(error));
    } finally {
      setSetupAction(undefined);
    }
  };
  const chooseExistingModel = () => void runSetupAction('choose', async () => {
    const files = await selection?.pickExistingModelFiles();
    if (!files) return false;
    await bridge.setupChooseExisting(files);
  });
  if (!setup || setup.state !== 'ready') return <SetupScreen
    setup={setup}
    busy={setupAction !== undefined}
    canChooseExisting={Boolean(selection)}
    operationError={setupError || (setup?.state === 'failed' ? describeSetupError(setup.error) : undefined)}
    onStart={() => void runSetupAction('start', () => bridge.startModelDownload())}
    onCancel={() => void runSetupAction('cancel', () => bridge.setupCancel())}
    onChooseExisting={chooseExistingModel}
  />;
  return <main className="app-shell" aria-label="Intern">
    <p className="sr-only" role="status" aria-label="Queue status" aria-live="polite" aria-atomic="true">{queueStatus}</p>
    <p className="sr-only" role="status" aria-label="Action status" aria-live="polite" aria-atomic="true">{actionMessage}</p>
    {actionError && <p className="operation-feedback" role="status" aria-label="Action error" aria-live="polite" aria-atomic="true">{actionError}</p>}
    <AppHeader inert={drawerOpen} busy={actionPending} paused={paused} onAddFiles={() => { void selection?.pickFiles().then((files) => applySelection({ files })); }} onAddFolder={() => { void selection?.pickFolder().then((folder) => { if (folder) applySelection({ folder }); }); }} onTogglePause={() => void (async () => { if (await runQueueAction(paused ? bridge.resumeQueue : bridge.pauseQueue, `Queue ${paused ? 'resumed' : 'paused'}.`)) setPaused(!paused); })()} />
    <Sidebar inert={drawerOpen} active={view} items={items} onChange={(next) => { focusRestoreVersion.current += 1; reviewTrigger.current = null; setView(next); setSelectedId(undefined); }} onSettings={openSettings} onHelp={() => void openGuide()} />
    <div className="workspace"><section className="queue-panel" aria-label="Queue items" inert={drawerOpen || undefined}>
      {/*
        An empty queue is the first thing a new user sees, and it used to be
        four column headings with nothing under them. The same drop target
        grows into the whole panel and says what to do with it.
      */}
      <DropZone variant={items.length === 0 ? 'hero' : 'bar'} onDrop={(payload) => { void selection?.resolveDrop(payload).then(applySelection); }} />
      {/*
        The way out of a folder chosen by mistake. Pointing the queue at a large
        directory used to be unrecoverable from inside the app: pausing stops it
        taking new work but leaves the backlog, Clear history only touches
        finished items, and dropping a waiting item was one click each. Four
        hundred items meant four hundred clicks, so the count is shown here to
        make the scale of what is being dropped explicit.
      */}
      {view === 'queue' && (readyItems.length > 0 || waitingItems.length > 0) && <div className="queue-actions">
        {waitingItems.length > 0 && <button type="button" aria-label="Discard waiting items" disabled={actionPending} onClick={() => void (async () => { const dropped = waitingItems.length; await runQueueAction(() => bridge.discardWaiting(), `Discarded ${dropped} waiting ${dropped === 1 ? 'item' : 'items'}.`); })()}>Discard waiting <span>{waitingItems.length}</span></button>}
        {readyItems.length > 0 && <button type="button" className="primary" aria-label="Apply all ready" disabled={actionPending} onClick={() => void applyAllReady()}>Apply all ready <span>{readyItems.length}</span></button>}
      </div>}
      {view === 'completed' && filtered.length > 0 && <div className="queue-actions">
        <button type="button" disabled={actionPending} onClick={(event) => openHistory(event.currentTarget)}>History</button>
        <button type="button" disabled={actionPending} onClick={() => void (async () => { if (await runQueueAction(() => bridge.clearHistory(), 'History cleared.')) queueMicrotask(() => document.querySelector<HTMLButtonElement>('.sidebar button[aria-label="Completed"]')?.focus()); })()}>Clear history</button>
      </div>}
      {items.length > 0 && (filtered.length > 0 ? <QueueTable items={filtered} selectedId={selectedId} onSelect={select} /> : <ViewEmpty view={view} />)}
      <p className="item-count">{filtered.length} {filtered.length === 1 ? 'item' : 'items'}</p></section>
      {selected && <ReviewInspector busy={actionPending} drawer={drawerOpen} item={selected} onClose={closeReview} onApprove={(filename, description) => void refreshAndClear(() => bridge.approve(selected.id, filename, description), 'Rename applied.')} onKeep={() => void refreshAndClear(() => bridge.keepOriginal(selected.id), 'Original filename kept.')} onCancel={() => void refreshAndClear(() => bridge.cancel(selected.id), 'Processing canceled.')} onRetry={() => void refreshAndClear(() => bridge.retry(selected.id), 'Item queued for retry.')} onRemove={() => void refreshAndClear(() => bridge.remove(selected.id), 'Item removed.')} onUndo={() => void refreshAndClear(() => bridge.undo(selected.id), 'Operation undone.')} />}
    </div>
    {historyOpen && <HistoryDialog bridge={bridge} selection={selection} onClose={closeHistory} />}
    {settingsOpen && <SettingsDialog settings={settings} bridge={bridge} selection={selection} onClose={closeSettings} onSave={async (next) => { await bridge.saveSettings(next); setSettings(next); closeSettings(); }} onCheckForUpdate={() => bridge.checkForUpdate()} onInstallUpdate={() => bridge.installUpdate()} />}
  </main>;
}

function describeActionError(error: unknown) {
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  if (typeof error === 'object' && error && 'message' in error && typeof error.message === 'string' && error.message.trim()) return error.message.trim();
  return 'The operation could not be completed.';
}

function describeSetupError(error: unknown) {
  const code = typeof error === 'string'
    ? error
    : typeof error === 'object' && error && 'code' in error && typeof error.code === 'string'
      ? error.code
      : undefined;
  switch (code) {
    case 'SETUP_BUSY': return 'Another model setup operation is already active. Wait for it to finish or cancel it. (SETUP_BUSY)';
    // These used to name files this build does not use: "the matching Q4 or Q8
    // model and mmproj GGUF files", and an "image self-test". Intern pins one
    // model file and has no vision projector, so the advice sent people looking
    // for something that does not exist. Name the file the manifest actually
    // pins instead.
    case 'MODEL_FILE_INVALID':
    case 'MODEL_MANIFEST_INVALID': return 'The selected file did not match the model Intern pins. Choose the exact Qwen3.5-2B-Q4_K_M.gguf file, or let Intern download it. (MODEL_FILE_INVALID)';
    case 'MODEL_SELF_TEST_FAILED': return 'Intern installed the model, but its local self-test failed. Try the download again or choose a verified model file. (MODEL_SELF_TEST_FAILED)';
    case 'INSUFFICIENT_DISK': return 'There is not enough free disk space to install the local model. Free space and try again. (INSUFFICIENT_DISK)';
  }
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  if (typeof error === 'object' && error && 'message' in error && typeof error.message === 'string' && error.message.trim()) return error.message.trim();
  if (typeof error === 'string' && error.trim()) return error.trim();
  return 'The local model could not be prepared.';
}

function queueStatusAnnouncement(items: QueueItem[], paused: boolean) {
  const labels: Array<[QueueItem['status'], string]> = [
    ['processing', 'processing'],
    ['ready', 'ready'],
    ['review', 'needs review'],
    ['waiting', 'waiting'],
    ['completed', 'completed'],
    ['failed', 'failed'],
  ];
  const counts = labels.flatMap(([status, label]) => {
    const count = items.filter((item) => item.status === status).length;
    return count ? [`${count} ${label}`] : [];
  });
  return `Queue ${paused ? 'paused' : 'active'}. ${counts.length ? counts.join(', ') : 'No items'}.`;
}
