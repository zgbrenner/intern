import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';

async function openSettings() {
  fireEvent.click((await screen.findAllByRole('button', { name: 'Settings' }))[0]);
  return screen.findByRole('dialog', { name: 'Settings' });
}

describe('the hosted model setting', () => {
  it('is off by default, and says what turning it on means', async () => {
    render(<App bridge={createInMemoryBridge()} />);
    const dialog = await openSettings();

    expect(within(dialog).getByLabelText('Local model on this computer (recommended)')).toBeChecked();
    expect(within(dialog).getByLabelText('Hosted model with my API key')).not.toBeChecked();
    expect(within(dialog).queryByRole('group', { name: 'Hosted model' })).not.toBeInTheDocument();
    expect(screen.getByText('Private · On this device')).toBeVisible();

    fireEvent.click(within(dialog).getByLabelText('Hosted model with my API key'));
    const hosted = within(dialog).getByRole('group', { name: 'Hosted model' });
    expect(within(hosted).getByRole('note')).toHaveTextContent(/sends document text off this computer/);
    expect(within(hosted).getByLabelText('API address')).toHaveAttribute('placeholder', 'https://api.anthropic.com/v1');
    expect(within(hosted).getByLabelText('Model')).toHaveAttribute('placeholder', 'claude-opus-5');
  });

  it('refuses to save a hosted model before a key is stored', async () => {
    const base = createInMemoryBridge();
    const saveSettings = vi.fn(base.saveSettings);
    render(<App bridge={{ ...base, saveSettings }} />);
    const dialog = await openSettings();
    fireEvent.click(within(dialog).getByLabelText('Hosted model with my API key'));
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save settings' }));

    expect(await within(dialog).findByRole('alert')).toHaveTextContent('Paste an API key before choosing the hosted model');
    expect(screen.getByRole('dialog', { name: 'Settings' })).toBeVisible();
  });

  it('stores the key in the credential store, tests the connection, and saves the choice', async () => {
    const base = createInMemoryBridge();
    const saveSettings = vi.fn(base.saveSettings);
    const hostedModelSetKey = vi.fn(base.hostedModelSetKey);
    render(<App bridge={{ ...base, saveSettings, hostedModelSetKey }} />);
    const dialog = await openSettings();
    fireEvent.click(within(dialog).getByLabelText('Hosted model with my API key'));
    fireEvent.change(within(dialog).getByLabelText('Model'), { target: { value: 'claude-sonnet-5' } });
    fireEvent.change(within(dialog).getByLabelText('API key'), { target: { value: 'sk-ant-api03-example-key-9876' } });

    fireEvent.click(within(dialog).getByRole('button', { name: 'Test connection' }));
    const status = await within(dialog).findByRole('status', { name: 'Hosted model test' });
    expect(status).toHaveTextContent(/Connected\. claude-sonnet-5 at https:\/\/api\.anthropic\.com\/v1\/messages named the calibration document/);
    expect(hostedModelSetKey).toHaveBeenCalledWith('sk-ant-api03-example-key-9876');
    // The key left the field for the store; the field now says so.
    expect(within(dialog).getByLabelText('API key')).toHaveValue('');
    expect(within(dialog).getByLabelText('API key')).toHaveAttribute('placeholder', 'Stored in your credential manager (…9876)');
    expect(within(dialog).getByRole('button', { name: 'Remove stored key' })).toBeVisible();

    fireEvent.click(within(dialog).getByRole('button', { name: 'Save settings' }));

    await waitFor(() => expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ modelSource: 'hosted', hostedProvider: 'anthropic', hostedModel: 'claude-sonnet-5' })));
    // The key is never part of the settings that were saved.
    expect(JSON.stringify(saveSettings.mock.calls)).not.toContain('sk-ant');
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Settings' })).not.toBeInTheDocument());
    expect(screen.getByText('Hosted model · Text leaves this device')).toBeVisible();
    expect(screen.queryByText('Private · On this device')).not.toBeInTheDocument();
  });

  it('names a rejected key rather than a generic failure', async () => {
    render(<App bridge={createInMemoryBridge()} />);
    const dialog = await openSettings();
    fireEvent.click(within(dialog).getByLabelText('Hosted model with my API key'));
    fireEvent.change(within(dialog).getByLabelText('Provider'), { target: { value: 'openai_compatible' } });
    fireEvent.change(within(dialog).getByLabelText('Model'), { target: { value: 'gpt-filing' } });
    fireEvent.change(within(dialog).getByLabelText('API key'), { target: { value: 'bad-key-1234567890' } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Test connection' }));

    expect(await within(dialog).findByRole('alert')).toHaveTextContent('The service rejected the API key');
  });
});
