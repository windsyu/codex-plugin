import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawnSync } from 'node:child_process';

export const RELEASE_TARGET = 'aarch64-apple-darwin';
const binaries = ['codex-view', 'codex-observerd'];
const licenseName = /^(?:licen[cs]e|copying|copyright|notice)(?:[._-]|$)/i;

function command(program, args, cwd) {
  const result = spawnSync(program, args, { cwd, encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  if (result.error || result.status !== 0) throw new Error(`${program} failed: ${result.error?.message || result.stderr.trim()}`);
  return result.stdout.trim();
}
export function releaseTarget(repo) {
  const host = command('rustc', ['-vV'], repo).match(/^host: (.+)$/m)?.[1];
  if (process.platform !== 'darwin' || host !== RELEASE_TARGET) throw new Error(`Release packaging supports only tested ${RELEASE_TARGET}; got ${host}`);
  return host;
}
function copy(source, destination, executable = false) {
  if (!fs.lstatSync(source).isFile()) throw new Error(`Not a regular release input: ${source}`);
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  fs.copyFileSync(source, destination, fs.constants.COPYFILE_EXCL);
  fs.chmodSync(destination, executable ? 0o755 : 0o644);
}
function files(directory, prefix = '') {
  return fs.readdirSync(directory).sort().flatMap(name => {
    const relative = path.join(prefix, name);
    const stat = fs.lstatSync(path.join(directory, name));
    if (stat.isSymbolicLink()) throw new Error(`Symlink in release inputs: ${relative}`);
    return stat.isDirectory() ? files(path.join(directory, name), relative) : [relative];
  });
}

// Preserve actual upstream notices, including bundled native sources' notices.
export function collectLicenseFiles(directory) {
  return files(directory).filter(file => licenseName.test(path.basename(file)));
}
function runtimeNotices(entry) {
  if (!entry || !/^[A-Za-z0-9][A-Za-z0-9._+-]*$/.test(entry.name) || !/^[A-Za-z0-9][A-Za-z0-9._+-]*$/.test(entry.version) ||
      typeof entry.declaredLicense !== 'string' || !entry.declaredLicense.trim() || !Array.isArray(entry.noticeFiles) || !entry.noticeFiles.length) {
    throw new Error('Missing or invalid static runtime license inputs');
  }
  const directory = path.resolve(entry.directory);
  const directoryStat = fs.lstatSync(directory);
  if (directoryStat.isSymbolicLink()) throw new Error(`Symlink in runtime inputs: ${entry.name}`);
  if (!directoryStat.isDirectory()) throw new Error(`Not a runtime source directory: ${entry.name}`);
  const seen = new Set();
  for (const file of entry.noticeFiles) {
    if (typeof file !== 'string' || !/^[A-Za-z0-9_@+.,/-]+$/.test(file) || path.isAbsolute(file) || file.split('/').some(part => !part || part === '.' || part === '..')) {
      throw new Error(`License path escapes ${entry.name}`);
    }
    if (seen.has(file)) throw new Error(`Duplicate runtime notice: ${entry.name}`);
    seen.add(file);
    let source = directory;
    const parts = file.split('/');
    for (let index = 0; index < parts.length; index++) {
      source = path.join(source, parts[index]);
      const stat = fs.lstatSync(source);
      if (stat.isSymbolicLink()) throw new Error(`Symlink in runtime inputs: ${entry.name}`);
      if (!(index === parts.length - 1 ? stat.isFile() : stat.isDirectory())) throw new Error(`Not a regular runtime notice: ${entry.name}`);
    }
    if (fs.readFileSync(source).includes(0)) throw new Error(`Non-text license for ${entry.name}: ${file}`);
  }
  return { ...entry, directory, noticeFiles: [...seen].sort() };
}

// Cargo metadata does not describe the standard library or compiler builtins
// statically linked from rustc's own sysroot. Preserve their original notices.
export function macosRustRuntimeLicenses(repo) {
  const version = command('rustc', ['-vV'], repo).match(/^release: (.+)$/m)?.[1];
  if (!/^\d+\.\d+\.\d+$/.test(version || '')) throw new Error('Expected a stable Rust runtime version');
  const directory = command('rustc', ['--print', 'sysroot'], repo);
  if (!path.isAbsolute(directory)) throw new Error('Expected an absolute Rust sysroot');
  const doc = 'share/doc/rust';
  const builtins = 'lib/rustlib/src/rust/library/compiler-builtins/LICENSE.txt';
  const entries = [
    { name: 'rust-library', version, declaredLicense: 'SEE COPYRIGHT-library.html', directory,
      noticeFiles: [`${doc}/COPYRIGHT-library.html`, ...['MIT', 'Apache-2.0', 'Unicode-3.0', 'BSD-2-Clause', 'LLVM-exception'].map(name => `${doc}/licenses/${name}.txt`)] },
    { name: 'rust-compiler-builtins', version, declaredLicense: 'MIT AND Apache-2.0 WITH LLVM-exception', directory,
      noticeFiles: [builtins, 'lib/rustlib/src/rust/library/compiler-builtins/libm/LICENSE.txt'] },
  ].map(runtimeNotices);
  for (const entry of entries) {
    for (const file of entry.noticeFiles) {
      if (!fs.readFileSync(path.join(directory, file), 'utf8').trim()) throw new Error(`Empty Rust runtime notice: ${file}`);
    }
  }
  if (!fs.readFileSync(path.join(directory, doc, 'COPYRIGHT-library.html'), 'utf8').includes('Copyright notices for The Rust Standard Library') ||
      !fs.readFileSync(path.join(directory, builtins), 'utf8').includes('MIT AND Apache-2.0 WITH')) {
    throw new Error('Unrecognized Rust runtime notices; review the actual sysroot before packaging');
  }
  return entries;
}
export function writeLicenses(repo, metadata, destination, { runtimeLicenses = [] } = {}) {
  if (!Array.isArray(runtimeLicenses)) throw new Error('Invalid static runtime license list');
  const runtimes = runtimeLicenses.map(runtimeNotices);
  const identities = new Set();
  for (const runtime of runtimes) {
    const identity = `${runtime.name}@${runtime.version}`;
    if (identities.has(identity)) throw new Error(`Duplicate runtime license: ${identity}`);
    identities.add(identity);
  }
  const entries = [];
  const write = (ecosystem, name, version, declaredLicense, directory, explicitFile) => {
    const notices = collectLicenseFiles(directory);
    if (explicitFile && !notices.includes(explicitFile)) notices.push(explicitFile);
    if (!declaredLicense || !notices.length) throw new Error(`Missing license/notice for ${ecosystem} ${name}@${version}`);
    const output = path.join('licenses', ecosystem, `${name.replaceAll('/', '__')}@${version}`);
    const licenseFiles = notices.sort().map(file => {
      const source = path.resolve(directory, file);
      const relative = path.relative(directory, source);
      if (relative.startsWith('..') || path.isAbsolute(relative)) throw new Error(`License path escapes ${name}`);
      if (fs.readFileSync(source).includes(0)) throw new Error(`Non-text license for ${name}: ${file}`);
      copy(source, path.join(destination, output, file));
      return path.join(output, file);
    });
    entries.push({ ecosystem, name, version, declaredLicense, licenseFiles });
  };
  for (const pkg of metadata.packages) {
    if (pkg.id === metadata.resolve.root) continue;
    write('cargo', pkg.name, pkg.version, pkg.license || 'SEE LICENSE FILE', path.dirname(pkg.manifest_path), pkg.license_file);
  }
  const lock = JSON.parse(fs.readFileSync(path.join(repo, 'web/package-lock.json'), 'utf8'));
  for (const [location, pkg] of Object.entries(lock.packages)) {
    if (!location || pkg.dev) continue;
    const directory = path.join(repo, 'web', location);
    if (!fs.existsSync(directory)) {
      if (pkg.optional) continue;
      throw new Error(`Missing locked web dependency: ${location}; run bootstrap`);
    }
    const installed = JSON.parse(fs.readFileSync(path.join(directory, 'package.json'), 'utf8'));
    if (installed.version !== pkg.version) throw new Error(`Web lock mismatch: ${location}`);
    write('npm', installed.name, installed.version, pkg.license || installed.license, directory);
  }
  // Static system/toolchain runtimes are not represented in Cargo/npm metadata.
  // The target-specific packager must supply actual upstream notices explicitly.
  for (const runtime of runtimes) {
    const output = path.join('licenses', 'toolchain-runtime', `${runtime.name}@${runtime.version}`);
    const licenseFiles = runtime.noticeFiles.map(file => {
      const relative = path.join(output, file);
      copy(path.join(runtime.directory, file), path.join(destination, relative));
      return relative;
    });
    entries.push({ ecosystem: 'toolchain-runtime', name: runtime.name, version: runtime.version, declaredLicense: runtime.declaredLicense, licenseFiles });
  }
  fs.mkdirSync(path.join(destination, 'licenses'), { recursive: true });
  fs.writeFileSync(path.join(destination, 'licenses/INDEX.json'), `${JSON.stringify(entries, null, 2)}\n`);
  return entries.length;
}
export function writeChecksums(directory) {
  const checksums = files(directory).filter(file => file !== 'CHECKSUMS.sha256').map(file => {
    if (/[\r\n\\]/.test(file)) throw new Error(`Unsupported checksum filename: ${file}`);
    return `${crypto.createHash('sha256').update(fs.readFileSync(path.join(directory, file))).digest('hex')}  ${file}`;
  });
  fs.writeFileSync(path.join(directory, 'CHECKSUMS.sha256'), `${checksums.join('\n')}\n`);
}

export function packageRelease(repo, artifacts, { offline = false } = {}) {
  const host = releaseTarget(repo);
  const version = fs.readFileSync(path.join(repo, 'Cargo.toml'), 'utf8').match(/^version = "([^"]+)"$/m)?.[1];
  if (!/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(version) || version !== version.trim()) throw new Error('Expected stable semver release version');
  const web = JSON.parse(fs.readFileSync(path.join(repo, 'web/package.json'), 'utf8'));
  if (web.version !== version) throw new Error('Rust and Web release versions differ');
  const runtimeLicenses = macosRustRuntimeLicenses(repo);
  const metadata = JSON.parse(command('cargo', ['metadata', '--locked', ...(offline ? ['--offline'] : []), '--filter-platform', host, '--format-version', '1'], repo));
  const name = `code-view-${version}-${host}`;
  const bundle = path.join(artifacts, name);
  fs.mkdirSync(bundle);
  for (const binary of binaries) {
    const source = path.join(metadata.target_directory, host, 'release', binary);
    if (command(source, ['--version'], repo) !== `${binary} ${version}`) throw new Error(`Stale release binary: ${binary}`);
    const libraries = command('otool', ['-L', source], repo).split('\n').slice(1).map(line => line.trim().split(' (')[0]);
    if (libraries.some(library => !library.startsWith('/usr/lib/') && !library.startsWith('/System/Library/'))) throw new Error(`Non-system dynamic dependency in ${binary}: ${libraries.join(', ')}`);
    copy(source, path.join(bundle, 'bin', binary), true);
  }
  copy(path.join(repo, 'scripts/release/code-view'), path.join(bundle, 'bin/code-view'), true);
  copy(path.join(repo, 'scripts/release/install.sh'), path.join(bundle, 'install.sh'), true);
  for (const file of ['LICENSE', 'THIRD_PARTY_NOTICES.md', 'CHANGELOG.md', 'Cargo.lock', 'web/package-lock.json', 'vendor/vt100/LICENSE', 'vendor/vt100/PATCH.md', 'web/src/workbench/icons/LICENSE', 'web/src/workbench/icons/README.md']) copy(path.join(repo, file), path.join(bundle, file));
  const licenseCount = writeLicenses(repo, metadata, bundle, { runtimeLicenses });
  const commit = command('git', ['rev-parse', 'HEAD'], repo);
  const dirty = !!command('git', ['status', '--porcelain'], repo);
  fs.writeFileSync(path.join(bundle, 'manifest.json'), `${JSON.stringify({ format: 1, name: 'code-view', version, target: host, sourceCommit: commit, sourceDirty: dirty, licenseCount }, null, 2)}\n`);
  fs.writeFileSync(path.join(bundle, 'RELEASE.md'), `# code-view ${version}

Codex View for macOS / Apple Silicon. This package contains code-view, codex-view and the read-only Observer compatibility entry codex-observerd; it does not contain the separate codex-relayd server. Web assets and the native folder picker are embedded. No Rust, Node.js or source checkout is needed to run.

Install from the extracted directory:

\`\`\`sh
shasum -a 256 -c CHECKSUMS.sha256
./install.sh --prefix "$HOME/.codex-view"
export PATH="$HOME/.codex-view/bin:$PATH"
cd /path/to/project
code-view
# Or: code-view /path/to/project
# History only: code-view --history
\`\`\`

The installer never edits shell files. Add the PATH line to your shell configuration if needed. Use --force only for an intentional replacement. Before upgrading, stop each owned background scope with code-view stop, or exit its foreground launcher with Ctrl-C. Installation does not stop running services automatically.

code-view start starts a background workbench without creating projects. Use code-view status, code-view open and code-view stop to inspect, open and stop it; stop also ends its owned CLI projects. code-view autostart on|off changes login startup preference, not the current process. Autostart runs after user login and does not promise operation while logged out, asleep or powered off. For scripts, use --no-open to suppress browser opening. Ordinary project launch stays in the foreground unless it reuses an existing background instance; keep its launcher terminal open. Closing a browser keeps projects running.

Project launch requires a separately installed official Codex CLI. Default OpenAI/ChatGPT launches use native direct mode: the terminal and native history remain available, while live model capture and call inspection are unavailable. Native configuration selects the provider; the workbench does not rewrite native configuration, switch providers, or disable native WebSocket transport. Capture preflight checks only fields required for fixed-upstream forwarding, authentication sources, and policy safety; unrelated configuration remains with the CLI. Supported custom Responses providers retain proxy capture. CLI 0.162.1 command authentication was tested with a synthetic provider; real-provider command authentication remains unverified. See the source-linked support documentation for exact routes, authentication modes, tested CLI versions, and remaining limits. Search needs rg; Git views need git.

Optional Codex Relay access requires matching computer, server and browser versions: workbench UI/API 3, wire transport 3 and relay schema 3. Business content is encrypted between browser and computer in separate Application and per-Run channels; the relay backend forwards ciphertext without business keys. The centrally served frontend and authorization service remain trusted: this does not protect against an operator modifying scripts or authorization, or hide metadata, lengths or timing. Upgrade all three components together and reopen older pages from the computer portal; there is no plaintext fallback. Preserve accounts, computer registrations and history; do not bypass version checks or delete data to upgrade. This View package does not deploy or migrate the relay server.

Configuration and history remain under ~/.codex-web; native Codex and old Observer data are not migrated. Derived history catalog 4 can be rebuilt; single-record project organization uses separate schema 1 state, and JSON configuration remains schema 2. Linux, Windows and Intel macOS are unverified. Actual logout/login startup, sleep/resume and full real-phone/public-network scenarios remain outside current validation.

Source: ${commit}${dirty ? ' (local changes included; not a published main tag)' : ''}. Package checksums verify transfer integrity; this package is not signed or notarized. Formal publishing requires a reviewed main tag and passing CI. Full usage and limits: [project documentation](https://github.com/windsyu/codex-plugin/tree/${commit}/docs).

The licenses directory includes all host-resolved Cargo dependencies (including build/test dependencies), installed Web runtime dependencies, and original Rust standard-library/compiler-builtins notices from the build toolchain. Runtime index versions identify the Rust toolchain release. The standard-library notice includes upstream cross-platform material without claiming that every listed component is linked. Development Web tools are not shipped. See THIRD_PARTY_NOTICES.md and licenses/INDEX.json for upstream attribution and notices.
`);
  writeChecksums(bundle);
  const archive = path.join(artifacts, `${name}.tar.gz`);
  command('tar', ['-czf', archive, '-C', artifacts, name], repo);
  const digest = crypto.createHash('sha256').update(fs.readFileSync(archive)).digest('hex');
  const checksum = path.join(artifacts, 'SHA256SUMS');
  fs.writeFileSync(checksum, `${digest}  ${path.basename(archive)}\n`);
  return { archive, checksum, bundle, version, target: host, licenseCount, sourceCommit: commit, sourceDirty: dirty };
}
