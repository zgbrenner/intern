import { expect, test } from '@playwright/test';

test('mixed batch can be reviewed, approved, and undone entirely in memory', async ({ page }) => {
  await page.goto('/?fixtureBatch=1');
  await expect(page.getByRole('main', { name: 'Intern' })).toBeVisible();

  await page.getByRole('region', { name: 'Drag files or folders here to add to the queue' }).evaluate((dropZone) => {
    const transfer = new DataTransfer();
    transfer.items.add(new File(['fictional invoice'], 'duplicate-invoice-a.pdf', { type: 'application/pdf' }));
    transfer.items.add(new File(['fictional invoice'], 'duplicate-invoice-b.pdf', { type: 'application/pdf' }));
    transfer.items.add(new File(['unsupported'], 'unsupported.csv', { type: 'text/csv' }));
    transfer.items.add(new File(['lock'], '~$nda.docx', { type: 'application/vnd.openxmlformats-officedocument.wordprocessingml.document' }));
    dropZone.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: transfer }));
  });

  await expect(page.getByRole('row', { name: /duplicate-invoice-a\.pdf/i })).toContainText('Needs review');
  await expect(page.getByRole('row', { name: /duplicate-invoice-b\.pdf/i })).toContainText('Needs review');
  await expect(page.getByRole('row', { name: /unsupported\.csv/i })).toContainText('Failed');
  await expect(page.getByRole('row', { name: /~\$nda\.docx/i })).toContainText('Failed');

  await page.getByRole('button', { name: 'Select duplicate-invoice-b.pdf' }).click();
  await expect(page.getByText(/different path.*separate/i)).toBeVisible();
  await page.getByRole('button', { name: 'Select unsupported.csv' }).click();
  await expect(page.getByText(/Unsupported format skipped/i)).toBeVisible();
  await page.getByRole('button', { name: 'Select ~$nda.docx' }).click();
  await expect(page.getByText(/Office lock file skipped/i)).toBeVisible();

  await page.getByRole('region', { name: 'Drag files or folders here to add to the queue' }).evaluate((dropZone) => {
    const transfer = new DataTransfer();
    transfer.items.add(new File(['fictional invoice'], 'duplicate-invoice-a.pdf', { type: 'application/pdf' }));
    dropZone.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: transfer }));
  });
  await expect(page.getByRole('row', { name: /duplicate-invoice-a\.pdf/i })).toHaveCount(1);
  await expect(page.getByRole('complementary', { name: 'Review item' }).getByText('duplicate-invoice-a.pdf', { exact: true })).toBeVisible();

  await page.getByRole('button', { name: 'Needs Review' }).click();
  await page.getByRole('button', { name: 'Select duplicate-invoice-a.pdf' }).click();
  await page.getByLabel('Filename').fill('2025-04-30 - Invoice - INV-2048 reviewed.pdf');
  await page.getByLabel('Description').fill('Invoice INV-2048 dated April 30, 2025 for Atlas Threadworks LLC.');
  await page.getByRole('button', { name: 'Approve & rename' }).click();

  await page.getByRole('button', { name: 'Completed' }).click();
  const completed = page.getByRole('row', { name: /duplicate-invoice-a\.pdf/i });
  await expect(completed).toContainText('2025-04-30 - Invoice - INV-2048 reviewed.pdf');
  await completed.getByRole('button', { name: /Select/ }).click();
  await page.getByRole('button', { name: 'Undo' }).click();

  await page.getByRole('button', { name: 'Needs Review' }).click();
  await expect(page.getByRole('row', { name: /duplicate-invoice-a\.pdf/i })).toBeVisible();
});
