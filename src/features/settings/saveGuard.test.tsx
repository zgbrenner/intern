import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';

/*
  App holds a set of hardcoded defaults until the real settings arrive. When
  the read failed the failure was swallowed, so the dialog showed those
  defaults as though they were the person's own configuration - and Save wrote
  them over a destination, a watched folder, and a machine name.
*/
describe('settings that could not be read', () => {
  it('refuses to save over settings it could not load', async () => {
    const base = createInMemoryBridge();
    const saveSettings = vi.fn(base.saveSettings);
    const getSettings = vi.fn(async () => { throw new Error('The settings file could not be opened.'); });
    render(<App bridge={{ ...base, getSettings, saveSettings }} />);
    fireEvent.click((await screen.findAllByRole('button', { name: 'Settings' }))[0]);
    const dialog = await screen.findByRole('dialog', { name: 'Settings' });

    fireEvent.click(within(dialog).getByRole('button', { name: 'Save settings' }));

    expect(await within(dialog).findByRole('alert')).toHaveTextContent('could not read your settings');
    await waitFor(() => expect(saveSettings).not.toHaveBeenCalled());
  });
});
