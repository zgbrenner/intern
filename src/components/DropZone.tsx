import { FileUp } from 'lucide-react';
import type { DragEvent } from 'react';
import { Icon } from './Icon';

export function DropZone({ onDrop }: { onDrop(payload: unknown): void }) {
  const handleDrop = (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault();
    onDrop(event.dataTransfer);
  };
  return <div className="drop-zone" aria-label="Drag files or folders here to add to the queue" aria-describedby="supported-formats" role="region" onDragOver={(event) => event.preventDefault()} onDrop={handleDrop}>
    <Icon icon={FileUp} /><div className="drop-copy"><span>Drag files or folders here to add to the queue</span><small id="supported-formats">Supports PDF, DOCX, TXT, Markdown, PNG, JPEG (JPG), and TIFF</small></div>
  </div>;
}
