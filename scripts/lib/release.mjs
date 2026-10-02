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
export function writeLicenses(repo, metadata, destination) {
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
  if (!/^0\.\d+\.\d+$/.test(version)) throw new Error('Expected pre-1.0 semver release version');
  const web = JSON.parse(fs.readFileSync(path.join(repo, 'web/package.json'), 'utf8'));
  if (web.version !== version) throw new Error('Rust and Web release versions differ');
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
  const licenseCount = writeLicenses(repo, metadata, bundle);
  const commit = command('git', ['rev-parse', 'HEAD'], repo);
  const dirty = !!command('git', ['status', '--porcelain'], repo);
  fs.writeFileSync(path.join(bundle, 'manifest.json'), `${JSON.stringify({ format: 1, name: 'code-view', version, target: host, sourceCommit: commit, sourceDirty: dirty, licenseCount }, null, 2)}\n`);
  fs.writeFileSync(path.join(bundle, 'RELEASE.md'), `# code-view ${version}\n\nmacOS / Apple Silicon release package. Web assets and the native folder picker are embedded. No Rust, Node.js or source checkout is needed to run.\n\nInstall from the extracted directory:\n\n\`\`\`sh\nshasum -a 256 -c CHECKSUMS.sha256\n./install.sh --prefix "$HOME/.local"\nexport PATH="$HOME/.local/bin:$PATH"\ncd /path/to/project\ncode-view\n# Or: code-view /path/to/project\n# History only: code-view --history\n\`\`\`\n\nThe installer never edits shell files. Add the PATH line to your shell configuration if needed. Use --force only for an intentional replacement. Keep the launcher terminal open; Ctrl-C stops its application and owned CLIs. Closing the browser keeps them running.\n\nProject launch requires a separately installed official Codex CLI and the validated unmanaged-custom Responses/static-bearer configuration. Tested CLI versions: 0.154.0, 0.155.1, 0.156.1, 0.159.2. Search needs rg; Git views need git. The default data directory remains ~/.codex-web; native Codex data is not migrated. Linux, Windows and Intel macOS are unverified.\n\nSource: ${commit}${dirty ? ' (local changes included; not a published main tag)' : ''}. Package checksums verify transfer integrity; this package is not signed or notarized. Formal publishing requires a reviewed main tag and passing CI. Full usage and limits: [project documentation](https://github.com/windsyu/codex-plugin/tree/main/docs).\n\nThe licenses directory includes all host-resolved Cargo dependencies (including build/test dependencies) and installed Web runtime dependencies. Development Web tools are not shipped. See THIRD_PARTY_NOTICES.md and licenses/INDEX.json for upstream attribution and notices.\n`);
  writeChecksums(bundle);
  const archive = path.join(artifacts, `${name}.tar.gz`);
  command('tar', ['-czf', archive, '-C', artifacts, name], repo);
  const digest = crypto.createHash('sha256').update(fs.readFileSync(archive)).digest('hex');
  const checksum = path.join(artifacts, 'SHA256SUMS');
  fs.writeFileSync(checksum, `${digest}  ${path.basename(archive)}\n`);
  return { archive, checksum, bundle, version, target: host, licenseCount, sourceCommit: commit, sourceDirty: dirty };
}
