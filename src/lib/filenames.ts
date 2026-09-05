/**
 * The date a filename begins with, as Intern writes it: `YYYY-MM-DD`, a real
 * calendar day, standing on its own before whatever follows. Every rename
 * must carry one - the backend refuses one that does not (DATE_REQUIRED) -
 * so the inspector checks here first and says so before the round trip.
 */
const LEADING_DATE = /^(\d{4})-(\d{2})-(\d{2})(?![0-9A-Za-z])/;

export function leadingDate(filename: string): string | undefined {
  const match = LEADING_DATE.exec(filename.trim());
  if (!match) return undefined;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const date = new Date(Date.UTC(year, month - 1, day));
  const real = date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day;
  return real ? `${match[1]}-${match[2]}-${match[3]}` : undefined;
}

/** `filename` with `date` leading it, replacing any date already there. */
export function withLeadingDate(filename: string, date: string): string {
  const rest = filename.trim().replace(/^\d{4}-\d{2}-\d{2}(?![0-9A-Za-z])[\s_-]*/, '');
  return rest ? `${date} ${rest}` : date;
}
