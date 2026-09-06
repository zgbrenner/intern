import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { App } from '../../App';
import { createInMemoryBridge } from '../../lib/inMemoryBridge';
import type { LearnedRule } from '../../types';

async function openSettings() {
  fireEvent.click((await screen.findAllByRole('button', { name: 'Settings' }))[0]);
  return screen.findByRole('dialog', { name: 'Settings' });
}

const learned: LearnedRule[] = [
  { id: '1', kind: 'party', from: 'Vistage Worldwide, Inc.', to: 'Vistage', seen: 1, active: false, learnedAt: 1716282900 },
  { id: '2', kind: 'document_type', from: 'Quarterly Operations Review', to: 'Meeting Minutes', seen: 2, active: true, learnedAt: 1716196500 },
];

describe('the spellings Intern has learned', () => {
  it('says there is nothing to show until review has taught something', async () => {
    render(<App bridge={createInMemoryBridge()} />);
    const dialog = await openSettings();
    expect(await within(dialog).findByText(/Nothing yet\. Change a party's name or a document type in review/)).toBeVisible();
    expect(within(dialog).queryByRole('list', { name: 'Learned spellings' })).not.toBeInTheDocument();
  });

  it('lists each spelling with its state, uses one at once, and forgets one', async () => {
    const base = createInMemoryBridge({ learnedRules: learned });
    const houseRuleUse = vi.fn(base.houseRuleUse);
    const houseRuleForget = vi.fn(base.houseRuleForget);
    render(<App bridge={{ ...base, houseRuleUse, houseRuleForget }} />);
    const dialog = await openSettings();
    const list = await within(dialog).findByRole('list', { name: 'Learned spellings' });
    const rows = within(list).getAllByRole('listitem');
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent('Party');
    expect(rows[0]).toHaveTextContent('Vistage Worldwide, Inc. written as Vistage');
    expect(rows[0]).toHaveTextContent('Seen once · in use after one more edit');
    expect(rows[1]).toHaveTextContent('Type');
    expect(rows[1]).toHaveTextContent('In use');
    // A spelling already in use has nothing to hurry.
    expect(within(rows[1]).queryByRole('button', { name: /Use now/ })).not.toBeInTheDocument();

    fireEvent.click(within(rows[0]).getByRole('button', { name: 'Use now: Vistage Worldwide, Inc. written as Vistage' }));
    await waitFor(() => expect(houseRuleUse).toHaveBeenCalledWith('1'));
    await waitFor(() => expect(within(list).getAllByRole('listitem')[0]).toHaveTextContent('In use'));
    expect(within(within(list).getAllByRole('listitem')[0]).queryByRole('button', { name: /Use now/ })).not.toBeInTheDocument();

    fireEvent.click(within(within(list).getAllByRole('listitem')[1]).getByRole('button', { name: 'Forget: Quarterly Operations Review written as Meeting Minutes' }));
    await waitFor(() => expect(houseRuleForget).toHaveBeenCalledWith('2'));
    await waitFor(() => expect(within(list).getAllByRole('listitem')).toHaveLength(1));
    expect(within(list).getAllByRole('listitem')[0]).toHaveTextContent('Vistage');
  });

  it('reports a spelling that could not be changed and keeps the list', async () => {
    const base = createInMemoryBridge({ learnedRules: learned });
    const houseRuleForget = vi.fn(async () => { throw { code: 'DATABASE_UNAVAILABLE', message: 'the queue database is locked' }; });
    render(<App bridge={{ ...base, houseRuleForget }} />);
    const dialog = await openSettings();
    const list = await within(dialog).findByRole('list', { name: 'Learned spellings' });
    fireEvent.click(within(within(list).getAllByRole('listitem')[0]).getByRole('button', { name: /Forget:/ }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('The spelling could not be changed');
    expect(within(list).getAllByRole('listitem')).toHaveLength(2);
  });
});
