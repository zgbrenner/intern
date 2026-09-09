import { useMemo, useRef } from 'react';
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
  // The window's drag-drop events are subscribed to by App, which routes them
  // through the same import path as the file pickers.
  const selection = useMemo(() => createTauriSelectionBoundary(), []);
  return <App bridge={bridge} selection={selection} />;
}
