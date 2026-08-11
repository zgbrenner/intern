import type { LucideIcon } from 'lucide-react';

export function Icon({ icon: Glyph, label }: { icon: LucideIcon; label?: string }) {
  return <Glyph aria-hidden={label ? undefined : true} aria-label={label} size={20} strokeWidth={1.75} />;
}
