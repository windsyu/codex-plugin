import { describe, expect, it } from 'vitest';
import { parseRoute, runPages } from './routes';
const id = '11111111-1111-4111-8111-111111111111';
const gateway = '22222222-2222-4222-8222-222222222222';
const route = (path: string) => parseRoute(new URL(path, 'https://synthetic.test'));
describe('explicit document routes', () => {
  it.each(runPages)('accepts local and gateway-scoped %s with the same run identity', page => {
    expect(route(`/runs/${id}/${page}`)).toEqual({ kind: 'run', runId: id, page });
    expect(route(`/g/${gateway}/runs/${id}/${page}`)).toEqual({ kind: 'run', runId: id, page });
  });
  it.each(['/', '/history', '/launch', '/settings', '/relay'])('accepts application page %s', path => expect(route(path).kind).toBe('home'));
  it.each(['/unknown', `/runs/${id}`, `/runs/${id}/chat/extra`, `/runs/${id}/unknown`, '/runs/00000000-0000-0000-0000-000000000000/chat', '/runs/AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA/chat', `/g/${gateway}/relay`, '/g/invalid/history', `/runs/${id}/chat?run=${gateway}`])('rejects non-contract path %s', path => expect(route(path)).toEqual({ kind: 'missing' }));
  it('recognizes the old run entry only at the application root', () => {
    expect(route(`/?run=${id}`)).toEqual({ kind: 'run', runId: id, page: 'chat', legacy: true });
    expect(route(`/history?run=${id}`)).toEqual({ kind: 'missing' });
  });
});
