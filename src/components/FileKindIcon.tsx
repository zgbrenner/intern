import { File, FileImage, FileSpreadsheet, FileText, FileType2 } from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import { Icon } from './Icon';

const kinds: Record<string, { kind: string; icon: LucideIcon }> = {
  pdf: { kind: 'pdf', icon: FileText },
  doc: { kind: 'document', icon: FileType2 },
  docx: { kind: 'document', icon: FileType2 },
  xls: { kind: 'spreadsheet', icon: FileSpreadsheet },
  xlsx: { kind: 'spreadsheet', icon: FileSpreadsheet },
  txt: { kind: 'text', icon: FileText },
  md: { kind: 'text', icon: FileText },
  png: { kind: 'image', icon: FileImage },
  jpg: { kind: 'image', icon: FileImage },
  jpeg: { kind: 'image', icon: FileImage },
  tif: { kind: 'image', icon: FileImage },
  tiff: { kind: 'image', icon: FileImage },
};

export function FileKindIcon({ filename }: { filename: string }) {
  const extension = filename.split('.').at(-1)?.toLowerCase() ?? '';
  const { kind, icon } = kinds[extension] ?? { kind: 'other', icon: File };
  return <span className={`file-kind file-kind--${kind}`} aria-hidden="true"><Icon icon={icon} /></span>;
}
