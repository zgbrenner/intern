import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { App } from '../../App';
import { TauriBridge, type TauriEvent, type TauriTransport } from '../../lib/tauriBridge';

const settings = { destination: 'C:\\Filed', destinationLayout: 'flat' as const, startMinimized: false, automaticRename: false, intakeFolder: '', intakeEnabled: false, processOthersUploads: false, machineLabel: '', runInBackground: false, startAtLogin: false, recordDescriptions: false, modelSource: 'local' as const, hostedProvider: 'anthropic' as const, hostedBaseUrl: '', hostedModel: '' };

function desktopTransport() {
  const commands: string[] = [];
  const responses: Record<string, unknown> = {
    queue_list: [],
    settings_get: settings,
    setup_get: { state: 'ready', downloadedBytes: 0, totalBytes: 0 },
  };
  const transport: TauriTransport = {
    invoke: async <T,>(command: string) => { commands.push(command); return responses[command] as T; },
    listen: async <T,>(_event: string, _handler: (event: TauriEvent<T>) => void) => () => undefined,
  };
  return { transport, commands };
}

/*
  The in-memory bridge is a bag of closures, so passing `bridge.pauseQueue`
  around works there. The desktop bridge is a class whose methods reach for
  `this.transport`, and an unbound method call left that undefined - Pause
  threw in the shipped app while every test went on passing.
*/
describe('pausing through the desktop bridge', () => {
  it('pauses and resumes when the bridge is the class the desktop app uses', async () => {
    const { transport, commands } = desktopTransport();
    render(<App bridge={new TauriBridge(transport)} />);

    fireEvent.click(await screen.findByRole('button', { name: 'Pause queue' }));

    await waitFor(() => expect(commands).toContain('queue_pause'));
    expect(screen.queryByRole('alert', { name: 'Action error' })).not.toBeInTheDocument();

    fireEvent.click(await screen.findByRole('button', { name: 'Resume queue' }));

    await waitFor(() => expect(commands).toContain('queue_resume'));
    expect(screen.queryByRole('alert', { name: 'Action error' })).not.toBeInTheDocument();
  });
});
