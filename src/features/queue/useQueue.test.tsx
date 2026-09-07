import { StrictMode } from 'react';
import { act, renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { DesktopBridge } from '../../lib/bridge';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';
import type { QueueBridgeEvent } from '../../lib/tauriBridge';
import type { QueueItem } from '../../types';
import { useQueue } from './useQueue';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const item: QueueItem = { id: 'document', originalFilename: 'agreement.pdf', status: 'waiting' };

describe('queue synchronization', () => {
  it('ignores an older refresh that resolves after a newer one', async () => {
    const first = deferred<QueueItem[]>();
    const second = deferred<QueueItem[]>();
    const listItems = vi.fn().mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const bridge = { ...createInMemoryBridge(), listItems };
    const { result } = renderHook(() => useQueue(bridge));
    await act(async () => {
      const refresh = result.current.refresh();
      second.resolve([{ ...item, status: 'completed' }]);
      await refresh;
    });
    await act(async () => { first.resolve([item]); await first.promise; });
    expect(result.current.items[0].status).toBe('completed');
  });

  it('retains the last snapshot on failure and clears the error after retry', async () => {
    const base = createInMemoryBridge({ items: [item] });
    const listItems = vi.fn(base.listItems);
    const bridge = { ...base, listItems };
    const { result } = renderHook(() => useQueue(bridge));
    await waitFor(() => expect(result.current.items).toEqual([item]));
    const error = new Error('Queue database is busy.');
    listItems.mockRejectedValueOnce(error);
    await act(async () => { await expect(result.current.refresh()).rejects.toBe(error); });
    expect(result.current.items).toEqual([item]);
    expect(result.current.error).toEqual({ kind: 'snapshot', cause: error });
    await act(async () => { await result.current.refresh(); });
    expect(result.current.error).toBeNull();
  });

  it('does not hide a subscription failure when a snapshot succeeds', async () => {
    const error = new Error('Event connection failed.');
    const stop = vi.fn();
    const subscribeQueue = vi.fn().mockRejectedValueOnce(error).mockResolvedValue(stop);
    const bridge = { ...createInMemoryBridge({ items: [item] }), subscribeQueue };
    const { result, unmount } = renderHook(() => useQueue(bridge));
    await waitFor(() => expect(result.current.error?.kind).toBe('subscription'));
    await act(async () => { await result.current.refresh(); });
    expect(result.current.error?.cause).toBe(error);
    act(() => result.current.reconnect());
    await waitFor(() => expect(result.current.error).toBeNull());
    expect(subscribeQueue).toHaveBeenCalledTimes(2);
    unmount();
    expect(stop).toHaveBeenCalledOnce();
  });

  it('does not turn settled items back into processing on late progress', async () => {
    const statuses = ['waiting', 'processing', 'ready', 'review', 'completed', 'failed'] as const;
    const items = statuses.map((status) => ({ ...item, id: status, status }));
    let listener!: (event: QueueBridgeEvent) => void;
    const bridge = {
      ...createInMemoryBridge({ items }),
      subscribeQueue: async (next: typeof listener) => { listener = next; return () => {}; },
    };
    const { result } = renderHook(() => useQueue(bridge));
    await waitFor(() => expect(result.current.items).toHaveLength(statuses.length));
    act(() => {
      for (const status of statuses) listener({ type: 'progress', itemId: status, stage: 'analyzing', progress: 40 });
    });
    expect(result.current.items.map((entry) => entry.status)).toEqual(['processing', 'processing', 'ready', 'review', 'completed', 'failed']);
  });

  it('ignores events and results from a replaced bridge', async () => {
    let oldListener!: (event: QueueBridgeEvent) => void;
    const pending = deferred<QueueItem[]>();
    const oldBridge = {
      ...createInMemoryBridge(),
      listItems: vi.fn(() => pending.promise),
      subscribeQueue: async (next: typeof oldListener) => { oldListener = next; return () => {}; },
    };
    const nextBridge = { ...createInMemoryBridge({ items: [{ ...item, status: 'completed' }] }), subscribeQueue: async () => () => {} };
    const { result, rerender } = renderHook<ReturnType<typeof useQueue>, { bridge: DesktopBridge }>(({ bridge }) => useQueue(bridge), { initialProps: { bridge: oldBridge } });
    await waitFor(() => expect(oldListener).toBeTypeOf('function'));
    rerender({ bridge: nextBridge });
    await waitFor(() => expect(result.current.items[0]?.status).toBe('completed'));
    const requests = oldBridge.listItems.mock.calls.length;
    await act(async () => {
      oldListener({ type: 'changed', paused: true });
      oldListener({ type: 'progress', itemId: item.id, stage: 'analyzing', progress: 20 });
      pending.resolve([item]);
      await pending.promise;
    });
    expect(oldBridge.listItems).toHaveBeenCalledTimes(requests);
    expect(result.current.items[0].status).toBe('completed');
    expect(result.current.paused).toBe(false);
  });

  it('disposes late subscriptions across Strict Mode cleanup and unmount', async () => {
    const subscriptions: ReturnType<typeof deferred<() => void>>[] = [];
    const subscribeQueue = vi.fn(() => {
      const subscription = deferred<() => void>();
      subscriptions.push(subscription);
      return subscription.promise;
    });
    const bridge = { ...createInMemoryBridge({ items: [] }), subscribeQueue };
    const { unmount } = renderHook(() => useQueue(bridge), { wrapper: StrictMode });
    await waitFor(() => expect(subscriptions).toHaveLength(2));
    const firstStop = vi.fn();
    const secondStop = vi.fn();
    unmount();
    await act(async () => {
      subscriptions[0].resolve(firstStop);
      subscriptions[1].resolve(secondStop);
      await Promise.all(subscriptions.map((subscription) => subscription.promise));
    });
    expect(firstStop).toHaveBeenCalledOnce();
    expect(secondStop).toHaveBeenCalledOnce();
  });
});
