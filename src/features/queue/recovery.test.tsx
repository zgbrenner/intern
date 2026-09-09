import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import type { SelectionBoundary, SelectionResult } from '../../lib/bridge';
import { createBrowserSelectionBoundary, createInMemoryBridge } from '../../lib/inMemoryBridge';
import type { QueueBridgeEvent } from '../../lib/tauriBridge';
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

  // The queue pauses itself rather than failing a whole backlog one document
  // at a time, and it names the reason on the change event. Until that reason
  // was carried through the bridge the queue simply stopped with nothing on
  // screen to say why.
  it('says why the queue stopped, and stops saying it once the queue runs again', async () => {
    let listener!: (event: QueueBridgeEvent) => void;
    const bridge = {
      ...createInMemoryBridge({ items: [ready] }),
      subscribeQueue: async (next: typeof listener) => { listener = next; return () => {}; },
    };
    render(<App bridge={bridge} />);
    await screen.findByRole('button', { name: 'Select agreement.pdf' });

    act(() => listener({ type: 'changed', error: 'HOSTED_MODEL_UNAUTHORIZED' }));

    expect(await screen.findByRole('alert', { name: 'Queue stopped' })).toHaveTextContent('The hosted service rejected the API key.');

    act(() => listener({ type: 'changed', paused: false }));

    await waitFor(() => expect(screen.queryByRole('alert', { name: 'Queue stopped' })).not.toBeInTheDocument());
  });

  // Tauri's own drag-drop is enabled, so on the desktop a dropped file never
  // reaches the HTML5 handler the tests above drive: the paths arrive as a
  // window event. That path called the bridge directly, so a refusal was
  // swallowed and a drop during another action started a second import.
  it('reports a desktop drop the backend refuses', async () => {
    let drop!: (result: SelectionResult) => void;
    const addFiles = vi.fn(async () => { throw new Error('The shared folder is offline.'); });
    const dropping = selection({ subscribeDrops: async (listener: typeof drop) => { drop = listener; return () => {}; } } as Partial<SelectionBoundary>);
    render(<App bridge={{ ...createInMemoryBridge({ items: [ready] }), addFiles }} selection={dropping} />);
    await screen.findByRole('button', { name: 'Select agreement.pdf' });

    await act(async () => { drop({ files: [{ path: 'C:\Docs\dropped.pdf', displayName: 'dropped.pdf' }] }); });

    expect(await screen.findByRole('status', { name: 'Action error' })).toHaveTextContent('The shared folder is offline.');
    expect(addFiles).toHaveBeenCalledOnce();
  });

  // Browsers report a dismissed file dialog as a `cancel` event and nothing
  // else. Waiting only for `change` meant the promise never settled, and the
  // queue's one-action-at-a-time guard stayed closed for the rest of the
  // session: every later Add files, Add folder, or drop was turned away.
  it('releases the queue when the browser file picker is dismissed', async () => {
    const inputs: HTMLInputElement[] = [];
    const createElement = document.createElement.bind(document);
    vi.spyOn(document, 'createElement').mockImplementation((tag: string) => {
      const element = createElement(tag);
      if (tag === 'input') inputs.push(element as HTMLInputElement);
      return element;
    });
    render(<App bridge={createInMemoryBridge({ items: [] })} selection={createBrowserSelectionBoundary()} />);
    fireEvent.click(await screen.findByRole('button', { name: /^Add files$/i }));
    await waitFor(() => expect(inputs).toHaveLength(1));

    await act(async () => { inputs[0].dispatchEvent(new Event('cancel')); });

    await waitFor(() => expect(screen.getByRole('button', { name: /^Add files$/i })).toBeEnabled());
    fireEvent.click(screen.getByRole('button', { name: /^Add files$/i }));
    await waitFor(() => expect(inputs).toHaveLength(2));
  });

  // The rename has already happened by the time the queue is reread. Telling
  // the person it failed sends them to look for a file that is no longer under
  // its old name; the reread failing is what the queue connection banner is
  // for, and it says so itself.
  it('does not blame the command when only the reread afterwards fails', async () => {
    const base = createInMemoryBridge({ items: [ready] });
    const listItems = vi.fn(base.listItems);
    render(<App bridge={{ ...base, listItems }} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Select agreement.pdf' }));
    listItems.mockRejectedValueOnce(new Error('Queue database is busy.'));

    fireEvent.click(screen.getByRole('button', { name: 'Apply rename' }));

    await waitFor(() => expect(screen.getByRole('status', { name: 'Action status' })).toHaveTextContent('Rename applied.'));
    expect(screen.queryByRole('status', { name: 'Action error' })).not.toBeInTheDocument();
    expect(await screen.findByRole('alert', { name: 'Queue connection error' })).toHaveTextContent('Queue database is busy.');
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
