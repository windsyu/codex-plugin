// Acceptance probes support both Application Run URLs and standalone readers.
function runApiPath(url, tail) {
  if (!tail.startsWith('/') || tail.startsWith('//')) throw new Error('invalid probe API tail');
  const id = new URL(url).searchParams.get('run');
  if (id !== null && !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(id)) {
    throw new Error('invalid probe Run identity');
  }
  return `/workbench/v1${id === null ? '' : `/runs/${id}`}${tail}`;
}
async function readRun(page, tail) {
  return page.evaluate(async path => {
    const response = await fetch(path, { credentials: 'same-origin', cache: 'no-store' });
    if (!response.ok) throw new Error(`probe API failed with status ${response.status}`);
    return response.json();
  }, runApiPath(page.url(), tail));
}
module.exports = { runApiPath, readRun };
