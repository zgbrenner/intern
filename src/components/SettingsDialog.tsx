import { ExternalLink, X } from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';
import type { AppSettings, CloudLocation, CloudRoot, DescriptionsStatus, DestinationLayout, HostedModelStatus, HostedProvider, IntakeStatus } from '../types';
import { GUIDE_URL } from '../lib/bridge';
import type { DescriptionsEventSource, DesktopBridge, IntakeEventSource, SelectionBoundary, UpdateStatus } from '../lib/bridge';
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
    case 'DESCRIPTIONS_NEED_DESTINATION': return 'Description records live in the destination folder, so choose a destination before turning them on. (DESCRIPTIONS_NEED_DESTINATION)';
    case 'DESCRIPTIONS_DISABLED': return 'Turn on description records and save settings first; then records can be written for documents already filed. (DESCRIPTIONS_DISABLED)';
    case 'HOSTED_MODEL_KEY_MISSING': return 'Paste an API key before choosing the hosted model. (HOSTED_MODEL_KEY_MISSING)';
    case 'HOSTED_MODEL_KEY_EMPTY': return 'The API key is empty. (HOSTED_MODEL_KEY_EMPTY)';
    case 'HOSTED_MODEL_MISCONFIGURED': return 'The hosted model\'s address or model name is not usable. The address must start with https:// (plain http:// only for a server on this computer), and the model needs a name. (HOSTED_MODEL_MISCONFIGURED)';
    case 'HOSTED_MODEL_UNAUTHORIZED': return 'The service rejected the API key. Check the key and the provider. (HOSTED_MODEL_UNAUTHORIZED)';
    case 'HOSTED_MODEL_UNREACHABLE': return 'The service could not be reached. Check the address and your connection. (HOSTED_MODEL_UNREACHABLE)';
    case 'HOSTED_MODEL_RATE_LIMITED': return 'The service asked for a slower pace. Try again in a moment. (HOSTED_MODEL_RATE_LIMITED)';
    case 'HOSTED_MODEL_REJECTED': return 'The service rejected the request — usually a model name it does not know. (HOSTED_MODEL_REJECTED)';
    case 'HOSTED_MODEL_REFUSED': return 'The model declined to read the calibration document, so it cannot be relied on to file yours. (HOSTED_MODEL_REFUSED)';
    case 'MODEL_SELF_TEST_FAILED': return 'The model answered, but not correctly: it did not name the calibration document. Try a more capable model. (MODEL_SELF_TEST_FAILED)';
    case 'MODEL_RESPONSE_INVALID': return 'The model did not answer in the shape Intern needs, twice. Try a more capable model. (MODEL_RESPONSE_INVALID)';
    case 'SECRET_STORE_UNAVAILABLE': return 'Your operating system\'s credential store could not be used, so the key was not saved. (SECRET_STORE_UNAVAILABLE)';
  }
  if (error instanceof Error && error.message.trim()) return error.message.trim();
  if (typeof error === 'object' && error && 'message' in error && typeof error.message === 'string' && error.message.trim()) return error.message.trim();
  if (typeof error === 'string' && error.trim()) return error.trim();
  return 'Settings could not be saved.';
}

/**
 * Opening the guide is a shell hand-off, so the only failures worth naming are
 * "no browser answered" and a refused capability. Either way the address is
 * shown, because a person can always type it themselves.
 */
function describeGuideError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error ?? '');
  return message.trim() ? `${message.trim()} You can reach it at ${GUIDE_URL}.` : `You can reach it at ${GUIDE_URL}.`;
}

function cloudBadgeText(cloud: CloudLocation): string {
  // OneDrive display names already carry the product name ("OneDrive – Contoso");
  // SharePoint's is the bare tenant, so the product is prefixed here. A
  // network share is named after the share, because that is what a person
  // would call it - and nothing about a share moves through a sync client.
  if (cloud.provider === 'network_share') return `On a network share – ${cloud.displayName}`;
  return cloud.provider === 'sharepoint' ? `Synced with SharePoint – ${cloud.displayName}` : `Synced with ${cloud.displayName}`;
}

/** The name a synced location is listed under: its badge text without the verb. */
function rootLabel(root: CloudRoot): string {
  if (root.provider === 'sharepoint') return `SharePoint – ${root.displayName}`;
  if (root.provider === 'network_share') return `Network share – ${root.displayName}`;
  return root.displayName;
}

function formatRecordedTime(at: number | null): string {
  return at === null ? 'never' : new Date(at * 1000).toLocaleString();
}

function describeDescriptionsError(error: string): string {
  // The backend keeps the code and the operating system's own words; the
  // first is for the guide's troubleshooting table, the second says what
  // actually happened (a folder that is read-only, a share that went away).
  return error.startsWith('DESCRIPTION_RETRACT_FAILED')
    ? `The record for an undone rename could not be removed. ${error}`
    : `The last description record could not be written. ${error}`;
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

/**
 * The layouts, in the order they are offered. The example is what a statement
 * of work would land under, so the choice is concrete rather than abstract.
 */
const LAYOUTS: Array<{ value: DestinationLayout; label: string; example: string }> = [
  { value: 'flat', label: 'No subfolders', example: 'Everything directly in the destination folder' },
  { value: 'year', label: 'By year', example: '2026\\' },
  { value: 'year_type', label: 'By year, then document type', example: '2026\\Statement of Work\\' },
  { value: 'type', label: 'By document type', example: 'Statement of Work\\' },
  { value: 'party', label: 'By first party', example: 'Ridgeline Cartography LLC\\' },
];

function isLayout(value: string): value is DestinationLayout {
  return LAYOUTS.some((layout) => layout.value === value);
}

const PROVIDERS: Array<{ value: HostedProvider; label: string }> = [
  { value: 'anthropic', label: 'Anthropic (Claude)' },
  { value: 'openai_compatible', label: 'OpenAI-compatible (OpenAI, OpenRouter, Ollama, LM Studio, and others)' },
];

function isProvider(value: string): value is HostedProvider {
  return PROVIDERS.some((provider) => provider.value === value);
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
  const [roots, setRoots] = useState<CloudRoot[]>([]);
  const [descriptions, setDescriptions] = useState<DescriptionsStatus>();
  const [backfilling, setBackfilling] = useState(false);
  const [backfillMessage, setBackfillMessage] = useState('');
  const [backfillError, setBackfillError] = useState('');
  const [helpError, setHelpError] = useState('');
  const [hostedStatus, setHostedStatus] = useState<HostedModelStatus>();
  // The key is typed here and sent to the credential store on Save or Test;
  // it never becomes part of `next`, so it never reaches the settings file.
  const [keyDraft, setKeyDraft] = useState('');
  const [testing, setTesting] = useState(false);
  const [testMessage, setTestMessage] = useState('');
  const [testError, setTestError] = useState('');
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
  useEffect(() => {
    let active = true;
    // Both are conveniences: a bridge without them (an older desktop build,
    // a test double) leaves the list empty and the status line quiet.
    void Promise.resolve().then(() => bridge.cloudRoots())
      .then((found) => { if (active) setRoots(found); })
      .catch(() => { if (active) setRoots([]); });
    void Promise.resolve().then(() => bridge.descriptionsStatus())
      .then((current) => { if (active) setDescriptions(current); })
      .catch(() => { /* The section still renders without a status. */ });
    const source = bridge as DesktopBridge & Partial<DescriptionsEventSource>;
    const stop = source.subscribeDescriptions?.((current) => { if (active) setDescriptions(current); });
    return () => { active = false; stop?.(); };
  }, [bridge]);
  useEffect(() => {
    let active = true;
    void Promise.resolve().then(() => bridge.hostedModelStatus())
      .then((current) => { if (active) setHostedStatus(current); })
      .catch(() => { /* An older bridge without the hosted model leaves the section quiet. */ });
    return () => { active = false; };
  }, [bridge]);
  const browse = async (apply: (path: string) => void) => {
    const folder = await selection?.pickFolder();
    if (folder) apply(folder.path);
  };
  const providerDefaults = hostedStatus?.providers.find((entry) => entry.provider === next.hostedProvider);
  // A typed key goes to the credential store first, so that what is saved or
  // tested is the configuration the person is looking at.
  const storeKeyDraft = async () => {
    if (!keyDraft.trim()) return;
    await bridge.hostedModelSetKey(keyDraft);
    setKeyDraft('');
    setHostedStatus(await bridge.hostedModelStatus());
  };
  const runSave = async () => {
    if (saving) return;
    setSaving(true);
    setSaveError('');
    try {
      await storeKeyDraft();
      await onSave(next);
    }
    catch (error) { setSaveError(saveFailure(error)); }
    finally { setSaving(false); }
  };
  const runHostedTest = async () => {
    if (testing) return;
    setTesting(true);
    setTestError('');
    setTestMessage('');
    try {
      await storeKeyDraft();
      const result = await bridge.hostedModelTest(next);
      setTestMessage(`Connected. ${result.model} at ${result.endpoint} named the calibration document “${result.filename}” in ${(result.inferenceMillis / 1000).toFixed(1)} s.`);
    } catch (error) {
      setTestError(saveFailure(error));
    } finally {
      setTesting(false);
    }
  };
  const runClearKey = async () => {
    setTestError('');
    setTestMessage('');
    try {
      await bridge.hostedModelClearKey();
      setKeyDraft('');
      setHostedStatus(await bridge.hostedModelStatus());
    } catch (error) {
      setTestError(saveFailure(error));
    }
  };
  const runBackfill = async () => {
    if (backfilling) return;
    setBackfilling(true);
    setBackfillError('');
    setBackfillMessage('');
    try {
      const result = await bridge.descriptionsBackfill();
      setBackfillMessage(result.failed
        ? `${result.written} ${result.written === 1 ? 'record' : 'records'} written; ${result.failed} could not be written.`
        : `${result.written} ${result.written === 1 ? 'record' : 'records'} written.`);
      setDescriptions(await bridge.descriptionsStatus());
    } catch (error) {
      setBackfillError(saveFailure(error));
    } finally {
      setBackfilling(false);
    }
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
  const runGuide = async () => {
    setHelpError('');
    try { await bridge.openGuide(); }
    catch (error) { setHelpError(`The guide could not be opened. ${describeGuideError(error)}`); }
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
  /*
    Grouped rather than stacked. This dialog had grown to a destination, four
    checkboxes, a watched folder with its own machine name and status block,
    and an updates section - all in one undifferentiated column, so a
    first-timer met a wall of controls with no way to tell which ones belonged
    together. The headings name the decision each group settles, and Save is
    pinned to the footer so it stays reachable from any scroll position.
  */
  return <div className="dialog-backdrop" role="presentation"><section ref={dialog} className="settings-dialog settings-panel" role="dialog" aria-modal="true" aria-label="Settings">
    <div className="dialog-head"><h2>Settings</h2><button type="button" className="icon-button" onClick={onClose} aria-label="Close settings"><Icon icon={X} /></button></div>
    <div className="dialog-body">
      <section className="settings-group">
        <h3>Filing</h3>
        <p className="section-lead">Where renamed documents are put, and when Intern may file one without asking.</p>
        <div className="folder-row">
          <label>Destination folder<input ref={destination} value={next.destination} onChange={(event) => setNext({ ...next, destination: event.target.value })} /></label>
          {selection && <button type="button" aria-label="Browse for destination folder" onClick={() => void browse((path) => setNext((current) => ({ ...current, destination: path })))}>Browse…</button>}
        </div>
        {destinationCloud && <p className="cloud-badge">{cloudBadgeText(destinationCloud)}</p>}
        {/*
          A year of contracts in one folder is a thousand files nobody can
          scan. Each layout names its folders from facts the filename already
          carries, so nothing is filed anywhere the name does not explain.
        */}
        <label>Arrange filed documents<select value={next.destinationLayout} onChange={(event) => { const value = event.target.value; if (isLayout(value)) setNext({ ...next, destinationLayout: value }); }}>
          {LAYOUTS.map((layout) => <option key={layout.value} value={layout.value}>{layout.label}</option>)}
        </select></label>
        <p className="check-hint">{LAYOUTS.find((layout) => layout.value === next.destinationLayout)?.example}{next.destinationLayout === 'flat' ? '.' : ' — a document missing that fact goes in an “Undated” or “Unsorted” folder, never loose in the root. Undo removes a folder it empties.'}</p>
        <label className="check-label"><input type="checkbox" checked={Boolean(next.automaticRename)} onChange={(event) => setNext({ ...next, automaticRename: event.target.checked })} />Automatically rename high-confidence files</label>
        <p className="check-hint">Anything Intern is less sure about still waits for you in Needs Review.</p>
      </section>
      <section className="settings-group">
        <h3>Model</h3>
        {/*
          The local model is the product's promise and the default. The hosted
          option exists for people who have decided the trade is worth it, and
          the section says exactly what the trade is - in the lead, again when
          it is chosen, and in the header badge once it is on.
        */}
        <p className="section-lead">Which model reads your documents. The local model keeps every document on this computer. A hosted model sends the text of each document to the service you name, under your own API key.</p>
        <label className="check-label"><input type="radio" name="model-source" value="local" checked={next.modelSource === 'local'} onChange={() => setNext({ ...next, modelSource: 'local' })} />Local model on this computer (recommended)</label>
        <label className="check-label"><input type="radio" name="model-source" value="hosted" checked={next.modelSource === 'hosted'} onChange={() => setNext({ ...next, modelSource: 'hosted' })} />Hosted model with my API key</label>
        {next.modelSource === 'hosted' && <div className="hosted-model" role="group" aria-label="Hosted model">
          <p className="privacy-warning" role="note">With this on, the text Intern reads from each document — not the file, but the words in it, condensed — is sent to this service to be named. It is the only setting that sends document text off this computer. Your key, your account, your provider's terms.</p>
          <label>Provider<select value={next.hostedProvider} onChange={(event) => { const value = event.target.value; if (isProvider(value)) setNext({ ...next, hostedProvider: value }); }}>
            {PROVIDERS.map((provider) => <option key={provider.value} value={provider.value}>{provider.label}</option>)}
          </select></label>
          <label>API address<input value={next.hostedBaseUrl} placeholder={providerDefaults?.baseUrl} spellCheck={false} onChange={(event) => setNext({ ...next, hostedBaseUrl: event.target.value })} /></label>
          <p className="check-hint">Leave empty for {providerDefaults?.baseUrl ?? 'the provider\'s own address'}. Plain http:// is accepted only for a server on this computer, such as Ollama or LM Studio.</p>
          <label>Model<input value={next.hostedModel} placeholder={providerDefaults?.model || 'The model name as the service knows it'} spellCheck={false} onChange={(event) => setNext({ ...next, hostedModel: event.target.value })} /></label>
          {providerDefaults?.model && <p className="check-hint">Leave empty for {providerDefaults.model}.</p>}
          <label>API key<input type="password" value={keyDraft} autoComplete="off" placeholder={hostedStatus?.keyStored ? `Stored in your credential manager (${hostedStatus.keyHint ?? '…'})` : 'Paste your API key'} onChange={(event) => setKeyDraft(event.target.value)} /></label>
          <p className="check-hint">Kept in your operating system's credential store under “Intern”, never in Intern's settings file.{hostedStatus?.keyStored ? ' A new key replaces the stored one.' : ''}</p>
          {testMessage && <p role="status" aria-label="Hosted model test" aria-live="polite">{testMessage}</p>}
          {testError && <p className="form-error" role="alert">{testError}</p>}
          <div className="update-actions">
            <button type="button" disabled={testing} onClick={() => void runHostedTest()}>{testing ? 'Testing…' : 'Test connection'}</button>
            {hostedStatus?.keyStored && <button type="button" disabled={testing} onClick={() => void runClearKey()}>Remove stored key</button>}
          </div>
        </div>}
      </section>
      <section className="settings-group">
        <h3>This computer</h3>
        <p className="section-lead">How Intern behaves when the window is closed, and when you sign in.</p>
        <label className="check-label"><input type="checkbox" checked={next.runInBackground} onChange={(event) => setNext({ ...next, runInBackground: event.target.checked })} />Run in background</label>
        <p className="check-hint">Keep Intern in the system tray when the window is closed — watched folders keep working.</p>
        <label className="check-label"><input type="checkbox" checked={next.startAtLogin} onChange={(event) => setNext({ ...next, startAtLogin: event.target.checked })} />Start Intern when you sign in</label>
        {/* Without the tray a minimized start would leave no way back to the
            window, so the checkbox is only offered while background mode is on. */}
        <label className="check-label"><input type="checkbox" disabled={!next.runInBackground} checked={next.startMinimized} onChange={(event) => setNext({ ...next, startMinimized: event.target.checked })} />Start minimized</label>
        {!next.runInBackground && <p className="check-hint">Available when “Run in background” is on, so the tray can bring the window back.</p>}
      </section>
      <section className="settings-group">
        <h3>Shared intake</h3>
        {/*
          The watched folder may be a OneDrive/SharePoint synced folder shared by
          several machines. Coordination happens through small files the sync
          client replicates - Intern itself still makes no network requests and
          no document content leaves this machine.
        */}
        <p className="section-lead">Intern can watch a folder — including a OneDrive or SharePoint folder shared with other machines — and process documents that appear in it. Watched intake needs a destination folder outside the intake folder.</p>
        <label className="check-label"><input type="checkbox" checked={next.intakeEnabled} onChange={(event) => setNext({ ...next, intakeEnabled: event.target.checked })} />Watch a folder for new documents</label>
        <div className="folder-row">
          <label>Intake folder<input value={next.intakeFolder} onChange={(event) => setNext({ ...next, intakeFolder: event.target.value })} /></label>
          {selection && <button type="button" aria-label="Browse for intake folder" onClick={() => void browse((path) => setNext((current) => ({ ...current, intakeFolder: path })))}>Browse…</button>}
        </div>
        {intakeCloud && <p className="cloud-badge">{cloudBadgeText(intakeCloud)}</p>}
        <label className="check-label"><input type="checkbox" checked={next.processOthersUploads} onChange={(event) => setNext({ ...next, processOthersUploads: event.target.checked })} />Also process documents uploaded by others</label>
        <label>This machine's name<input value={next.machineLabel} onChange={(event) => setNext({ ...next, machineLabel: event.target.value })} /></label>
        {/*
          The folder a SharePoint library syncs to lives under the user's
          profile with a name they never chose ("C:\Users\pat\Contoso\Legal -
          Documents"), and finding it in a folder dialog is the step people got
          stuck on. The sync client's own configuration already knows every
          such folder, so they are offered here as one-click starting points;
          a subfolder can still be typed or browsed afterwards.
        */}
        {roots.length > 0 && <div className="synced-locations" role="group" aria-label="Synced locations on this computer">
          <p className="field-label">Synced locations on this computer</p>
          <ul>{roots.map((root) => <li key={`${root.provider}:${root.path}`}>
            <span className="synced-name">{rootLabel(root)}</span>
            <span className="synced-path" title={root.path}>{root.path}</span>
            <span className="synced-actions">
              <button type="button" aria-label={`Use ${rootLabel(root)} as the intake folder`} onClick={() => setNext((current) => ({ ...current, intakeFolder: root.path }))}>Intake</button>
              <button type="button" aria-label={`Use ${rootLabel(root)} as the destination folder`} onClick={() => setNext((current) => ({ ...current, destination: root.path }))}>Destination</button>
            </span>
          </li>)}</ul>
        </div>}
        {next.intakeEnabled && <div className="intake-status">
          <p role="status" aria-label="Intake status" aria-live="polite">{intake
            ? `${intake.watching ? 'Watching' : 'Not watching'} · ${activeMachines} ${activeMachines === 1 ? 'machine' : 'machines'} active · ${intake.heldForOthers} held for others · Last scan: ${formatScanTime(intake.lastScanAt)}`
            : 'Checking intake status…'}</p>
          {intake && intake.syncConflicts > 0 && <p className="check-hint" role="status">{intake.syncConflicts === 1 ? '1 file is' : `${intake.syncConflicts} files are`} a sync conflict copy left behind by OneDrive or SharePoint. Intern leaves {intake.syncConflicts === 1 ? 'it' : 'them'} alone — resolve the conflict in the folder and the surviving document is picked up on the next scan.</p>}
          {intake && intake.awaitingHydration > 0 && <p className="check-hint" role="status">{intake.awaitingHydration === 1 ? '1 document is' : `${intake.awaitingHydration} documents are`} waiting for OneDrive to download {intake.awaitingHydration === 1 ? 'its' : 'their'} contents. Intern is holding {intake.awaitingHydration === 1 ? 'it' : 'them'} rather than failing {intake.awaitingHydration === 1 ? 'it' : 'them'}; connect this machine and the next scan picks {intake.awaitingHydration === 1 ? 'it' : 'them'} up.</p>}
          {intake && intake.unreadableFolders > 0 && <p className="check-hint" role="status">{intake.unreadableFolders === 1 ? '1 subfolder' : `${intake.unreadableFolders} subfolders`} could not be read on the last scan — usually a folder this account has no permission to open. Everything else was scanned; documents in {intake.unreadableFolders === 1 ? 'that folder' : 'those folders'} are not.</p>}
          {(intakeError || intake?.error) && <p className="form-error" role="alert">{intakeError || intake?.error}</p>}
          <button type="button" disabled={scanning} onClick={() => void runScanNow()}>{scanning ? 'Scanning…' : 'Scan now'}</button>
        </div>}
      </section>
      <section className="settings-group">
        <h3>Descriptions for SharePoint</h3>
        {/*
          The sentence Intern writes about a document used to live only in the
          queue. A SharePoint library has a column for it; the sync client that
          carries the file can carry a small record beside it; and a flow (the
          guide has the recipe) copies the sentence into the column. Intern
          itself still makes no network request for this.
        */}
        <p className="section-lead">Intern can leave a small record beside each document it files — the one-sentence description and the date, type, and parties behind the name — so a SharePoint library column can be filled from it. Records go in <code>.intern\descriptions</code> under the destination folder; nothing is sent anywhere by Intern.</p>
        <label className="check-label"><input type="checkbox" checked={Boolean(next.recordDescriptions)} onChange={(event) => setNext({ ...next, recordDescriptions: event.target.checked })} />Write a description record for each filed document</label>
        <p className="check-hint">Needs a destination folder. The guide's “Descriptions in a SharePoint column” section shows the Power Automate flow that fills the column from these records.</p>
        {descriptions && <div className="descriptions-status">
          <p role="status" aria-label="Description records status" aria-live="polite">{descriptions.enabled
            ? `On · ${descriptions.recordedThisSession} ${descriptions.recordedThisSession === 1 ? 'record' : 'records'} written since Intern started · Last: ${formatRecordedTime(descriptions.lastRecordedAt)}`
            : 'Off · no records are being written'}{descriptions.folder ? ` · ${descriptions.folder}` : ''}</p>
          {descriptions.lastError && <p className="form-error" role="alert">{describeDescriptionsError(descriptions.lastError)}</p>}
          {backfillMessage && <p role="status" aria-label="Backfill status" aria-live="polite">{backfillMessage}</p>}
          {backfillError && <p className="form-error" role="alert">{backfillError}</p>}
          {/*
            Only once the saved settings have records on: the backend refuses
            otherwise, and an enabled button that always fails is worse than a
            hint saying what to do first.
          */}
          <button type="button" disabled={backfilling || !settings.recordDescriptions} onClick={() => void runBackfill()}>{backfilling ? 'Writing…' : 'Write records for documents already filed'}</button>
          {!settings.recordDescriptions && <p className="check-hint">Save with records on, then write records for what was filed before.</p>}
        </div>}
      </section>
      <section className="settings-group">
        <h3>Updates</h3>
        {/*
          Deliberately a button and not a background check. Intern reads private
          documents; software that contacts a server on its own schedule is
          software you have to take on trust. Nothing is sent but a request for
          the release manifest, and nothing is installed unless it is signed by
          the key this build was compiled with.
        */}
        <p className="section-lead">Intern checks only when you ask. Updates must be signed by this project's key or they are refused.</p>
        {status?.state === 'current' && <p role="status" aria-label="Update status" aria-live="polite">Intern {status.currentVersion} is the latest release.</p>}
        {status?.state === 'unsupported' && <p role="status" aria-label="Update status" aria-live="polite">Updates are available in the installed desktop application.</p>}
        {status?.state === 'available' && <p role="status" aria-label="Update status" aria-live="polite">Version {status.version} is available. You have {status.currentVersion}.</p>}
        {updateError && <p className="form-error" role="alert">{updateError}</p>}
        <div className="update-actions">
          <button type="button" disabled={busy} onClick={() => void runCheck()}>{checking ? 'Checking…' : 'Check for updates'}</button>
          {status?.state === 'available' && <button type="button" className="primary" disabled={busy} onClick={() => void runInstall()}>{installing ? 'Installing…' : `Install ${status.version} and restart`}</button>}
        </div>
      </section>
      <section className="settings-group">
        <h3>Help & support</h3>
        <p className="section-lead">The guide walks through setup, what each status means, and what to do when a document fails.</p>
        {helpError && <p className="form-error" role="alert">{helpError}</p>}
        <div className="update-actions">
          <button type="button" className="link-out" onClick={() => void runGuide()}>Open the guide<Icon icon={ExternalLink} /></button>
        </div>
        <p className="check-hint">Opens in your browser.</p>
      </section>
    </div>
    <div className="dialog-actions">
      {saveError && <p className="form-error save-error" role="alert">{saveError}</p>}
      <button type="button" className="primary" disabled={saving} onClick={() => void runSave()}>{saving ? 'Saving…' : 'Save settings'}</button>
    </div>
  </section></div>;
}
