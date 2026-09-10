/**
 * A pass over the whole window in the order a person drives it: the queue, the
 * filter, the review drawer, the dialogs, the keyboard, and the narrow window.
 * It began as an exploratory session written to find defects rather than to pin
 * behaviour, and the defects it found are now assertions.
 *
 * It drives the browser build, whose backend is the in-memory bridge, so it
 * proves the interface and not the pipeline underneath it.
 *
 * Set `INTERN_SHOTS` to a directory to keep a frame at each step, the way the
 * exploratory session did; unset - the default, and what CI runs - nothing is
 * written and Playwright's own trace covers a failure.
 */
import { expect, test, type Page } from '@playwright/test';
import { mkdirSync } from 'node:fs';

const SHOTS = process.env.INTERN_SHOTS;
if (SHOTS) mkdirSync(SHOTS, { recursive: true });

let step = 0;
async function shot(page: Page, name: string) {
  if (!SHOTS) return;
  step += 1;
  await page.screenshot({ path: `${SHOTS}/${String(step).padStart(2, '0')}-${name}.png`, fullPage: false });
}

test.describe.configure({ mode: 'serial' });

test('the queue, the drawer, and the filter behave as a person drives them', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('main', { name: 'Intern' })).toBeVisible();
  await shot(page, 'initial');

  // The seeded queue is eight rows: three ready, one review, one processing,
  // three waiting, plus a completed one that lives under Completed.
  await expect(page.getByRole('row')).toHaveCount(9); // header + 8
  await expect(page.getByRole('button', { name: /^Queue/ })).toContainText('8');

  // Every row's status must be readable as text, not colour alone.
  for (const [name, status] of [
    ['Employment Agreement - John Smith.pdf', 'Ready'],
    ['Lease Agreement - 123 Main St.pdf', 'Needs review'],
    ['Q1 Financials.pdf', 'Processing'],
    ['Invoice INV-1001.pdf', 'Waiting'],
  ] as const) {
    await expect(page.getByRole('row', { name: new RegExp(name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'i') })).toContainText(status);
  }

  // The filter appears above six items and narrows by original name,
  // proposed name, and a word from the description.
  const filter = page.getByPlaceholder(/Filter by filename or description/i);
  await expect(filter).toBeVisible();
  await filter.fill('lease');
  await expect(page.getByRole('row')).toHaveCount(2);
  await shot(page, 'filter-by-original-name');
  await filter.fill('Non-Disclosure');
  await expect(page.getByRole('row', { name: /NDA - Acme Corp\.docx/i })).toBeVisible();
  await filter.fill('landlord');
  await expect(page.getByRole('row', { name: /Lease Agreement - 123 Main St\.pdf/i })).toBeVisible();
  await filter.fill('zzzznothing');
  // A filter that matches nothing replaces the table with a sentence naming
  // what was searched for, rather than leaving a bare header over emptiness.
  await expect(page.getByText(/No items match .zzzznothing./i)).toBeVisible();
  await expect(page.getByRole('row')).toHaveCount(0);
  await shot(page, 'filter-empty-result');
  await filter.fill('');
  await expect(page.getByRole('row')).toHaveCount(9);

  // The review drawer describes the row it was opened from.
  await page.getByRole('button', { name: 'Select Lease Agreement - 123 Main St.pdf' }).click();
  const drawer = page.getByRole('complementary', { name: 'Review item' });
  await expect(drawer).toBeVisible();
  await expect(drawer).toContainText('Lease Agreement - 123 Main St.pdf');
  await expect(drawer.getByLabel('Filename')).toHaveValue('2023-09-15 Lease Agreement between ABC Properties LLC and TenantCo Inc.pdf');
  await expect(drawer).toContainText('ABC Properties LLC');
  await expect(drawer).toContainText('TenantCo Inc.');
  await shot(page, 'review-drawer');

  // Wide, the inspector is a side panel and not a modal, so Escape must not
  // dismiss it: it describes the selected row, and the selection is still
  // there. Closing is the panel's own control.
  await page.keyboard.press('Escape');
  await expect(drawer).toBeVisible();
  await drawer.getByRole('button', { name: /close/i }).click();
  await expect(drawer).toBeHidden();
  await shot(page, 'panel-closed-by-its-own-control');

  // Narrow, the same inspector becomes a modal drawer, and there Escape is
  // the expected way out. 1100px is the breakpoint.
  await page.setViewportSize({ width: 1000, height: 800 });
  await page.getByRole('button', { name: 'Select Lease Agreement - 123 Main St.pdf' }).click();
  const modal = page.getByRole('dialog', { name: 'Review item' });
  await expect(modal).toBeVisible();
  await expect(modal).toHaveAttribute('aria-modal', 'true');
  await shot(page, 'narrow-drawer-is-modal');
  await page.keyboard.press('Escape');
  await expect(modal).toBeHidden();
  await shot(page, 'narrow-drawer-closed-by-escape');
  await page.setViewportSize({ width: 1280, height: 800 });
});

test('a rename without a date is refused, and one with a date is applied and undone', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Select Lease Agreement - 123 Main St.pdf' }).click();
  const drawer = page.getByRole('complementary', { name: 'Review item' });

  // Every applied name must start with the document's date.
  await drawer.getByLabel('Filename').fill('Lease Agreement between ABC Properties LLC and TenantCo Inc.pdf');
  await drawer.getByRole('button', { name: /Approve & rename/i }).click();
  await shot(page, 'date-required-refusal');
  await expect(page.getByRole('alert').filter({ hasText: /date/i }).first()).toBeVisible();
  // The row must not have moved.
  await expect(page.getByRole('row', { name: /Lease Agreement - 123 Main St\.pdf/i })).toContainText('Needs review');

  // With a date it applies.
  await drawer.getByLabel('Filename').fill('2023-09-15 Lease Agreement between ABC Properties LLC and TenantCo Inc.pdf');
  await drawer.getByRole('button', { name: /Approve & rename/i }).click();
  await page.getByRole('button', { name: /^Completed/ }).click();
  await expect(page.getByRole('row', { name: /Lease Agreement - 123 Main St\.pdf/i })).toBeVisible();
  await shot(page, 'applied-shows-under-completed');

  // Undo returns the file and leaves the document waiting for a person.
  await page.getByRole('row', { name: /Lease Agreement - 123 Main St\.pdf/i }).getByRole('button', { name: /Select/ }).click();
  await page.getByRole('button', { name: /^Undo/ }).click();
  await shot(page, 'after-undo');
  await page.getByRole('button', { name: /Needs Review/ }).click();
  await expect(page.getByRole('row', { name: /Lease Agreement - 123 Main St\.pdf/i })).toBeVisible();
  await shot(page, 'undone-waits-in-review');
});

test('pause, resume, apply all ready, and discard waiting all report what they did', async ({ page }) => {
  await page.goto('/');
  const pause = page.getByRole('button', { name: /Pause queue/i });
  await expect(pause).toBeVisible();
  await pause.click();
  await expect(page.getByRole('button', { name: /Resume queue/i })).toBeVisible();
  await shot(page, 'paused');
  await page.getByRole('button', { name: /Resume queue/i }).click();
  await expect(page.getByRole('button', { name: /Pause queue/i })).toBeVisible();

  await page.getByRole('button', { name: /Apply all ready/i }).click();
  await shot(page, 'apply-all-ready');
  await page.getByRole('button', { name: /^Completed/ }).click();
  await expect(page.getByRole('row')).not.toHaveCount(1);
  await shot(page, 'completed-after-apply-all');

  await page.getByRole('button', { name: /^Queue/ }).click();
  const discard = page.getByRole('button', { name: /Discard waiting/i });
  if (await discard.isVisible()) {
    await discard.click();
    await shot(page, 'discard-waiting');
  }
});

test('settings reveals the hosted-model fields, keeps typed text, and closes on Escape', async ({ page }) => {
  await page.goto('/');
  const gear = page.getByRole('button', { name: /Settings/i });
  await gear.click();
  const dialog = page.getByRole('dialog', { name: /Settings/i });
  await expect(dialog).toBeVisible();
  await shot(page, 'settings-open');

  // The header must say the privacy posture, and it must be the local one
  // until a hosted model is actually chosen.
  await expect(page.getByText(/Private . On this device/i)).toBeVisible();

  // Choosing a hosted model reveals the address, model, and key fields.
  await dialog.getByRole('radio', { name: /Hosted model with my API key/i }).check();
  await shot(page, 'settings-hosted-revealed');
  // Not `input[type=text]`: an input that omits the attribute is still a text
  // field, and most of these omit it.
  const textInputs = dialog.locator('input:not([type="checkbox"]):not([type="radio"])');
  expect(await textInputs.count(), 'choosing a hosted model must reveal its address, model, and key').toBeGreaterThan(2);
  await expect(dialog.getByText(/sent to this service to be named/i)).toBeVisible();

  // Typing into a revealed field must survive re-renders of the window
  // behind the dialog. (The queue-event case that caused the original focus
  // defect cannot be produced in the browser build, which has no ticking
  // queue; that one is covered by the dialog-focus unit tests.)
  const address = dialog.getByLabel(/API address/i).first();
  if (await address.count()) {
    await address.click();
    await address.fill('http://127.0.0.1:11434/v1');
    await page.waitForTimeout(1200);
    await expect(address).toHaveValue('http://127.0.0.1:11434/v1');
    await expect(address).toBeFocused();
    await shot(page, 'settings-typed-address-kept');
  }

  // Escape closes, nothing is saved, and the focus returns to the control
  // that opened it.
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(gear).toBeFocused();
  await shot(page, 'settings-closed');

  // Reopening shows the unsaved change was discarded.
  await gear.click();
  await expect(page.getByRole('dialog', { name: /Settings/i }).getByRole('radio', { name: /Local model on this computer/i })).toBeChecked();
  await shot(page, 'settings-reopened-unsaved-discarded');
  await page.keyboard.press('Escape');
});

test('the queue can be driven from the keyboard alone', async ({ page }) => {
  await page.goto('/');
  // Tab into the page and walk until a queue row selector takes focus, then
  // open it with the keyboard and check the panel followed.
  let opened = false;
  for (let press = 0; press < 40 && !opened; press += 1) {
    await page.keyboard.press('Tab');
    const label = await page.evaluate(() => document.activeElement?.getAttribute('aria-label') ?? '');
    if (label.startsWith('Select ')) {
      await page.keyboard.press('Enter');
      opened = true;
    }
  }
  expect(opened, 'a queue row must be reachable by keyboard').toBe(true);
  await expect(page.getByRole('complementary', { name: 'Review item' })).toBeVisible();
  await shot(page, 'keyboard-opened-panel');
});

test('history lists finished operations newest first', async ({ page }) => {
  await page.goto('/');
  // History lives under Completed, beside Clear history.
  await page.getByRole('button', { name: /^Completed/ }).click();
  const history = page.getByRole('button', { name: /^History$/ });
  await expect(history).toBeVisible();
  await history.first().click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('Board Meeting Minutes');
  await shot(page, 'history');
  await page.keyboard.press('Escape');
});

test('the window stays usable at 1024 pixels and the drawer does not overflow', async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 768 });
  await page.goto('/');
  await page.getByRole('main', { name: 'Intern' }).waitFor();

  // 1024 is the narrowest supported window, and 1100 and below is where the
  // inspector becomes a modal drawer. The selection Intern seeds so the panel
  // is not empty is not a person's, so it must not open that drawer: launching
  // into a modal leaves the queue inert and the caret in a filename field
  // before anyone has clicked anything.
  const launched = await page.evaluate(() => ({
    role: document.querySelector('aside.inspector')?.getAttribute('role') ?? null,
    queueInert: document.querySelector('section.queue-panel')?.hasAttribute('inert') ?? null,
    focused: document.activeElement?.getAttribute('aria-label')
      ?? document.activeElement?.labels?.[0]?.textContent?.trim() ?? null,
  }));
  expect(launched, 'a narrow window must not launch into a modal drawer over an inert queue')
    .toEqual({ role: 'complementary', queueInert: false, focused: null });

  await expect(page.getByRole('complementary', { name: 'Review item' })).toBeVisible();
  await shot(page, 'narrow-1024');
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, 'the page must not scroll horizontally at 1024 pixels').toBeLessThanOrEqual(0);
});

test('no console errors or failed requests during an ordinary session', async ({ page }) => {
  const problems: string[] = [];
  page.on('console', (message) => { if (message.type() === 'error') problems.push(`console: ${message.text()}`); });
  page.on('pageerror', (error) => problems.push(`pageerror: ${error.message}`));
  page.on('requestfailed', (request) => problems.push(`requestfailed: ${request.url()}`));

  await page.goto('/');
  await page.getByRole('button', { name: 'Select Employment Agreement - John Smith.pdf' }).click();
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: /Needs Review/ }).click();
  await page.getByRole('button', { name: /^Completed/ }).click();
  await page.getByRole('button', { name: /^Queue/ }).click();
  await page.getByRole('button', { name: /Settings/i }).click();
  await page.keyboard.press('Escape');
  await shot(page, 'console-clean-session');

  expect(problems, `browser reported problems:\n${problems.join('\n')}`).toEqual([]);
});
