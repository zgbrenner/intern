import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import type { ExistingModelFiles, SelectionBoundary } from '../../lib/bridge';
import { PINNED_MODEL_BYTES, createInMemoryBridge } from '../../lib/inMemoryBridge';
import type { SetupState } from '../../types';
// The manifest the installer ships and the backend reads. Importing the real
// file is the point: it is what makes the assertion below a drift guard rather
// than a second copy of the same number.
import modelManifest from '../../../src-tauri/resources/model-manifest.json';

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

  it('quotes the download size the manifest will actually fetch, not a hardcoded one', async () => {
    const total = modelManifest.files.reduce((sum, file) => sum + file.size, 0);

    // The setup screen is the first thing a new user sees, and it used to
    // announce a hardcoded "approximately 3.27 GB" - a model plus a vision
    // projector from an earlier design - for a download that is one 1.19 GiB
    // file. Both halves are pinned here: the demo total must equal the shipped
    // manifest, and the screen must render whatever total it is given.
    expect(PINNED_MODEL_BYTES).toBe(total);

    render(<App
      bridge={createInMemoryBridge({ setup: { state: 'required', downloadedBytes: 0, totalBytes: total } })}
      selection={setupSelection(async () => undefined)}
    />);

    expect(await screen.findByText(/Download 1\.19 GiB of model files/i)).toBeVisible();
    expect(screen.queryByText(/3\.27 GB/i)).not.toBeInTheDocument();
  });

  it('explains the local privacy boundary and both setup sources', async () => {
    render(<App
      bridge={createInMemoryBridge({ setup: { state: 'required', downloadedBytes: 0, totalBytes: PINNED_MODEL_BYTES } })}
      selection={setupSelection(async () => undefined)}
    />);

    expect(await screen.findByRole('heading', { name: 'Set up Intern' })).toBeVisible();
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

  describe('updates', () => {
    const openSettings = async () => {
      const trigger = (await screen.findAllByRole('button', { name: 'Settings' }))[0];
      fireEvent.click(trigger);
    };

    it('checks only when asked, and never on its own', async () => {
      const checkForUpdate = vi.fn(async () => ({ state: 'current' as const, currentVersion: '0.1.0-alpha.1' }));
      render(<App bridge={{ ...createInMemoryBridge(), checkForUpdate }} />);
      await openSettings();

      // The whole point of a button: opening Settings must not reach the network.
      expect(checkForUpdate).not.toHaveBeenCalled();

      fireEvent.click(screen.getByRole('button', { name: 'Check for updates' }));
      await waitFor(() => expect(screen.getByRole('status', { name: 'Update status' })).toHaveTextContent(/0\.1\.0-alpha\.1 is the latest release/i));
      expect(checkForUpdate).toHaveBeenCalledTimes(1);
    });

    it('offers to install a newer version and names both versions', async () => {
      const bridge = createInMemoryBridge({ update: { state: 'available', currentVersion: '0.1.0-alpha.1', version: '0.2.0' } });
      const installUpdate = vi.fn(async () => {});
      render(<App bridge={{ ...bridge, installUpdate }} />);
      await openSettings();
      fireEvent.click(screen.getByRole('button', { name: 'Check for updates' }));

      await waitFor(() => expect(screen.getByRole('status', { name: 'Update status' })).toHaveTextContent(/Version 0\.2\.0 is available\. You have 0\.1\.0-alpha\.1/i));
      fireEvent.click(screen.getByRole('button', { name: /Install 0\.2\.0 and restart/i }));
      await waitFor(() => expect(installUpdate).toHaveBeenCalledTimes(1));
    });

    // The reason the whole signing apparatus exists. If this message ever
    // degrades into a generic failure, a user cannot tell "GitHub is down" from
    // "something served me code this build refuses to trust".
    it('says plainly when an update fails its signature check', async () => {
      const checkForUpdate = vi.fn(async () => { throw new Error('signature verification failed'); });
      render(<App bridge={{ ...createInMemoryBridge(), checkForUpdate }} />);
      await openSettings();
      fireEvent.click(screen.getByRole('button', { name: 'Check for updates' }));

      const alert = await screen.findByRole('alert');
      expect(alert).toHaveTextContent(/was not signed by this project's key, so Intern refused it/i);
    });

    it('reports an unreachable endpoint as a network problem, not a rejection', async () => {
      const checkForUpdate = vi.fn(async () => { throw new Error(''); });
      render(<App bridge={{ ...createInMemoryBridge(), checkForUpdate }} />);
      await openSettings();
      fireEvent.click(screen.getByRole('button', { name: 'Check for updates' }));

      const alert = await screen.findByRole('alert');
      expect(alert).toHaveTextContent(/Could not reach GitHub/i);
      expect(alert).not.toHaveTextContent(/signed/i);
    });
  });

  it.each([
    ['MODEL_FILE_INVALID', /did not match the model Intern pins/i],
    ['MODEL_SELF_TEST_FAILED', /local self-test failed/i],
  ])('announces the %s setup failure with recovery guidance', async (error, message) => {
    render(<App bridge={createInMemoryBridge({ setup: { state: 'failed', downloadedBytes: 40, totalBytes: 300, error } })} selection={setupSelection(async () => modelFiles)} />);

    expect(await screen.findByRole('alert')).toHaveTextContent(message);
    expect(screen.getByRole('button', { name: 'Try download again' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Choose existing model files' })).toBeEnabled();
  });

  // These messages previously told a user to supply "Q4 or Q8 model and mmproj
  // GGUF files" and reported an "image self-test". This build pins exactly one
  // file and starts the server with --no-mmproj, so that advice named files that
  // do not exist. The names it does use must stay tied to the shipped manifest.
  it('never asks for model files this build does not use', async () => {
    const names = modelManifest.files.map((file) => file.name);
    expect(names).toEqual(['Qwen3.5-2B-Q4_K_M.gguf']);

    for (const error of ['MODEL_FILE_INVALID', 'MODEL_SELF_TEST_FAILED']) {
      const { unmount } = render(<App
        bridge={createInMemoryBridge({ setup: { state: 'failed', downloadedBytes: 40, totalBytes: PINNED_MODEL_BYTES, error } })}
        selection={setupSelection(async () => modelFiles)}
      />);
      const alert = await screen.findByRole('alert');
      expect(alert).not.toHaveTextContent(/mmproj|projector|Q8|image self-test/i);
      unmount();
    }
  });
});
