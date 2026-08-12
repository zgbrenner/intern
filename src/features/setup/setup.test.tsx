import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import type { ExistingModelFiles, SelectionBoundary } from '../../lib/bridge';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';
import type { SetupState } from '../../types';

const modelFiles = { modelPath: 'C:\\Models\\intern-q4.gguf' };

function setupSelection(pickExistingModelFiles: () => Promise<ExistingModelFiles | undefined>): SelectionBoundary {
  return {
    pickFiles: async () => [],
    pickFolder: async () => undefined,
    pickExistingModelFiles,
    resolveDrop: async () => ({}),
  };
}

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

  it('explains the local privacy boundary and both setup sources', async () => {
    render(<App
      bridge={createInMemoryBridge({ setup: { state: 'required', downloadedBytes: 0, totalBytes: 3_278_329_184 } })}
      selection={setupSelection(async () => undefined)}
    />);

    expect(await screen.findByText(/approximately 3\.27 GB/i)).toBeVisible();
    expect(screen.getByText(/documents.*stay on this device/i)).toBeVisible();
    expect(screen.getByText(/processing runs fully locally/i)).toBeVisible();
    expect(screen.getByRole('button', { name: 'Download model' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Choose existing model files' })).toBeEnabled();
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

  it('cancels an active setup and offers resume without losing progress', async () => {
    let setup: SetupState = { state: 'downloading', downloadedBytes: 120, totalBytes: 300 };
    const base = createInMemoryBridge();
    const setupCancel = vi.fn(async () => {
      setup = { state: 'required', downloadedBytes: 120, totalBytes: 300, error: 'MODEL_DOWNLOAD_CANCELED' };
    });
    const bridge = { ...base, getSetup: async () => ({ ...setup }), setupCancel };
    render(<App bridge={bridge} selection={setupSelection(async () => modelFiles)} />);

    expect(await screen.findByRole('button', { name: 'Downloading model…' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Choose existing model files' })).toBeDisabled();
    const cancel = screen.getByRole('button', { name: 'Cancel setup' });
    expect(cancel).toBeEnabled();
    fireEvent.click(cancel);

    expect(await screen.findByRole('button', { name: 'Resume download' })).toBeEnabled();
    expect(screen.getByText('120 of 300 bytes')).toBeVisible();
    expect(screen.getByRole('status', { name: 'Setup status' })).toHaveTextContent(/progress was saved/i);
    expect(setupCancel).toHaveBeenCalledOnce();
  });

  it('passes only native model paths from the selection boundary to setup', async () => {
    let setup: SetupState = { state: 'required', downloadedBytes: 0, totalBytes: 300 };
    const base = createInMemoryBridge();
    const setupChooseExisting = vi.fn(async () => { setup = { ...setup, state: 'downloading' }; });
    const bridge = { ...base, getSetup: async () => ({ ...setup }), setupChooseExisting };
    render(<App bridge={bridge} selection={setupSelection(async () => modelFiles)} />);

    fireEvent.click(await screen.findByRole('button', { name: 'Choose existing model files' }));

    await waitFor(() => expect(setupChooseExisting).toHaveBeenCalledWith(modelFiles));
    expect(screen.getByRole('button', { name: 'Downloading model…' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Choose existing model files' })).toBeDisabled();
  });

  it('announces a busy setup command error and re-enables setup actions', async () => {
    const base = createInMemoryBridge({ setup: { state: 'required', downloadedBytes: 0, totalBytes: 300 } });
    const startModelDownload = vi.fn(async () => { throw { code: 'SETUP_BUSY', message: 'a model setup operation is already active' }; });
    render(<App bridge={{ ...base, startModelDownload }} selection={setupSelection(async () => modelFiles)} />);

    fireEvent.click(await screen.findByRole('button', { name: 'Download model' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(/another model setup operation is already active/i);
    expect(screen.getByRole('button', { name: 'Download model' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Choose existing model files' })).toBeEnabled();
  });

  it.each([
    ['MODEL_FILE_INVALID', /selected model files did not match/i],
    ['MODEL_SELF_TEST_FAILED', /local text and image self-test failed/i],
  ])('announces the %s setup failure with recovery guidance', async (error, message) => {
    render(<App bridge={createInMemoryBridge({ setup: { state: 'failed', downloadedBytes: 40, totalBytes: 300, error } })} selection={setupSelection(async () => modelFiles)} />);

    expect(await screen.findByRole('alert')).toHaveTextContent(message);
    expect(screen.getByRole('button', { name: 'Try download again' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Choose existing model files' })).toBeEnabled();
  });
});
