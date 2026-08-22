import { X } from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';
import type { AppSettings, CloudLocation, IntakeStatus } from '../types';
import type { DesktopBridge, IntakeEventSource, SelectionBoundary, UpdateStatus } from '../lib/bridge';
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

/**
 * Settings save is where intake misconfiguration is caught, so the backend's
 * error codes are translated into sentences that say what to change rather
 * than what check failed.
 */
function saveFailure(error: unknown): string {
  const code = typeof error === 'string'
    ? error
    : typeof error === 'object' && error && 'code' in error && typeof error.code === 'string'
      ? error.code
      : error instanceof Error
        ? error.message
        : undefined;
  switch (code?.trim()) {
    case 'INTAKE_FOLDER_MISSING': return 'The intake folder could not be found. Choose an existing folder to watch. (INTAKE_FOLDER_MISSING)';
    case 'INTAKE_NEEDS_DESTINATION': return 'Watched intake needs a destination folder outside the intake folder. (INTAKE_NEEDS_DESTINATION)';
    case 'DESTINATION_INSIDE_INTAKE': return 'The destination folder is inside the intake folder, so renamed files would be picked up as new documents. Choose a destination outside it. (DESTINATION_INSIDE_INTAKE)';
    case 'AUTOSTART_FAILED': return 'Your system would not let Intern change whether it starts at sign-in, so nothing was saved. Try again. (AUTOSTART_FAILED)';
  }
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  if (typeof error === 'object' && error && 'message' in error && typeof error.message === 'string' && error.message.trim()) return error.message.trim();
  if (typeof error === 'string' && error.trim()) return error.trim();
  return 'Settings could not be saved.';
}

function cloudBadgeText(cloud: CloudLocation): string {
  // OneDrive display names already carry the product name ("OneDrive – Contoso");
  // SharePoint's is the bare tenant, so the product is prefixed here.
  return cloud.provider === 'sharepoint' ? `Synced with SharePoint – ${cloud.displayName}` : `Synced with ${cloud.displayName}`;
}

/**
 * Debounced folder classification: typing pauses ~300ms before the path is
 * looked up, and a stale response never overwrites a newer one.
 */
function useCloudBadge(classify: (path: string) => Promise<CloudLocation | null>, path: string): CloudLocation | null {
  const [cloud, setCloud] = useState<CloudLocation | null>(null);
  useEffect(() => {
    const trimmed = path.trim();
    if (!trimmed) { setCloud(null); return; }
    let active = true;
    const timer = window.setTimeout(() => {
      classify(trimmed)
        .then((location) => { if (active) setCloud(location); })
        .catch(() => { if (active) setCloud(null); });
    }, 300);
    return () => { active = false; window.clearTimeout(timer); };
  }, [classify, path]);
  return cloud;
}

function formatScanTime(lastScanAt: number | null): string {
  if (lastScanAt === null) return 'not yet';
  return new Date(lastScanAt * 1000).toLocaleTimeString();
}

interface Props {
  settings: AppSettings;
  bridge: DesktopBridge;
  selection?: SelectionBoundary;
  onSave(settings: AppSettings): Promise<void>;
  onClose(): void;
  onCheckForUpdate(): Promise<UpdateStatus>;
  onInstallUpdate(): Promise<void>;
}

export function SettingsDialog({ settings, bridge, selection, onSave, onClose, onCheckForUpdate, onInstallUpdate }: Props) {
  const [next, setNext] = useState(settings);
  const [status, setStatus] = useState<UpdateStatus>();
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [updateError, setUpdateError] = useState('');
  const [saveError, setSaveError] = useState('');
  const [saving, setSaving] = useState(false);
  const [intake, setIntake] = useState<IntakeStatus>();
  const [intakeError, setIntakeError] = useState('');
  const [scanning, setScanning] = useState(false);
  const busy = checking || installing;
  const dialog = useRef<HTMLElement>(null);
  const destination = useRef<HTMLInputElement>(null);
  useEffect(() => setNext(settings), [settings]);
  const classify = useCallback((path: string) => bridge.classifyFolder(path), [bridge]);
  const destinationCloud = useCloudBadge(classify, next.destination);
  const intakeCloud = useCloudBadge(classify, next.intakeFolder);
  useEffect(() => {
    let active = true;
    void bridge.intakeStatus()
      .then((current) => { if (active) setIntake(current); })
      .catch(() => { /* Status is a convenience; the section still renders without it. */ });
    const source = bridge as DesktopBridge & Partial<IntakeEventSource>;
    const stop = source.subscribeIntake?.((current) => { if (active) setIntake(current); });
    return () => { active = false; stop?.(); };
  }, [bridge]);
  const browse = async (apply: (path: string) => void) => {
    const folder = await selection?.pickFolder();
    if (folder) apply(folder.path);
  };
  const runSave = async () => {
    if (saving) return;
    setSaving(true);
    setSaveError('');
    try { await onSave(next); }
    catch (error) { setSaveError(saveFailure(error)); }
    finally { setSaving(false); }
  };
  const runScanNow = async () => {
    if (scanning) return;
    setScanning(true);
    setIntakeError('');
    try {
      await bridge.scanIntakeNow();
      setIntake(await bridge.intakeStatus());
    } catch (error) {
      setIntakeError(saveFailure(error));
    } finally {
      setScanning(false);
    }
  };
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
  const activeMachines = intake?.machines.filter((machine) => machine.active).length ?? 0;
  return <div className="dialog-backdrop" role="presentation"><section ref={dialog} className="settings-dialog" role="dialog" aria-modal="true" aria-label="Settings">
    <div className="inspector-title"><h2>Settings</h2><button type="button" className="icon-button" onClick={onClose} aria-label="Close settings"><Icon icon={X} /></button></div>
    <div className="folder-row">
      <label>Destination folder<input ref={destination} value={next.destination} onChange={(event) => setNext({ ...next, destination: event.target.value })} /></label>
      {selection && <button type="button" aria-label="Browse for destination folder" onClick={() => void browse((path) => setNext((current) => ({ ...current, destination: path })))}>Browse…</button>}
    </div>
    {destinationCloud && <p className="cloud-badge">{cloudBadgeText(destinationCloud)}</p>}
    <label className="check-label"><input type="checkbox" checked={next.runInBackground} onChange={(event) => setNext({ ...next, runInBackground: event.target.checked })} />Run in background</label>
    <p className="check-hint">Keep Intern in the system tray when the window is closed — watched folders keep working.</p>
    <label className="check-label"><input type="checkbox" checked={next.startAtLogin} onChange={(event) => setNext({ ...next, startAtLogin: event.target.checked })} />Start Intern when you sign in</label>
    {/* Without the tray a minimized start would leave no way back to the
        window, so the checkbox is only offered while background mode is on. */}
    <label className="check-label"><input type="checkbox" disabled={!next.runInBackground} checked={next.startMinimized} onChange={(event) => setNext({ ...next, startMinimized: event.target.checked })} />Start minimized</label>
    {!next.runInBackground && <p className="check-hint">Available when “Run in background” is on, so the tray can bring the window back.</p>}
    <label className="check-label"><input type="checkbox" checked={Boolean(next.automaticRename)} onChange={(event) => setNext({ ...next, automaticRename: event.target.checked })} />Automatically rename high-confidence files</label>
    <section className="intake">
      <h3>Shared intake</h3>
      {/*
        The watched folder may be a OneDrive/SharePoint synced folder shared by
        several machines. Coordination happens through small files the sync
        client replicates - Intern itself still makes no network requests and
        no document content leaves this machine.
      */}
      <p className="update-note">Intern can watch a folder — including a OneDrive or SharePoint synced folder shared with other machines — and process documents that appear in it. Watched intake needs a destination folder outside the intake folder.</p>
      <label className="check-label"><input type="checkbox" checked={next.intakeEnabled} onChange={(event) => setNext({ ...next, intakeEnabled: event.target.checked })} />Watch a folder for new documents</label>
      <div className="folder-row">
        <label>Intake folder<input value={next.intakeFolder} onChange={(event) => setNext({ ...next, intakeFolder: event.target.value })} /></label>
        {selection && <button type="button" aria-label="Browse for intake folder" onClick={() => void browse((path) => setNext((current) => ({ ...current, intakeFolder: path })))}>Browse…</button>}
      </div>
      {intakeCloud && <p className="cloud-badge">{cloudBadgeText(intakeCloud)}</p>}
      <label className="check-label"><input type="checkbox" checked={next.processOthersUploads} onChange={(event) => setNext({ ...next, processOthersUploads: event.target.checked })} />Also process documents uploaded by others</label>
      <label>This machine's name<input value={next.machineLabel} onChange={(event) => setNext({ ...next, machineLabel: event.target.value })} /></label>
      {next.intakeEnabled && <div className="intake-status">
        <p role="status" aria-label="Intake status" aria-live="polite">{intake
          ? `${intake.watching ? 'Watching' : 'Not watching'} · ${activeMachines} ${activeMachines === 1 ? 'machine' : 'machines'} active · ${intake.heldForOthers} held for others · Last scan: ${formatScanTime(intake.lastScanAt)}`
          : 'Checking intake status…'}</p>
        {(intakeError || intake?.error) && <p className="form-error" role="alert">{intakeError || intake?.error}</p>}
        <button type="button" disabled={scanning} onClick={() => void runScanNow()}>{scanning ? 'Scanning…' : 'Scan now'}</button>
      </div>}
    </section>
    {saveError && <p className="form-error save-error" role="alert">{saveError}</p>}
    <button type="button" className="primary" disabled={saving} onClick={() => void runSave()}>{saving ? 'Saving…' : 'Save settings'}</button>
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
