import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';

describe('setup and queue controls', () => {
  it('shows an inert loading state while setup is still being read', () => {
    const baseBridge = createInMemoryBridge();
    const bridge = { ...baseBridge, getSetup: () => new Promise<never>(() => {}) };
    render(<App bridge={bridge} />);

    expect(screen.getByRole('status', { name: 'Loading setup' })).toBeVisible();
    expect(screen.queryByRole('button', { name: /Download model/i })).not.toBeInTheDocument();
  });

  it('reports exact local model download bytes', async () => {
    render(<App bridge={createInMemoryBridge({ setup: { state: 'downloading', downloadedBytes: 123_456_789, totalBytes: 3_221_225_472 } })} />);

    expect(await screen.findByText('123,456,789 of 3,221,225,472 bytes')).toBeVisible();
  });

  it('pauses and resumes processing progress', async () => {
    render(<App bridge={createInMemoryBridge()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Pause queue' }));
    expect(await screen.findByRole('button', { name: 'Resume queue' })).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: 'Resume queue' }));
    expect(await screen.findByText('Processing (0%)')).toBeVisible();
  });

  it('keeps the queue inaccessible after local setup failure', async () => {
    render(<App bridge={createInMemoryBridge({ setup: { state: 'failed', error: 'The local model could not be downloaded.' } })} />);

    expect(await screen.findByRole('main', { name: 'Intern setup' })).toBeVisible();
    expect(screen.queryByRole('navigation', { name: 'Queue navigation' })).not.toBeInTheDocument();
  });

  it('polls staged in-memory download progress and disables the download action', async () => {
    const bridge = createInMemoryBridge({ setup: { state: 'required', downloadedBytes: 0, totalBytes: 300 }, downloadStepBytes: 100, downloadIntervalMs: 400 });
    render(<App bridge={bridge} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Download model' }));

    expect(await screen.findByRole('button', { name: 'Downloading model…' })).toBeDisabled();
    expect(await screen.findByText('0 of 300 bytes')).toBeVisible();
    await waitFor(() => expect(screen.getByText('100 of 300 bytes')).toBeVisible(), { timeout: 1_200 });
  });
});
