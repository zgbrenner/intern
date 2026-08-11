import { useEffect, useRef, useState } from 'react';
import { AppHeader } from './components/AppHeader';
import { DropZone } from './components/DropZone';
import { QueueTable } from './components/QueueTable';
import { ReviewInspector } from './components/ReviewInspector';
import { SettingsDialog } from './components/SettingsDialog';
import { SetupScreen } from './components/SetupScreen';
import { Sidebar } from './components/Sidebar';
import type { DesktopBridge, SelectionBoundary, SelectionResult } from './lib/bridge';
import { createInMemoryBridge } from './lib/inMemoryBridge';
import type { SetupEventSource } from './lib/tauriBridge';
import { useQueue } from './features/queue/useQueue';
import type { AppSettings, QueueItem, QueueView, SetupState } from './types';

export function App({ bridge: suppliedBridge, selection }: { bridge?: DesktopBridge; selection?: SelectionBoundary }) {
  const bridgeRef = useRef<DesktopBridge>(suppliedBridge ?? createInMemoryBridge());
  const seededSelection = useRef(false);
  const settingsTrigger = useRef<HTMLElement | null>(null);
  const bridge = suppliedBridge ?? bridgeRef.current;
  const { items, paused, setPaused, refresh, execute } = useQueue(bridge);
  const [view, setView] = useState<QueueView>('queue');
  const [selectedId, setSelectedId] = useState<string>();
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settings, setSettings] = useState<AppSettings>({ destination: '', startMinimized: false, automaticRename: false });
  const [setup, setSetup] = useState<SetupState | undefined>(suppliedBridge ? undefined : { state: 'ready', downloadedBytes: 0, totalBytes: 0 });

  useEffect(() => { void bridge.getSettings().then(setSettings); void bridge.getSetup().then(setSetup); }, [bridge]);
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
      const next = await bridge.getSetup();
      if (!active) return;
      setSetup(next);
      if (next.state === 'downloading') timer = window.setTimeout(() => { void poll(); }, 250);
    };
    timer = window.setTimeout(() => { void poll(); }, 250);
    return () => { active = false; if (timer !== undefined) window.clearTimeout(timer); };
  }, [bridge, setup?.state]);
  useEffect(() => {
    if (!seededSelection.current && items.length) {
      seededSelection.current = true;
      setSelectedId(items.find((item) => item.status === 'review')?.id);
    }
  }, [items]);
  const filtered = items.filter((item) => view === 'queue' ? item.status !== 'completed' : view === 'review' ? item.status === 'review' : item.status === 'completed');
  const selected = items.find((item) => item.id === selectedId);
  const select = (item: QueueItem) => { setSelectedId(item.id); };
  const refreshAndClear = async (run: () => Promise<void>) => { await execute(run); setSelectedId(undefined); };
  const openSettings = (trigger: HTMLButtonElement) => { settingsTrigger.current = trigger; setSettingsOpen(true); };
  const closeSettings = () => { setSettingsOpen(false); settingsTrigger.current?.focus(); };
  const applySelection = (result: SelectionResult) => {
    const focusAfter = async (run: () => Promise<void>, displayName: string) => {
      await run();
      const refreshed = await bridge.listItems();
      const target = [...refreshed].reverse().find((item) => item.originalFilename === displayName);
      if (target) setSelectedId(target.id);
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
  if (!setup || setup.state !== 'ready') return <SetupScreen setup={setup} onStart={() => void (async () => { await bridge.startModelDownload(); setSetup(await bridge.getSetup()); })()} />;
  return <main className="app-shell" aria-label="Intern">
    <AppHeader paused={paused} onAddFiles={() => { void selection?.pickFiles().then((files) => applySelection({ files })); }} onAddFolder={() => { void selection?.pickFolder().then((folder) => { if (folder) applySelection({ folder }); }); }} onTogglePause={() => void (async () => { await execute(paused ? bridge.resumeQueue : bridge.pauseQueue); setPaused(!paused); })()} onSettings={openSettings} />
    <Sidebar active={view} items={items} onChange={(next) => { setView(next); setSelectedId(undefined); }} onSettings={openSettings} />
    <div className="workspace"><section className="queue-panel" aria-label="Queue items"><DropZone onDrop={(payload) => { void selection?.resolveDrop(payload).then(applySelection); }} /><QueueTable items={filtered} selectedId={selectedId} onSelect={select} /><p className="item-count">{filtered.length} items</p></section>
      {selected && <ReviewInspector item={selected} onClose={() => setSelectedId(undefined)} onApprove={(filename, description) => void refreshAndClear(() => bridge.approve(selected.id, filename, description))} onKeep={() => void refreshAndClear(() => bridge.keepOriginal(selected.id))} onRetry={() => void refreshAndClear(() => bridge.retry(selected.id))} onRemove={() => void refreshAndClear(() => bridge.remove(selected.id))} onUndo={() => void refreshAndClear(() => bridge.undo(selected.id))} />}
    </div>
    {settingsOpen && <SettingsDialog settings={settings} onClose={closeSettings} onSave={(next) => void (async () => { await bridge.saveSettings(next); setSettings(next); closeSettings(); })()} />}
  </main>;
}
