import { FileUp } from 'lucide-react';
import type { DragEvent } from 'react';
import { Icon } from './Icon';

export function DropZone({ onDrop }: { onDrop(payload: unknown): void }) {
  const handleDrop = (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault();
    onDrop(event.dataTransfer);
  };
  return <div className="drop-zone" aria-label="Drag files or folders here to add to the queue" role="region" onDragOver={(event) => event.preventDefault()} onDrop={handleDrop} tabIndex={0}>
    <Icon icon={FileUp} /><div>Drag files or folders here to add to the queue</div>
  </div>;
}
