import { CheckCircle2, CircleAlert, List, Settings } from 'lucide-react';
import { Icon } from './Icon';
import type { QueueView } from '../types';

const entries = [
  { view: 'queue' as const, label: 'Queue', icon: List },
  { view: 'review' as const, label: 'Needs Review', icon: CircleAlert },
  { view: 'completed' as const, label: 'Completed', icon: CheckCircle2 },
];
export function Sidebar({ active, items, inert, onChange, onSettings }: { active: QueueView; items: { status: string }[]; inert?: boolean; onChange(view: QueueView): void; onSettings(trigger: HTMLButtonElement): void }) {
  const count = (view: QueueView) => view === 'queue' ? items.filter((item) => item.status !== 'completed').length : items.filter((item) => item.status === (view === 'review' ? 'review' : 'completed')).length;
  return <nav className="sidebar" aria-label="Queue navigation" inert={inert || undefined}>
    <div>{entries.map(({ view, label, icon }) => <button type="button" key={view} className={active === view ? 'active' : ''} onClick={() => onChange(view)} aria-current={active === view ? 'page' : undefined} aria-label={label}><Icon icon={icon} /><span>{label}</span><b>{count(view)}</b></button>)}</div>
    <button className="settings-nav" type="button" onClick={(event) => onSettings(event.currentTarget)} aria-label="Settings"><Icon icon={Settings} /></button>
  </nav>;
}
