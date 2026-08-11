import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { App } from './App';

describe('App', () => {
  it('exposes the Intern application landmark', () => {
    render(<App />);

    expect(screen.getByRole('main', { name: 'Intern' })).toBeInTheDocument();
  });

  it('exposes one unambiguous Settings action', async () => {
    render(<App />);

    expect(await screen.findAllByRole('button', { name: 'Settings' })).toHaveLength(1);
  });
});
