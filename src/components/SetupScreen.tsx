import { Download, TriangleAlert } from 'lucide-react';
import { byteCount } from '../lib/format';
import type { SetupState } from '../types';
import { Icon } from './Icon';

export function SetupScreen({ setup, onStart }: { setup?: SetupState; onStart(): void }) {
  if (!setup) return <main className="setup-screen" aria-label="Intern setup"><section><p role="status" aria-label="Loading setup" aria-live="polite">Loading setup</p></section></main>;
  const failed = setup.state === 'failed';
  const downloading = setup.state === 'downloading';
  return <main className="setup-screen" aria-label="Intern setup"><section>
    <Icon icon={failed ? TriangleAlert : Download} /><h1>{failed ? 'Model setup needs attention' : 'Set up Intern'}</h1>
    <p>{failed ? setup.error ?? 'The local model could not be prepared.' : 'Download the local model to begin processing on this device.'}</p>
    {downloading && <><progress value={setup.downloadedBytes} max={setup.totalBytes} /><p aria-live="polite">{byteCount(setup.downloadedBytes)} of {byteCount(setup.totalBytes)} bytes</p></>}
    <button type="button" className="primary" onClick={onStart} disabled={downloading}>{downloading ? 'Downloading model…' : failed ? 'Try download again' : 'Download model'}</button>
  </section></main>;
}
