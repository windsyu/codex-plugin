// Deployment units share protocol/UI, never the other unit's production runtime.
export const components = Object.freeze({
  workbench: Object.freeze({ package: 'codex-local-observer', binaries: ['codex-view', 'codex-observerd'] }),
  relay: Object.freeze({ package: 'codex-relayd', binaries: ['codex-relayd'] }),
});
const sharedPackages = ['codex-relay-protocol', 'codex-workbench-assets'];

export function cargoScope(component, { tests = false } = {}) {
  if (component === 'all') return ['--workspace'];
  const selected = components[component];
  if (!selected) throw new Error(`Unknown component: ${component}`);
  return [selected.package, ...(tests ? sharedPackages : [])].flatMap(name => ['-p', name]);
}

// The same ordered recipe drives real builds and synthetic orchestration tests.
export async function buildComponent(execute, { component, check = false, release = false, locked, target }) {
  const scope = cargoScope(component);
  const testScope = cargoScope(component, { tests: true });
  await execute('npm', ['run', 'build', '--prefix', 'web']);
  if (check) {
    // Formatting remains repository-wide; compilation/tests use the selected unit.
    await execute('cargo', ['fmt', '--all', '--check']);
    await execute('cargo', ['clippy', ...locked, ...testScope, '--all-targets', '--', '-D', 'warnings']);
    await execute('cargo', ['test', ...locked, ...testScope, '--all-targets', '--', '--test-threads=4']);
    await execute('cargo', ['build', ...locked, ...scope, '--bins']);
    await execute('cargo', ['build', ...locked, ...scope, '--release', '--bins']);
  } else {
    await execute('cargo', ['build', ...locked, ...scope, '--bins', ...(release ? ['--release'] : []), ...(target ? ['--target', target] : [])]);
  }
}

// Cargo --no-deps metadata is sufficient to inspect all workspace path edges.
// Include even optional production edges: future features must preserve isolation.
export function assertComponentBoundaries(metadata) {
  const byName = new Map(metadata.packages.map(pkg => [pkg.name, pkg]));
  const visit = (name, seen = new Set()) => {
    if (seen.has(name)) return seen;
    seen.add(name);
    const pkg = byName.get(name);
    if (!pkg) throw new Error(`Missing workspace package: ${name}`);
    for (const dependency of pkg.dependencies) {
      if (dependency.path && dependency.kind !== 'dev') visit(dependency.name, seen);
    }
    return seen;
  };
  const workbench = components.workbench.package, relay = components.relay.package;
  for (const [name, forbidden] of [[workbench, [relay]], [relay, [workbench]], ...sharedPackages.map(name => [name, [workbench, relay]])]) {
    const reachable = visit(name);
    for (const other of forbidden) if (reachable.has(other)) throw new Error(`Production dependency crosses deployment boundary: ${name} -> ${other}`);
  }
}
