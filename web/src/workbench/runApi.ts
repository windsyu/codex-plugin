import { parseRoute } from './routes';

/** Build a workbench API path scoped to the run in the current page URL. */
export function runApi(path: string): string {
  if (!path.startsWith('/')) throw new Error('run API path must be relative');
  const route = parseRoute(new URL(location.href));
  if (route.kind === 'missing') throw new Error('invalid run id');
  return route.kind === 'run' ? `/workbench/v1/runs/${route.runId}${path}` : `/workbench/v1${path}`;
}
