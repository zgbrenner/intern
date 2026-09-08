import { FilePlus2, FolderOpen, Pause, Play } from 'lucide-react';
import { Icon } from './Icon';

interface Props { paused: boolean; inert?: boolean; busy?: boolean; hosted?: boolean; onAddFiles(): void; onAddFolder(): void; onTogglePause(): void }
export function AppHeader({ paused, inert, busy, hosted, onAddFiles, onAddFolder, onTogglePause }: Props) {
  /*
    The badge is the product's promise, so it must not keep making it once a
    hosted model is on. Then it says the opposite, in the same place, so the
    one setting that sends document text off the machine is never invisible.
  */
  return <header className="app-header" inert={inert || undefined}>
    <div className="product"><strong>Intern<span className="brand-tag">for Vistage</span></strong>{hosted
      ? <span className="private hosted" title="A hosted model is reading your documents. Change it in Settings."><i />Hosted model · Text leaves this device</span>
      : <span className="private"><i />Private · On this device</span>}</div>
    <div className="header-actions">
      <button type="button" onClick={onAddFiles}><Icon icon={FilePlus2} />Add files</button>
      <button type="button" onClick={onAddFolder}><Icon icon={FolderOpen} />Add folder</button>
      <button type="button" className="icon-button" disabled={busy} onClick={onTogglePause} aria-label={paused ? 'Resume queue' : 'Pause queue'}><Icon icon={paused ? Play : Pause} /></button>
    </div>
  </header>;
}
