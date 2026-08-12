import { useCallback, useEffect, useState } from 'react';
import type { DesktopBridge } from '../../lib/bridge';
import type { QueueBridgeEvent, QueueEventSource } from '../../lib/tauriBridge';
import type { QueueItem } from '../../types';

export function useQueue(bridge: DesktopBridge) {
  const [items, setItems] = useState<QueueItem[]>([]);
  const [paused, setPaused] = useState(false);
  const refresh = useCallback(async () => setItems(await bridge.listItems()), [bridge]);
  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => {
    const source = bridge as DesktopBridge & Partial<QueueEventSource>;
    if (!source.subscribeQueue) return;
    let active = true;
    let stop: (() => void) | undefined;
    const onEvent = (event: QueueBridgeEvent) => {
      if (event.type === 'progress') {
        setItems((current) => current.map((item) => item.id === event.itemId
          ? { ...item, status: 'processing', ...(event.progress === undefined ? {} : { progress: event.progress }) }
          : item));
        return;
      }
      if (event.paused !== undefined) setPaused(event.paused);
      void refresh();
    };
    void source.subscribeQueue(onEvent).then((unsubscribe) => {
      if (active) stop = unsubscribe;
      else unsubscribe();
    });
    return () => { active = false; stop?.(); };
  }, [bridge, refresh]);
  const execute = useCallback(async (action: () => Promise<void>) => { await action(); await refresh(); }, [refresh]);
  return { items, paused, setPaused, refresh, execute };
}
