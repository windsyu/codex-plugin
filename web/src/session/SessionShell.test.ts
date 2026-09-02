import { describe, expect, it } from 'vitest';

import type { SessionWorker } from '../types';
import { sessionOwnsInitialThread } from './SessionShell';

const worker: SessionWorker = {
  workerId: 'worker-1',
  state: 'ready',
  cwd: '/fixture',
  rows: 24,
  cols: 80,
  outputSeq: 1,
  terminalRetainedBytes: 0,
  terminalCheckpointBytes: 0,
  ptyEof: false,
  inputLease: { version: 0, state: 'none' },
  persistedWorker: {
    version: 4,
    sourceId: 'source-1',
    sourceEpoch: 'epoch-1',
    primaryThreadId: 'thread-1'
  },
  threadLeases: [{
    leaseId: 'lease-1',
    codexThreadId: 'thread-1',
    role: 'primary',
    state: 'active',
    version: 2
  }]
};

describe('SessionShell ownership restore', () => {
  it('reattaches only when the stored worker owns the exact source epoch and Thread', () => {
    expect(sessionOwnsInitialThread(worker)).toBe(true);
    expect(sessionOwnsInitialThread(worker, {
      sourceId: 'source-1', sourceEpoch: 'epoch-1', codexThreadId: 'thread-1'
    })).toBe(true);
    expect(sessionOwnsInitialThread(worker, {
      sourceId: 'source-1', sourceEpoch: 'epoch-stale', codexThreadId: 'thread-1'
    })).toBe(false);
    expect(sessionOwnsInitialThread(worker, {
      sourceId: 'source-1', sourceEpoch: 'epoch-1', codexThreadId: 'thread-2'
    })).toBe(false);
    expect(sessionOwnsInitialThread({ ...worker, persistedWorker: undefined }, {
      sourceId: 'source-1', sourceEpoch: 'epoch-1', codexThreadId: 'thread-1'
    })).toBe(false);
  });
});
