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
    await expect(page.getByRole('button', { name: 'Settings' })).toHaveCount(2);
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
    expect(focusStyle.width).toBe('2px');
    expect(focusStyle.style).not.toBe('none');
    expect(focusStyle.color).not.toBe('transparent');

    if (process.env.INTERN_QA_CAPTURE === '1') {
      const capture = resolve('docs/qa/latest-implementation.png');
      await mkdir(dirname(capture), { recursive: true });
      await page.screenshot({ path: capture, animations: 'disabled', fullPage: false });
    }
  });

  test('preserves labels, focus targets, and a non-overflowing inspector drawer at 1024 pixels', async ({ page }) => {
    await page.setViewportSize({ width: 1024, height: 768 });
    await page.goto('/');
    const navigation = page.getByRole('navigation', { name: 'Queue navigation' });
    for (const name of ['Queue', 'Needs Review', 'Completed', 'Settings']) {
      await expect(navigation.getByRole('button', { name })).toBeVisible();
    }
    await expect(page.getByRole('complementary', { name: 'Review item' })).toBeVisible();
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

    await page.locator('body').focus();
    await page.keyboard.press('Tab');
    const firstHeaderAction = page.getByRole('button', { name: 'Add files' });
    await expect(firstHeaderAction).toBeFocused();
    await expect(firstHeaderAction).toHaveCSS('outline-width', '2px');
  });
});
