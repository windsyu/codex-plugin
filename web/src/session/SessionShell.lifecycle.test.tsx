import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, expect, it, vi } from 'vitest';
import { Api } from '../api';
import type { SessionWorker } from '../types';
import { SessionShell } from './SessionShell';

const stream = vi.hoisted(() => ({ emit: undefined as undefined | ((event: unknown) => void) }));
vi.mock('../api', async (original) => ({
  ...await original<typeof import('../api')>(),
  reconnectingStream: (_api: unknown, _path: string, emit: (event: unknown) => void, _status: unknown, signal: AbortSignal) => {
    stream.emit = emit;
    return new Promise<void>((resolve) => signal.addEventListener('abort', () => resolve()));
  }
}));
vi.mock('./TerminalPanel', () => ({
  TerminalPanel: ({ leaseId, onConnected }: { leaseId?: string; onConnected: () => Promise<unknown> }) =>
    <button onClick={() => void onConnected()} data-testid="terminal">{leaseId ? 'input-enabled' : 'input-disabled'}</button>
}));

let container: HTMLDivElement;
afterEach(() => {
  if (container) { act(() => render(null, container)); container.remove(); }
  sessionStorage.clear(); stream.emit = undefined;
});

it('keeps its input lease when the pre-attachment SSE stream publishes a connecting Worker', async () => {
  const worker: SessionWorker = {
    workerId: 'worker-starting', state: 'connecting', cwd: '/fixture', rows: 24, cols: 80,
    outputSeq: 0, terminalRetainedBytes: 0, terminalCheckpointBytes: 0, ptyEof: false,
    inputLease: { version: 0, state: 'none' }
  };
  let finishAttach!: (value: unknown) => void;
  const api = {
    get: vi.fn(async () => ({ data: [{ sourceId: 'source', sourceEpoch: 'epoch', storeSourceId: 'store',
      supervisorVersion: 1, defaultCwd: '/fixture', status: 'ready' }] })),
    post: vi.fn((path: string) => {
      if (path === '/v2/sessions') return Promise.resolve({ data: worker });
      if (path.endsWith('/attach')) return new Promise((resolve) => { finishAttach = resolve; });
      return Promise.resolve({ data: { version: 1, state: 'active', leaseId: 'input', ownerAttachmentId: 'attachment' } });
    })
  } as unknown as Api;
  container = document.createElement('div'); document.body.append(container);
  await act(async () => { render(<SessionShell api={api} intent={{ mode: 'new' }} onClose={() => {}} />, container); });
  await vi.waitFor(() => expect(container.querySelector('form')).not.toBeNull());
  expect(api.post).not.toHaveBeenCalled();
  await act(async () => { container.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); });
  await vi.waitFor(() => expect(stream.emit).toBeTypeOf('function'));
  await act(async () => {
    finishAttach({ data: { attachmentId: 'attachment', attachmentToken: 'b'.repeat(64), descriptor: 'descriptor', inputLeaseVersion: 0 } });
  });
  await vi.waitFor(() => expect(container.querySelector('[data-testid="terminal"]')).not.toBeNull());
  await act(async () => { (container.querySelector('[data-testid="terminal"]') as HTMLButtonElement).click(); });
  expect(container.textContent).toContain('input-enabled');
  act(() => stream.emit?.({ type: 'session_state', data: { ...worker,
    inputLease: { version: 1, state: 'active', leaseId: 'input', ownerAttachmentId: 'attachment' } } }));
  expect(container.textContent).toContain('input-enabled');
  act(() => stream.emit?.({ type: 'session_state', data: worker }));
  expect(container.textContent).toContain('input-enabled');
  // A genuinely different owner must still remove input, even during startup.
  act(() => stream.emit?.({ type: 'session_state', data: { ...worker,
    inputLease: { version: 2, state: 'active', leaseId: 'other-input', ownerAttachmentId: 'other' } } }));
  expect(container.textContent).toContain('input-disabled');
});
