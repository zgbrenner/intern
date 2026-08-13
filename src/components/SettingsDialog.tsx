import { X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import type { AppSettings } from '../types';
import type { UpdateStatus } from '../lib/bridge';
import { Icon } from './Icon';

/**
 * A failed update check is almost always the network, and saying so is more
 * use than the raw error. A refused signature is the one case worth naming
 * exactly, because it means something served an update this build will not
 * trust.
 */
function updateFailure(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error ?? '');
  if (/signature|verify|pubkey/i.test(message)) return `That update was not signed by this project's key, so Intern refused it. (${message})`;
  if (!message.trim()) return 'Could not reach GitHub to check for updates.';
  return `Could not check for updates: ${message}`;
}

interface Props {
  settings: AppSettings;
  onSave(settings: AppSettings): void;
  onClose(): void;
  onCheckForUpdate(): Promise<UpdateStatus>;
  onInstallUpdate(): Promise<void>;
}

export function SettingsDialog({ settings, onSave, onClose, onCheckForUpdate, onInstallUpdate }: Props) {
  const [next, setNext] = useState(settings);
  const [status, setStatus] = useState<UpdateStatus>();
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [updateError, setUpdateError] = useState('');
  const busy = checking || installing;
  const dialog = useRef<HTMLElement>(null);
  const destination = useRef<HTMLInputElement>(null);
  useEffect(() => setNext(settings), [settings]);
  const runCheck = async () => {
    setChecking(true);
    setUpdateError('');
    try { setStatus(await onCheckForUpdate()); }
    catch (error) { setUpdateError(updateFailure(error)); setStatus(undefined); }
    finally { setChecking(false); }
  };
  const runInstall = async () => {
    setInstalling(true);
    setUpdateError('');
    try { await onInstallUpdate(); }
    catch (error) { setUpdateError(updateFailure(error)); }
    finally { setInstalling(false); }
  };
  useEffect(() => {
    destination.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') { event.preventDefault(); onClose(); return; }
      if (event.key !== 'Tab') return;
      const focusable = Array.from(dialog.current?.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled])') ?? []);
      if (!focusable.length) return;
      const current = document.activeElement;
      const index = focusable.indexOf(current as HTMLElement);
      const nextIndex = event.shiftKey ? (index <= 0 ? focusable.length - 1 : index - 1) : (index === focusable.length - 1 ? 0 : index + 1);
      if (index !== -1) { event.preventDefault(); focusable[nextIndex].focus(); }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [onClose]);
  return <div className="dialog-backdrop" role="presentation"><section ref={dialog} className="settings-dialog" role="dialog" aria-modal="true" aria-label="Settings">
    <div className="inspector-title"><h2>Settings</h2><button type="button" className="icon-button" onClick={onClose} aria-label="Close settings"><Icon icon={X} /></button></div>
    <label>Destination folder<input ref={destination} value={next.destination} onChange={(event) => setNext({ ...next, destination: event.target.value })} /></label>
    <label className="check-label"><input type="checkbox" checked={next.startMinimized} onChange={(event) => setNext({ ...next, startMinimized: event.target.checked })} />Start minimized</label>
    <label className="check-label"><input type="checkbox" checked={Boolean(next.automaticRename)} onChange={(event) => setNext({ ...next, automaticRename: event.target.checked })} />Automatically rename high-confidence files</label>
    <button type="button" className="primary" onClick={() => onSave(next)}>Save settings</button>
    <section className="updates">
      <h3>Updates</h3>
      {/*
        Deliberately a button and not a background check. Intern reads private
        documents; software that contacts a server on its own schedule is
        software you have to take on trust. Nothing is sent but a request for
        the release manifest, and nothing is installed unless it is signed by
        the key this build was compiled with.
      */}
      <p className="update-note">Intern checks only when you ask. Updates must be signed by this project's key or they are refused.</p>
      {status?.state === 'current' && <p role="status" aria-label="Update status" aria-live="polite">Intern {status.currentVersion} is the latest release.</p>}
      {status?.state === 'unsupported' && <p role="status" aria-label="Update status" aria-live="polite">Updates are available in the installed desktop application.</p>}
      {status?.state === 'available' && <p role="status" aria-label="Update status" aria-live="polite">Version {status.version} is available. You have {status.currentVersion}.</p>}
      {updateError && <p className="form-error" role="alert">{updateError}</p>}
      <div className="update-actions">
        <button type="button" disabled={busy} onClick={() => void runCheck()}>{checking ? 'Checking…' : 'Check for updates'}</button>
        {status?.state === 'available' && <button type="button" className="primary" disabled={busy} onClick={() => void runInstall()}>{installing ? 'Installing…' : `Install ${status.version} and restart`}</button>}
      </div>
    </section>
  </section></div>;
}
