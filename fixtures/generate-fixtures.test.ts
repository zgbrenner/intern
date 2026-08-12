import { createHash } from 'node:crypto';
import { mkdtemp, readFile, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { generateFixtures } from './generate-fixtures.mjs';

async function inventory(root: string) {
  const files: string[] = [];
  async function visit(directory: string, prefix = '') {
    for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) await visit(join(directory, entry.name), relative);
      else files.push(relative);
    }
  }
  await visit(root);
  return Promise.all(files.map(async (file) => ({
    file,
    sha256: createHash('sha256').update(await readFile(join(root, file))).digest('hex'),
  })));
}

describe('clean-room fixture generator', () => {
  it('emits the complete gold corpus byte-for-byte deterministically', async () => {
    const first = await mkdtemp(join(tmpdir(), 'intern-fixtures-a-'));
    const second = await mkdtemp(join(tmpdir(), 'intern-fixtures-b-'));

    await generateFixtures(first);
    await generateFixtures(second);

    const firstInventory = await inventory(first);
    expect(firstInventory).toEqual(await inventory(second));
    expect(firstInventory.map(({ file }) => file)).toEqual(expect.arrayContaining([
      'employment-agreement.pdf',
      'scanned-lease.pdf',
      'mixed-signature.pdf',
      'nda.docx',
      'multi-date-invoice.pdf',
      'meeting-minutes.md',
      'rotated-low-resolution-scan.png',
      'encrypted.pdf',
      'malformed.pdf',
      'long-document-100-pages.pdf',
      'document-image.png',
      'document-image.jpg',
      'document-image.tiff',
      'statement-of-work.pdf',
      'termination-notice.pdf',
      'consulting-amendment.pdf',
      'vendor-invoice.pdf',
      'settlement-agreement.pdf',
      'ambiguous-note.pdf',
      'order-form.docx',
      'mixed-batch/duplicate-invoice-a.pdf',
      'mixed-batch/duplicate-invoice-b.pdf',
      'mixed-batch/unsupported.csv',
      'mixed-batch/~$nda.docx',
    ]));
    expect((await readFile(join(first, 'long-document-100-pages.pdf'), 'latin1')).match(/\/Type \/Page\b/g)).toHaveLength(100);
  });

  it('matches the reviewed canonical digest manifest', async () => {
    const root = await mkdtemp(join(tmpdir(), 'intern-fixtures-manifest-'));
    await generateFixtures(root);

    expect(JSON.parse(await readFile(join(root, 'manifest.json'), 'utf8')))
      .toEqual(JSON.parse(await readFile(join(process.cwd(), 'fixtures/manifest.json'), 'utf8')));
  });

  it('contains only fictional gold facts and no predecessor material', async () => {
    const root = await mkdtemp(join(tmpdir(), 'intern-fixtures-cleanroom-'));
    const gold = await generateFixtures(root);

    expect(gold.schema_version).toBe(2);
    expect(gold.fixtures.find((fixture) => fixture.file === 'employment-agreement.pdf')?.parties)
      .toEqual(['Northstar Lantern Works LLC', 'Mira Vale']);
    for (const fixture of gold.fixtures) {
      expect(fixture).toMatchObject({
        expected_readiness: expect.stringMatching(/^(ready|needs_review|failed)$/),
        ambiguity: expect.any(Array),
        acceptable_description_facts: expect.any(Array),
        expected_routing: expect.stringMatching(/^(native_text|ocr|mixed_native_ocr|anydoc|text|error)$/),
      });
    }

    // The traps are the point of the corpus: dates and names that are easy to
    // extract and wrong to file under.
    const sow = gold.fixtures.find((fixture) => fixture.file === 'statement-of-work.pdf');
    expect(sow?.document_date).toBe('2026-04-01');
    expect(sow?.forbidden_dates).toEqual(expect.arrayContaining(['2023-06-02', '2026-04-09']));
    const notice = gold.fixtures.find((fixture) => fixture.file === 'termination-notice.pdf');
    expect(notice?.acceptable_dates).toContain('2027-01-31');
    expect(notice?.forbidden_parties).toContain('Marcus Reyes');
    const amendment = gold.fixtures.find((fixture) => fixture.file === 'consulting-amendment.pdf');
    expect(amendment?.forbidden_dates).toContain('2023-01-12');
    const invoice = gold.fixtures.find((fixture) => fixture.file === 'vendor-invoice.pdf');
    expect(invoice?.forbidden_dates).toContain('2026-02-04');
    expect(gold.fixtures.find((fixture) => fixture.file === 'multi-date-invoice.pdf')?.ambiguity)
      .toContain('invoice_and_due_dates');
    expect(gold.fixtures.find((fixture) => fixture.file === 'rotated-low-resolution-scan.png')?.expected_routing)
      .toBe('ocr');
    expect(Object.fromEntries(gold.fixtures.map((fixture) => [fixture.file, fixture.expected_routing])))
      .toMatchObject({
        'employment-agreement.pdf': 'native_text',
        'multi-date-invoice.pdf': 'native_text',
        'scanned-lease.pdf': 'ocr',
        'rotated-low-resolution-scan.png': 'ocr',
        'document-image.png': 'ocr',
        'document-image.jpg': 'ocr',
        'document-image.tiff': 'ocr',
        'mixed-signature.pdf': 'mixed_native_ocr',
      });
    for (const { file } of await inventory(root)) {
      expect((await readFile(join(root, file))).includes(Buffer.from(['Back', 'Log'].join('')))).toBe(false);
    }
  });
});
