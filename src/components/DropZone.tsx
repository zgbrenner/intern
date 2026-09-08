import { FileUp } from 'lucide-react';
import type { DragEvent } from 'react';
import { Icon } from './Icon';

const LABEL = 'Drag files or folders here to add to the queue';
const FORMATS = 'Supports PDF, DOCX, XLSX, EML, TXT, Markdown, PNG, JPEG (JPG), and TIFF';

/**
 * The same drop target in two sizes.
 *
 * `bar` is the strip above a queue that already has work in it. `hero` is what
 * a first-time user actually meets: an empty table with four column headings
 * and nothing under them said only that the app had loaded. It is the same
 * region, with the same accessible name and the same drop handler, so the
 * first thing a person sees is also the thing they can act on.
 */
export function DropZone({ variant = 'bar', onDrop }: { variant?: 'bar' | 'hero'; onDrop(payload: unknown): void }) {
  const handleDrop = (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault();
    onDrop(event.dataTransfer);
  };
  return <div className={`drop-zone drop-zone--${variant}`} aria-label={LABEL} aria-describedby="supported-formats" role="region" onDragOver={(event) => event.preventDefault()} onDrop={handleDrop}>
    {variant === 'hero'
      ? <div className="drop-copy">
        <span className="drop-mark"><Icon icon={FileUp} /></span>
        <h2>Drop documents here</h2>
        <p>Intern reads each one on this device, proposes a filename, and shows you the evidence behind it. Nothing is renamed until you approve it.</p>
        <p className="drop-alternative">Or use the Add files and Add folder buttons above.</p>
        <small id="supported-formats">{FORMATS}</small>
      </div>
      : <>
        <Icon icon={FileUp} /><div className="drop-copy"><span>{LABEL}</span><small id="supported-formats">{FORMATS}</small></div>
      </>}
  </div>;
}
