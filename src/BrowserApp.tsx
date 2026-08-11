import { useEffect, useMemo, useRef } from 'react';
import { App } from './App';
import { createBrowserSelectionBoundary, createFixtureBatchBridge } from './lib/inMemoryBridge';
import { TauriBridge, createTauriSelectionBoundary, isTauriRuntime } from './lib/tauriBridge';

export function BrowserApp() {
  const fixtureBatch = new URLSearchParams(window.location.search).get('fixtureBatch') === '1';
  const bridge = useRef(fixtureBatch ? createFixtureBatchBridge() : undefined).current;
  if (isTauriRuntime()) {
    return <TauriApp />;
  }
  return <App bridge={bridge} selection={createBrowserSelectionBoundary()} />;
}

function TauriApp() {
  const bridge = useMemo(() => new TauriBridge(), []);
  const selection = useMemo(() => createTauriSelectionBoundary(), []);
  useEffect(() => {
    let active = true;
    let stop: (() => void) | undefined;
    void selection.subscribeDrops((result) => {
      if (!active) return;
      if (result.folder) void bridge.addFolder(result.folder);
      else if (result.files?.length) void bridge.addFiles(result.files);
    }).then((unsubscribe) => {
      if (active) stop = unsubscribe;
      else unsubscribe();
    });
    return () => { active = false; stop?.(); };
  }, [bridge, selection]);
  return <App bridge={bridge} selection={selection} />;
}
