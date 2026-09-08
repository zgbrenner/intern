import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import type { SelectionBoundary } from '../../lib/bridge';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';
import type { QueueItem } from '../../types';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}

function selection(overrides: Partial<SelectionBoundary> = {}): SelectionBoundary {
  return {
    pickFiles: async () => [],
    pickFolder: async () => undefined,
    pickExistingModelFiles: async () => undefined,
    resolveDrop: async () => ({}),
    ...overrides,
  };
}

const ready: QueueItem = {
  id: 'agreement', originalFilename: 'agreement.pdf', status: 'ready',
  proposedFilename: '2026-09-07 Agreement.pdf',
};

 describe('recoverable queue and import failures', () => {
  it('shows an initial queue failure and retries without restarting the app', async () => {
    const base = createInMemoryBridge({ items: [ready] });
    const listItems = vi.fn(base.listItems).mockRejectedValueOnce(new Error('Database is temporarily busy.'));
    render(<App bridge={{ ...base, listItems }} />);
    expect(await screen.findByRole('alert', { name: 'Queue connection error' })).toHaveTextContent('Database is temporarily busy.');
    fireEvent.click(screen.getByRole('button', { name: 'Retry queue connection' }));
    await screen.findByRole('button', { name: 'Select agreement.pdf' });
    expect(screen.queryByRole('alert', { name: 'Queue connection error' })).not.toBeInTheDocument();
  });

  it('reports a file picker rejection, including string errors from the bridge', async () => {
    const pickFiles = vi.fn().mockRejectedValueOnce('The file picker could not open.').mockResolvedValue([]);
    render(<App bridge={createInMemoryBridge({ items: [] })} selection={selection({ pickFiles })} />);
    fireEvent.click(await screen.findByRole('button', { name: /^Add files$/i }));
    expect(await screen.findByRole('status', { name: 'Action error' })).toHaveTextContent('The file picker could not open.');
    await waitFor(() => expect(screen.getByRole('button', { name: /^Add files$/i })).toBeEnabled());
    fireEvent.click(screen.getByRole('button', { name: /^Add files$/i }));
    await waitFor(() => expect(pickFiles).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByRole('status', { name: 'Action error' })).not.toBeInTheDocument());
  });

  it('reports folder picker errors', async () => {
    render(<App bridge={createInMemoryBridge({ items: [] })} selection={selection({ pickFolder: async () => { throw new Error('Folder access denied.'); } })} />);
    fireEvent.click(await screen.findByRole('button', { name: /^Add folder$/i }));
    expect(await screen.findByRole('status', { name: 'Action error' })).toHaveTextContent('Folder access denied.');
  });

  it('reports drop resolution errors without removing the existing queue', async () => {
    render(<App bridge={createInMemoryBridge({ items: [ready] })} selection={selection({ resolveDrop: async () => { throw new Error('Dropped file is unavailable.'); } })} />);
    fireEvent.drop(await screen.findByRole('region', { name: /drag files/i }));
    expect(await screen.findByRole('status', { name: 'Action error' })).toHaveTextContent('Dropped file is unavailable.');
    expect(screen.getByRole('button', { name: 'Select agreement.pdf' })).toBeVisible();
  });

  it('reports an import failure after a successful selection', async () => {
    const addFiles = vi.fn(async () => { throw new Error('The shared folder is offline.'); });
    render(<App bridge={{ ...createInMemoryBridge({ items: [ready] }), addFiles }} selection={selection({ pickFiles: async () => [{ path: 'browser://new.pdf', displayName: 'new.pdf' }] })} />);
    fireEvent.click(await screen.findByRole('button', { name: /^Add files$/i }));
    expect(await screen.findByRole('status', { name: 'Action error' })).toHaveTextContent('The shared folder is offline.');
    expect(screen.getByRole('button', { name: 'Select agreement.pdf' })).toBeVisible();
    expect(addFiles).toHaveBeenCalledOnce();
  });

  it('treats a canceled picker as a no-op, not a successful import or an error', async () => {
    const addFiles = vi.fn(async () => {});
    render(<App bridge={{ ...createInMemoryBridge({ items: [] }), addFiles }} selection={selection()} />);
    fireEvent.click(await screen.findByRole('button', { name: /^Add files$/i }));
    await waitFor(() => expect(screen.getByRole('button', { name: /^Add files$/i })).toBeEnabled());
    expect(addFiles).not.toHaveBeenCalled();
    expect(screen.queryByRole('status', { name: 'Action error' })).not.toBeInTheDocument();
    expect(screen.getByRole('status', { name: 'Action status' })).toBeEmptyDOMElement();
  });

  it('does not start overlapping drops, and tells the user to try the second drop again', async () => {
    const pending = deferred<{}>();
    const resolveDrop = vi.fn(() => pending.promise);
    render(<App bridge={createInMemoryBridge({ items: [] })} selection={selection({ resolveDrop })} />);
    const zone = await screen.findByRole('region', { name: /drag files/i });
    act(() => { fireEvent.drop(zone); fireEvent.drop(zone); });
    expect(resolveDrop).toHaveBeenCalledOnce();
    expect(screen.getByRole('status', { name: 'Action error' })).toHaveTextContent('Add these files again when it finishes.');
    await act(async () => { pending.resolve({}); await pending.promise; });
    await waitFor(() => expect(screen.getByRole('button', { name: /^Add files$/i })).toBeEnabled());
  });

  it('does not steal the reviewer selection when a slow import finishes', async () => {
    const pending = deferred<void>();
    const second = { ...ready, id: 'second', originalFilename: 'second.pdf' };
    const imported: QueueItem = { id: 'imported', originalFilename: 'new.pdf', status: 'waiting' };
    const items: QueueItem[] = [ready, second];
    const addFiles = vi.fn(async () => { await pending.promise; items.push(imported); });
    const bridge = { ...createInMemoryBridge({ items: [] }), listItems: async () => [...items], addFiles };
    render(<App bridge={bridge} selection={selection({ pickFiles: async () => [{ path: 'browser://new.pdf', displayName: 'new.pdf' }] })} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Select agreement.pdf' }));
    fireEvent.click(screen.getByRole('button', { name: /^Add files$/i }));
    await waitFor(() => expect(addFiles).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole('button', { name: 'Select second.pdf' }));
    await act(async () => { pending.resolve(); await pending.promise; });
    await screen.findByRole('button', { name: 'Select new.pdf' });
    expect(screen.getByRole('complementary', { name: 'Review item' })).toHaveTextContent('second.pdf');
  });

  it('gates same-tick rename submissions before React has rerendered', async () => {
    const pending = deferred<void>();
    const approve = vi.fn(() => pending.promise);
    render(<App bridge={{ ...createInMemoryBridge({ items: [ready] }), approve }} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Select agreement.pdf' }));
    const apply = screen.getByRole('button', { name: 'Apply rename' });
    act(() => { fireEvent.click(apply); fireEvent.click(apply); });
    expect(approve).toHaveBeenCalledOnce();
    await act(async () => { pending.resolve(); await pending.promise; });
  });
});
