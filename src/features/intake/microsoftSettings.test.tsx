import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';
import { MicrosoftIntakeSettings } from './MicrosoftIntakeSettings';
import type { MicrosoftIntakeBridge, MicrosoftIntakeStatus, MicrosoftAccount } from './microsoft';
const account: MicrosoftAccount = { tenantId: 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa', id: 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb', displayName: 'Zachary Brenner', email: 'zack@example.test' };
const initial: MicrosoftIntakeStatus = { connected: false, account: null, tenantId: account.tenantId, clientId: 'cccccccc-cccc-cccc-cccc-cccccccccccc', binding: null, documents: [], error: null };
function bridge(status = initial) {
  let current = { ...status };
  const microsoft: MicrosoftIntakeBridge = {
    microsoftIntakeStatus: vi.fn(async () => current),
    microsoftSignInStart: vi.fn(async () => ({ userCode: 'ABCD-EFGH', verificationUri: 'https://microsoft.com/devicelogin', intervalSeconds: 5, expiresAt: 9999999999 })),
    microsoftSignInPoll: vi.fn(async () => ({ state: 'pending' as const, intervalSeconds: 5 })),
    microsoftDisconnect: vi.fn(async () => { current = { ...current, connected: false, account: null }; }),
    microsoftBindIntake: vi.fn(async (driveId, folderId) => {
      const binding = { localFolder: 'C:/Intake', driveId, folderId, webUrl: 'https://example.sharepoint.com/Legal/Intake', tenantId: account.tenantId };
      current = { ...current, binding }; return binding;
    }),
    microsoftOpenSignIn: vi.fn(async () => {}),
  };
  return { ...createInMemoryBridge(), ...microsoft };
}
describe('Microsoft upload identity setup', () => {
  it('requires explicit permissions acknowledgment and valid public IDs', async () => {
    render(<MicrosoftIntakeSettings bridge={bridge()} savedFolder="C:/Intake" unsavedFolder={false} />);
    await waitFor(() => expect(screen.getByLabelText('Microsoft tenant ID')).toHaveValue(account.tenantId));
    const connect = screen.getByRole('button', { name: 'Connect my Microsoft account' });
    expect(connect).toBeDisabled();
    fireEvent.click(screen.getByLabelText(/understand the Microsoft permissions/));
    expect(connect).toBeEnabled();
    fireEvent.change(screen.getByLabelText('Microsoft application ID'), { target: { value: 'Zachary Brenner' } });
    expect(connect).toBeDisabled();
    expect(screen.getByRole('note')).toHaveTextContent('Audit permissions cover the workload, not just this folder.');
  });
  it('shows the authenticated identity, with no editable name that could grant ownership', async () => {
    render(<MicrosoftIntakeSettings bridge={bridge({ ...initial, connected: true, account })} savedFolder="C:/Intake" unsavedFolder={false} />);
    expect(await screen.findByText('Zachary Brenner')).toBeVisible();
    expect(screen.getByText('zack@example.test')).toBeVisible();
    expect(screen.queryByRole('textbox', { name: /name|email/i })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Disconnect Microsoft' })).toBeEnabled();
  });
  it('uses a fixed sign-in action and cancels pending authorization when closed', async () => {
    const api = bridge();
    const { unmount } = render(<MicrosoftIntakeSettings bridge={api} savedFolder="C:/Intake" unsavedFolder={false} />);
    await waitFor(() => expect(screen.getByLabelText('Microsoft tenant ID')).toHaveValue(account.tenantId));
    fireEvent.click(screen.getByLabelText(/understand the Microsoft permissions/));
    fireEvent.click(screen.getByRole('button', { name: 'Connect my Microsoft account' }));
    expect(await screen.findByRole('status', { name: 'Microsoft sign-in' })).toHaveTextContent('ABCD-EFGH');
    fireEvent.click(screen.getByRole('button', { name: 'Open Microsoft sign-in' }));
    await waitFor(() => expect(api.microsoftOpenSignIn).toHaveBeenCalledWith());
    unmount();
    expect(api.microsoftDisconnect).toHaveBeenCalledOnce();
  });
  it('never pairs a draft folder that has not been saved', async () => {
    render(<MicrosoftIntakeSettings bridge={bridge({ ...initial, connected: true, account })} savedFolder="C:/Old" unsavedFolder />);
    await screen.findByText('Zachary Brenner');
    fireEvent.change(screen.getByLabelText('Microsoft drive ID'), { target: { value: 'drive' } });
    fireEvent.change(screen.getByLabelText('Microsoft folder ID'), { target: { value: 'folder' } });
    expect(screen.getByRole('button', { name: 'Verify folder pairing' })).toBeDisabled();
    expect(screen.getByText('Save your changed intake folder before pairing it.')).toBeVisible();
  });
  it('pairs saved intake through the backend and shows the resolved Microsoft folder', async () => {
    const api = bridge({ ...initial, connected: true, account });
    render(<MicrosoftIntakeSettings bridge={api} savedFolder="C:/Intake" unsavedFolder={false} />);
    await screen.findByText('Zachary Brenner');
    fireEvent.change(screen.getByLabelText('Microsoft drive ID'), { target: { value: 'drive' } });
    fireEvent.change(screen.getByLabelText('Microsoft folder ID'), { target: { value: 'folder' } });
    fireEvent.click(screen.getByRole('button', { name: 'Verify folder pairing' }));
    expect(await screen.findByRole('status', { name: 'Microsoft folder pairing' })).toHaveTextContent('https://example.sharepoint.com/Legal/Intake');
    expect(api.microsoftBindIntake).toHaveBeenCalledWith('drive', 'folder');
  });
  it('separates the uploader, processor, and unknown holds without an override button', async () => {
    const processor = { ...account, id: 'dddddddd-dddd-dddd-dddd-dddddddddddd', displayName: 'John Smith', email: 'john@example.test' };
    const api = bridge({ ...initial, connected: true, account, documents: [
      { path: 'one', filename: 'agreement.pdf', state: 'processed', reason: 'Analysis completed.', uploader: account, processedBy: processor, filedAs: null, checkedAt: 0 },
      { path: 'two', filename: 'unknown.pdf', state: 'unknown', reason: 'Upload event missing.', uploader: null, processedBy: null, filedAs: null, checkedAt: 0 },
    ] });
    render(<MicrosoftIntakeSettings bridge={api} savedFolder="C:/Intake" unsavedFolder={false} />);
    expect(await screen.findByRole('status', { name: 'Uploader counts' })).toHaveTextContent('1 verified · 0 belonging to others · 1 uploader unknown');
    expect(screen.getByText(/Processed by John Smith/)).toBeVisible();
    expect(screen.getByText('Held: uploader unknown')).toBeVisible();
    expect(screen.queryByRole('button', { name: /process anyway|ignore verification/i })).not.toBeInTheDocument();
  });
  it('displays a failed connection without claiming that uploads were verified', async () => {
    const api = { ...bridge(), microsoftSignInStart: vi.fn(async () => { throw new Error('Organization denied consent.'); }) };
    render(<MicrosoftIntakeSettings bridge={api} savedFolder="C:/Intake" unsavedFolder={false} />);
    await waitFor(() => expect(screen.getByLabelText('Microsoft tenant ID')).toHaveValue(account.tenantId));
    fireEvent.click(screen.getByLabelText(/understand the Microsoft permissions/));
    fireEvent.click(screen.getByRole('button', { name: 'Connect my Microsoft account' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Organization denied consent.');
    expect(screen.queryByText('Processing for')).not.toBeInTheDocument();
  });
  it('does not issue duplicate device-code requests from same-tick clicks', async () => {
    let resolve!: (value: Awaited<ReturnType<MicrosoftIntakeBridge['microsoftSignInStart']>>) => void;
    const api = { ...bridge(), microsoftSignInStart: vi.fn(() => new Promise<Awaited<ReturnType<MicrosoftIntakeBridge['microsoftSignInStart']>>>((done) => { resolve = done; })) };
    const { unmount } = render(<MicrosoftIntakeSettings bridge={api} savedFolder="C:/Intake" unsavedFolder={false} />);
    await waitFor(() => expect(screen.getByLabelText('Microsoft tenant ID')).toHaveValue(account.tenantId));
    fireEvent.click(screen.getByLabelText(/understand the Microsoft permissions/));
    const button = screen.getByRole('button', { name: 'Connect my Microsoft account' });
    act(() => { fireEvent.click(button); fireEvent.click(button); });
    expect(api.microsoftSignInStart).toHaveBeenCalledOnce();
    await act(async () => { resolve({ userCode: 'CODE', verificationUri: 'https://microsoft.com/devicelogin', intervalSeconds: 5, expiresAt: 9999999999 }); });
    unmount();
  });
});
