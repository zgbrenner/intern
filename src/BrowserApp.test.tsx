import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

const picker = vi.hoisted(() => vi.fn(async () => []));

vi.mock('./lib/inMemoryBridge', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./lib/inMemoryBridge')>();
  return {
    ...actual,
    createBrowserSelectionBoundary: () => ({ pickFiles: picker, pickFolder: async () => undefined, resolveDrop: async () => ({}) }),
  };
});

import { BrowserApp } from './BrowserApp';

describe('BrowserApp', () => {
  it('starts the fixture E2E adapter empty so dropped files drive the run', async () => {
    window.history.replaceState({}, '', '/?fixtureBatch=1');

    render(<BrowserApp />);

    expect(await screen.findByText('0 items')).toBeVisible();
    expect(screen.queryByText('Lease Agreement - 123 Main St.pdf')).not.toBeInTheDocument();
    window.history.replaceState({}, '', '/');
  });

  it('injects the browser selection boundary into the platform-neutral App', async () => {
    render(<BrowserApp />);

    fireEvent.click(await screen.findByRole('button', { name: 'Add files' }));

    await waitFor(() => expect(picker).toHaveBeenCalledOnce());
  });
});
