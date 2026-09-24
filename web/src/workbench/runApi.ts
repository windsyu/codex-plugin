const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

/** Build a workbench API path scoped to the run in the current page URL. */
export function runApi(path: string): string {
  if (!path.startsWith('/')) throw new Error('run API path must be relative');
  const run = new URLSearchParams(location.search).get('run');
  if (run === null) return `/workbench/v1${path}`;
  if (!UUID.test(run)) throw new Error('invalid run id');
  return `/workbench/v1/runs/${run}${path}`;
}
