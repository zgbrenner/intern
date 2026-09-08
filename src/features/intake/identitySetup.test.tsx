import { fireEvent, render, screen, within } from '@testing-library/react';
import { expect, it } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';

it('explains that unverified shared uploads stay untouched and separates Microsoft identity from the machine label', async () => {
  render(<App bridge={createInMemoryBridge()} />);
  fireEvent.click(await screen.findByRole('button', { name: 'Settings' }));
  const dialog = screen.getByRole('dialog', { name: 'Settings' });
  expect(within(dialog).getByText(/unverified uploads are never processed/i)).toBeVisible();
  expect(within(dialog).getByRole('group', { name: 'Microsoft upload identity' })).toBeVisible();
});
