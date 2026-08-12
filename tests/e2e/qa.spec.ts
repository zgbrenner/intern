import { mkdir } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { expect, test } from '@playwright/test';

test.describe('whole-product browser QA', () => {
  test.use({ viewport: { width: 1536, height: 1024 } });

  test('captures the accepted primary state at 1536 by 1024 with accessible controls', async ({ page }) => {
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await page.goto('/');

    await expect(page.getByRole('main', { name: 'Intern' })).toBeVisible();
    await expect(page.getByRole('navigation', { name: 'Queue navigation' })).toBeVisible();
    await expect(page.getByRole('complementary', { name: 'Review item' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Add files' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Add folder' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Pause queue' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Settings' })).toHaveCount(1);
    await expect(page.getByLabel('Filename')).toBeVisible();
    await expect(page.getByLabel('Description')).toBeVisible();

    const geometry = await page.evaluate(() => {
      const box = (selector: string) => document.querySelector(selector)?.getBoundingClientRect();
      return {
        viewport: [window.innerWidth, window.innerHeight],
        headerHeight: box('.app-header')?.height,
        sidebarWidth: box('.sidebar')?.width,
        inspectorWidth: box('.inspector')?.width,
        horizontalOverflow: document.documentElement.scrollWidth - window.innerWidth,
      };
    });
    expect(geometry).toEqual({ viewport: [1536, 1024], headerHeight: 72, sidebarWidth: 230, inspectorWidth: 370, horizontalOverflow: 0 });

    const addFiles = page.getByRole('button', { name: 'Add files' });
    await addFiles.focus();
    const focusStyle = await addFiles.evaluate((element) => {
      const style = getComputedStyle(element);
      return { width: style.outlineWidth, style: style.outlineStyle, color: style.outlineColor };
    });
    expect(Number.parseFloat(focusStyle.width)).toBeGreaterThanOrEqual(2);
    expect(focusStyle.style).not.toBe('none');
    expect(focusStyle.color).not.toBe('transparent');

    const statusContrast = await page.evaluate(() => {
      const luminance = (color: string) => {
        const channels = color.match(/[\d.]+/g)?.slice(0, 3).map((value) => Number(value) / 255) ?? [];
        const [red, green, blue] = channels.map((value) => value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4);
        return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
      };
      const ratio = (selector: string) => {
        const element = document.querySelector(selector);
        if (!element) return 0;
        const foreground = luminance(getComputedStyle(element).color);
        const rowColor = getComputedStyle(element.closest('tr') ?? element).backgroundColor;
        const background = luminance(rowColor === 'rgba(0, 0, 0, 0)' ? 'rgb(255, 255, 255)' : rowColor);
        return (Math.max(foreground, background) + 0.05) / (Math.min(foreground, background) + 0.05);
      };
      return { review: ratio('.status.review'), waiting: ratio('.status.waiting') };
    });
    expect(statusContrast.review).toBeGreaterThanOrEqual(4.5);
    expect(statusContrast.waiting).toBeGreaterThanOrEqual(4.5);

    const filename = page.getByLabel('Filename');
    await filename.focus();
    await expect(filename).toBeFocused();

    if (process.env.INTERN_QA_CAPTURE === '1') {
      const capture = resolve('docs/qa/latest-implementation.png');
      await mkdir(dirname(capture), { recursive: true });
      await page.screenshot({ path: capture, animations: 'disabled', fullPage: false });
    }
  });

  test('keeps ready, active, and completed queue states actionable without persistent clutter', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByRole('button', { name: 'Apply all ready' })).toBeVisible();
    await page.getByRole('button', { name: 'Select Q1 Financials.xlsx' }).click();
    await expect(page.getByRole('button', { name: 'Cancel processing' })).toBeVisible();
    await page.getByRole('button', { name: 'Completed' }).click();
    await expect(page.getByRole('button', { name: 'Clear history' })).toBeVisible();
  });

  test('preserves labels, focus targets, and a non-overflowing inspector drawer at 1024 pixels', async ({ page }) => {
    await page.setViewportSize({ width: 1024, height: 768 });
    await page.goto('/');
    const navigation = page.locator('.sidebar');
    await expect(navigation).toHaveAttribute('inert', '');
    for (const name of ['Queue', 'Needs Review', 'Completed', 'Settings']) {
      await expect(navigation.locator(`button[aria-label="${name}"]`)).toBeVisible();
    }
    const drawer = page.getByRole('dialog', { name: 'Review item' });
    await expect(drawer).toBeVisible();
    await expect(page.getByLabel('Filename')).toBeFocused();
    const geometry = await page.evaluate(() => {
      const sidebar = document.querySelector('.sidebar')?.getBoundingClientRect();
      const inspector = document.querySelector('.inspector')?.getBoundingClientRect();
      return {
        sidebarWidth: sidebar?.width,
        inspectorWidth: inspector?.width,
        inspectorRight: inspector ? window.innerWidth - inspector.right : undefined,
        horizontalOverflow: document.documentElement.scrollWidth - window.innerWidth,
      };
    });
    expect(geometry).toEqual({ sidebarWidth: 64, inspectorWidth: 370, inspectorRight: 0, horizontalOverflow: 0 });

    const lastDrawerAction = page.getByRole('button', { name: 'More review actions' });
    await lastDrawerAction.focus();
    await page.keyboard.press('Tab');
    await expect(page.getByRole('button', { name: 'Close review' })).toBeFocused();
    await page.keyboard.press('Escape');

    const trigger = page.getByRole('button', { name: 'Select Lease Agreement - 123 Main St.pdf' });
    await trigger.click();
    await expect(page.getByRole('dialog', { name: 'Review item' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(trigger).toBeFocused();
  });
});
