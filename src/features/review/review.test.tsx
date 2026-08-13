import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';

describe('review actions', () => {
  afterEach(() => vi.unstubAllGlobals());
  const selectRow = (row: HTMLElement) => fireEvent.click(within(row).getByRole('button', { name: /select/i }));
  it('keeps editing local until a nonblank filename is approved', async () => {
    const baseBridge = createInMemoryBridge();
    const approve = vi.fn(baseBridge.approve);
    const bridge = { ...baseBridge, approve };
    render(<App bridge={bridge} />);
    selectRow(await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i }));
    const filename = screen.getByLabelText('Filename');
    fireEvent.change(filename, { target: { value: '' } });

    expect(approve).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: /Approve & rename/i }));

    expect(screen.getByRole('alert')).toHaveTextContent('Filename is required');
    expect((await bridge.listItems()).find((item) => item.id === 'lease')?.status).toBe('review');
  });

  // The description is half of what Intern produces, and it used to be rendered
  // only while an item was still ready or in review. Applying a rename made the
  // item `completed`, which hid the sentence for good - so the fact describing
  // the document disappeared exactly when the document was filed.
  it('still shows the description after a file has been renamed', async () => {
    render(<App bridge={createInMemoryBridge()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Completed' }));
    selectRow(await screen.findByRole('row', { name: /Completed lease.pdf/i }));

    const inspector = screen.getByRole('complementary', { name: 'Review item' });
    expect(within(inspector).getByText(/Residential lease agreement for a twelve-month term/i)).toBeVisible();
    // Settled, so it is reported rather than edited - but it must be reachable.
    expect(within(inspector).queryByLabelText('Description')).not.toBeInTheDocument();
    expect(within(inspector).getByRole('button', { name: /Copy description/i })).toBeEnabled();
  });

  it('shows each description in the queue without needing a row selected', async () => {
    render(<App bridge={createInMemoryBridge()} />);
    const row = await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i });

    expect(within(row).getByText(/Commercial lease agreement between landlord and tenant/i)).toBeVisible();
  });

  it('preserves a draft through a same-revision queue refresh', async () => {
    const bridge = createInMemoryBridge();
    render(<App bridge={bridge} />);
    selectRow(await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i }));
    const filename = screen.getByLabelText('Filename');
    fireEvent.change(filename, { target: { value: 'My local draft.pdf' } });
    fireEvent.click(screen.getByRole('button', { name: 'Pause queue' }));

    await waitFor(() => expect(filename).toHaveValue('My local draft.pdf'));
  });

  it('traps keyboard focus in settings and restores it to the invoking control', async () => {
    render(<App bridge={createInMemoryBridge()} />);
    const trigger = (await screen.findAllByRole('button', { name: 'Settings' }))[0];
    fireEvent.click(trigger);

    const destination = await screen.findByLabelText('Destination folder');
    await waitFor(() => expect(destination).toHaveFocus());
    // Tabbing off the LAST control must wrap to the first. Found dynamically
    // rather than named, because this used to hard-code "Save settings" and
    // broke the moment the dialog grew an Updates section - which tested the
    // button order, not the trap.
    const dialog = screen.getByRole('dialog', { name: 'Settings' });
    const focusable = Array.from(dialog.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled])'));
    expect(focusable.length).toBeGreaterThan(2);
    focusable[focusable.length - 1].focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(screen.getByRole('button', { name: 'Close settings' })).toHaveFocus();
    fireEvent.keyDown(document, { key: 'Escape' });

    expect(screen.queryByRole('dialog', { name: 'Settings' })).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it('turns the narrow inspector into a contained drawer and restores row focus', async () => {
    vi.stubGlobal('matchMedia', vi.fn(() => ({
      matches: true,
      media: '(max-width: 1100px)',
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })));
    render(<App bridge={createInMemoryBridge()} />);

    const initialDrawer = await screen.findByRole('dialog', { name: 'Review item' });
    await waitFor(() => expect(screen.getByLabelText('Filename')).toHaveFocus());
    expect(screen.getByRole('banner')).toHaveAttribute('inert');
    expect(screen.getByRole('navigation', { name: 'Queue navigation' })).toHaveAttribute('inert');

    const lastAction = screen.getByRole('button', { name: 'More review actions' });
    lastAction.focus();
    fireEvent.keyDown(initialDrawer, { key: 'Tab' });
    expect(screen.getByRole('button', { name: 'Close review' })).toHaveFocus();
    fireEvent.keyDown(initialDrawer, { key: 'Escape' });

    const trigger = screen.getByRole('button', { name: 'Select Lease Agreement - 123 Main St.pdf' });
    fireEvent.click(trigger);
    const reopenedDrawer = await screen.findByRole('dialog', { name: 'Review item' });
    await waitFor(() => expect(screen.getByLabelText('Filename')).toHaveFocus());
    fireEvent.keyDown(reopenedDrawer, { key: 'Escape' });

    expect(screen.queryByRole('dialog', { name: 'Review item' })).not.toBeInTheDocument();
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it('moves focus to a visible queue control when refresh removes the invoking row', async () => {
    const bridge = createInMemoryBridge();
    render(<App bridge={bridge} />);
    const trigger = await screen.findByRole('button', { name: 'Select Lease Agreement - 123 Main St.pdf' });
    fireEvent.click(trigger);
    await bridge.remove('lease');
    fireEvent.click(screen.getByRole('button', { name: 'Pause queue' }));

    await waitFor(() => expect(screen.queryByRole('complementary', { name: 'Review item' })).not.toBeInTheDocument());
    await waitFor(() => expect(screen.getByRole('button', { name: 'Apply all ready' })).toHaveFocus());
  });

  it('saves the automatic high-confidence rename setting', async () => {
    const base = createInMemoryBridge();
    const saveSettings = vi.fn(base.saveSettings);
    render(<App bridge={{ ...base, saveSettings }} />);
    fireEvent.click((await screen.findAllByRole('button', { name: 'Settings' }))[0]);

    fireEvent.click(await screen.findByLabelText('Automatically rename high-confidence files'));
    fireEvent.click(screen.getByRole('button', { name: 'Save settings' }));

    await waitFor(() => expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ automaticRename: true })));
  });

  it('moves Keep original to Completed and lets the user undo it', async () => {
    const bridge = createInMemoryBridge();
    render(<App bridge={bridge} />);
    selectRow(await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i }));
    fireEvent.click(screen.getByRole('button', { name: /Keep original/i }));
    fireEvent.click(await screen.findByRole('button', { name: /^Completed/ }));
    selectRow(await screen.findByRole('row', { name: /Lease Agreement - 123 Main St.pdf/i }));
    fireEvent.click(screen.getByRole('button', { name: 'Undo' }));

    expect((await bridge.listItems()).find((item) => item.id === 'lease')?.status).toBe('review');
  });
});
