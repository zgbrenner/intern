import { Download, TriangleAlert } from 'lucide-react';
import { byteCount, byteSize } from '../lib/format';
import type { SetupState } from '../types';
import { Icon } from './Icon';

interface SetupScreenProps {
  setup?: SetupState;
  busy: boolean;
  canChooseExisting: boolean;
  operationError?: string;
  onStart(): void;
  onCancel(): void;
  onChooseExisting(): void;
  /** Open Settings to configure a hosted model instead of the download. */
  onUseHostedModel?(): void;
}

export function SetupScreen({ setup, busy, canChooseExisting, operationError, onStart, onCancel, onChooseExisting, onUseHostedModel }: SetupScreenProps) {
  if (!setup) return <main className="setup-screen" aria-label="Intern setup"><section><p role="status" aria-label="Loading setup" aria-live="polite">Loading setup</p></section></main>;
  const failed = setup.state === 'failed';
  const downloading = setup.state === 'downloading';
  const canceled = setup.state === 'required' && setup.error === 'MODEL_DOWNLOAD_CANCELED';
  const resumable = setup.downloadedBytes > 0 && setup.downloadedBytes < setup.totalBytes;
  const showProgress = downloading || resumable || failed && setup.downloadedBytes > 0;
  return <main className="setup-screen" aria-label="Intern setup"><section>
    <Icon icon={failed ? TriangleAlert : Download} /><h1>{failed ? 'Model setup needs attention' : 'Set up Intern'}</h1>
    {/*
      The size comes from the setup state, which the backend fills from
      model-manifest.json before the first byte is fetched, so it cannot drift
      from what will actually be downloaded. It used to be the hardcoded string
      "approximately 3.27 GB" - the combined size of a model and a vision
      projector, from a design this pipeline no longer uses. The first screen a
      new user saw overstated the download by more than two and a half times.
    */}
    <p>Download {setup.totalBytes > 0 ? `${byteSize(setup.totalBytes)} of ` : ''}model files, or choose matching files already on this computer.</p>
    <p>Your documents and filenames stay on this device. After setup, processing runs fully locally with no network dependency.</p>
    {operationError && <p className="setup-error" role="alert">{operationError}</p>}
    {canceled && !operationError && <p role="status" aria-label="Setup status" aria-live="polite">Download canceled. Your progress was saved.</p>}
    {showProgress && <><progress aria-label="Model setup progress" value={setup.downloadedBytes} max={setup.totalBytes} /><p aria-live="polite">{byteCount(setup.downloadedBytes)} of {byteCount(setup.totalBytes)} bytes</p></>}
    <div className="setup-actions">
      <button type="button" className="primary" onClick={onStart} disabled={downloading || busy}>{downloading ? 'Downloading model…' : failed ? 'Try download again' : resumable ? 'Resume download' : 'Download model'}</button>
      {downloading && <button type="button" onClick={onCancel} disabled={busy}>Cancel setup</button>}
      <button type="button" onClick={onChooseExisting} disabled={downloading || busy || !canChooseExisting}>Choose existing model files</button>
    </div>
    {/*
      The other way in. A hosted model needs no download, but it sends the
      text of every document to the service named in Settings - the one
      thing the paragraph above promises Intern does not do - so it is
      offered as the smaller, plainly-labelled choice, never the default.
    */}
    {onUseHostedModel && <p className="setup-alternative">Or use a hosted model with your own API key — no download, but document text is sent to that service. <button type="button" className="link-button" disabled={downloading || busy} onClick={onUseHostedModel}>Set up a hosted model</button></p>}
  </section></main>;
}
