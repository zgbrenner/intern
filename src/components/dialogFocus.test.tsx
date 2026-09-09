import { act, fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { App } from '../App';
import { createInMemoryBridge } from '../lib/inMemoryBridge';
import type { QueueBridgeEvent } from '../lib/tauriBridge';
import type { QueueItem } from '../types';

const processing: QueueItem = { id: 'document', originalFilename: 'agreement.pdf', status: 'processing', progress: 10 };
const filed: QueueItem = { id: 'filed', originalFilename: 'lease.pdf', status: 'completed', proposedFilename: '2026-01-02 Lease.pdf' };

function bridgeWithEvents(items: QueueItem[]) {
  let listener: ((event: QueueBridgeEvent) => void) | undefined;
  const bridge = {
    ...createInMemoryBridge({ items }),
    subscribeQueue: async (next: (event: QueueBridgeEvent) => void) => { listener = next; return () => { listener = undefined; }; },
  };
  return { bridge, emitProgress: () => act(() => listener?.({ type: 'progress', itemId: processing.id, stage: 'analyzing', progress: 60 })) };
}

/*
  A queue that is working emits a progress event every second or so, and each
  one rerenders App. Both dialogs used to focus their first control on every
  one of those rerenders, so a person typing an API key or a folder path had
  the caret pulled out of the field mid-word.
*/
describe('dialog focus while the queue is working', () => {
  it('leaves settings focus where the person put it when the queue reports progress', async () => {
    const { bridge, emitProgress } = bridgeWithEvents([processing]);
    render(<App bridge={bridge} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Settings' }));
    const machineName = await screen.findByLabelText('This machine\'s name');
    machineName.focus();
    fireEvent.change(machineName, { target: { value: 'Reception' } });

    emitProgress();

    expect(document.activeElement).toBe(machineName);
  });

  it('leaves history focus where the person put it when the queue reports progress', async () => {
    const { bridge, emitProgress } = bridgeWithEvents([processing, filed]);
    render(<App bridge={bridge} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Completed' }));
    fireEvent.click(await screen.findByRole('button', { name: 'History' }));
    const close = await screen.findByRole('button', { name: 'Close history' });
    close.focus();

    emitProgress();

    expect(document.activeElement).toBe(close);
  });
});
