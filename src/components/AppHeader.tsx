import { FilePlus2, FolderOpen, Pause, Play, Settings } from 'lucide-react';
import { Icon } from './Icon';

interface Props { paused: boolean; onAddFiles(): void; onAddFolder(): void; onTogglePause(): void; onSettings(trigger: HTMLButtonElement): void }
export function AppHeader({ paused, onAddFiles, onAddFolder, onTogglePause, onSettings }: Props) {
  return <header className="app-header">
    <div className="product"><strong>Intern</strong><span className="private"><i />Private · On this device</span></div>
    <div className="header-actions">
      <button type="button" onClick={onAddFiles}><Icon icon={FilePlus2} />Add files</button>
      <button type="button" onClick={onAddFolder}><Icon icon={FolderOpen} />Add folder</button>
      <button type="button" className="icon-button" onClick={onTogglePause} aria-label={paused ? 'Resume queue' : 'Pause queue'}><Icon icon={paused ? Play : Pause} /></button>
      <button type="button" className="icon-button" onClick={(event) => onSettings(event.currentTarget)} aria-label="Settings"><Icon icon={Settings} /></button>
    </div>
  </header>;
}
