import { expect, it, vi } from 'vitest';
import { browserId } from './browserId';
it('creates cleanup operation IDs on HTTP where randomUUID is absent', () => {
  const original = crypto;
  vi.stubGlobal('crypto', { getRandomValues: original.getRandomValues.bind(original) });
  try {
    const values = Array.from({ length: 40 }, browserId);
    expect(new Set(values).size).toBe(40);
    for (const value of values) expect(value).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  } finally { vi.unstubAllGlobals(); }
});
