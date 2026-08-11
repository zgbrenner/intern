import { FilePlus2, FolderOpen, Pause, Play } from 'lucide-react';
import { Icon } from './Icon';

interface Props { paused: boolean; inert?: boolean; busy?: boolean; onAddFiles(): void; onAddFolder(): void; onTogglePause(): void }
export function AppHeader({ paused, inert, busy, onAddFiles, onAddFolder, onTogglePause }: Props) {
  return <header className="app-header" inert={inert || undefined}>
    <div className="product"><strong>Intern</strong><span className="private"><i />Private · On this device</span></div>
    <div className="header-actions">
      <button type="button" onClick={onAddFiles}><Icon icon={FilePlus2} />Add files</button>
      <button type="button" onClick={onAddFolder}><Icon icon={FolderOpen} />Add folder</button>
      <button type="button" className="icon-button" disabled={busy} onClick={onTogglePause} aria-label={paused ? 'Resume queue' : 'Pause queue'}><Icon icon={paused ? Play : Pause} /></button>
    </div>
  </header>;
}
