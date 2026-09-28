import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { checkBinaries, checkDocs, classifyContent } from '../lib/repository-checks.mjs';
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aW6cAAAAASUVORK5CYII=', 'base64');
function repo(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'repository-checks-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const git = (...args) => execFileSync('git', ['-C', root, ...args], { encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] });
  git('init', '-b', 'main'); git('config', 'user.name', 'Synthetic Test'); git('config', 'user.email', 'test@example.invalid');
  return { root, git, write: (name, value) => { fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true }); fs.writeFileSync(path.join(root, name), value); } };
}
test('content permits genuine images and SVG, rejects fake image and extensionless binary', () => {
  assert.equal(classifyContent(png), null);
  assert.equal(classifyContent(Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"></svg>')), null);
  assert.match(classifyContent(Buffer.from([0x7f, 0x45, 0x4c, 0x46, 0])), /binary/);
  assert.match(classifyContent(Buffer.concat([png.subarray(0, 8), Buffer.from([0])])), /binary/);
  assert.match(classifyContent(Buffer.from([0xc3, 0x28])), /binary/);
  assert.match(classifyContent(Buffer.from('%PDF-1.4\nASCII-only PDF container')), /binary/);
});
test('base64 policy catches complete files, data URIs and long tokens, permits images and tiny inline fixtures', () => {
  const binary = Buffer.from(Array.from({ length: 1200 }, (_, i) => i % 256)); const encoded = binary.toString('base64');
  assert.match(classifyContent(Buffer.from(encoded)), /base64/);
  assert.match(classifyContent(Buffer.from(`const fixture = "${encoded}";`)), /base64/);
  assert.match(classifyContent(Buffer.from(`data:application/octet-stream;base64,${Buffer.alloc(96).toString('base64')}`)), /base64/);
  assert.equal(classifyContent(Buffer.from(png.toString('base64'))), null);
  assert.equal(classifyContent(Buffer.from('const fixture = "AAEC";')), null);
  assert.equal(classifyContent(Buffer.from('data:image/png;base64,AAAA) data:audio/wav;base64,AAAA')), null);
});
test('ordinary prose stays text in staged, tree and history checks', t => {
  const { root, git, write } = repo(t);
  write('plain.txt', 'This is a plain text file');
  write('NOTES', 'This\nis\na\nplain\ntext\nfile\n');
  write('paragraph.md', 'This is a plain text file\nThis is another text file\n');
  git('add', '.');
  assert.deepEqual(checkBinaries(root).findings, []);
  git('commit', '-m', 'plain text');
  assert.deepEqual(checkBinaries(root, { mode: 'tree' }).findings, []);
  assert.deepEqual(checkBinaries(root, { mode: 'history' }).findings, []);
});
test('base64 files retain single-line and regular line-wrapped detection', () => {
  const encoded = Buffer.from(Array.from({ length: 161 }, (_, i) => i % 256)).toString('base64');
  for (const width of [64, 76]) {
    const wrapped = encoded.match(new RegExp(`.{1,${width}}`, 'g')).join('\r\n');
    assert.match(classifyContent(Buffer.from(`${wrapped}\r\n`)), /base64/);
  }
  assert.match(classifyContent(Buffer.from(Buffer.alloc(12).toString('base64'))), /base64/);
  assert.equal(classifyContent(Buffer.from(png.toString('base64').match(/.{1,64}/g).join('\n'))), null);
});
test('staged checker reads index rather than working tree and ignores unchanged index files', t => {
  const { root, git, write } = repo(t);
  write('base.md', '# Base'); git('add', '.'); git('commit', '-m', 'base');
  write('picture.png', Buffer.from([0, 1, 2])); git('add', '.'); write('picture.png', png);
  assert.equal(checkBinaries(root).errors, 1);
  git('add', '.'); write('picture.png', Buffer.from([0, 1, 2]));
  assert.equal(checkBinaries(root).errors, 0);
  git('commit', '-m', 'image'); assert.equal(checkBinaries(root).checked, 0);
});
test('tree/history checker sees deleted binary and other branches regardless of attributes', t => {
  const { root, git, write } = repo(t);
  write('.gitattributes', '* text diff=conceal\n'); write('README.md', '# Test'); git('add', '.'); git('commit', '-m', 'base');
  git('checkout', '-b', 'other'); write('no-extension', Buffer.from([0, 1, 2])); git('add', '.'); git('commit', '-m', 'binary');
  git('rm', 'no-extension'); git('commit', '-m', 'delete binary'); git('checkout', 'main');
  assert.equal(checkBinaries(root, { mode: 'tree' }).errors, 0);
  const history = checkBinaries(root, { mode: 'history' }); assert.equal(history.errors, 1); assert.equal(history.findings[0].path, 'no-extension');
});
test('history excludes stash and auxiliary refs but includes tags', t => {
  const { root, git, write } = repo(t);
  write('README.md', '# Test'); git('add', '.'); git('commit', '-m', 'base');
  write('blob', Buffer.from([0, 1])); git('add', '.'); git('stash', 'push');
  assert.equal(checkBinaries(root, { mode: 'history' }).errors, 0);
  git('tag', 'historical', 'refs/stash'); assert.equal(checkBinaries(root, { mode: 'history' }).errors, 1);
});
test('docs handles Chinese, duplicate headings, HTML IDs, reference links and code examples', t => {
  const { root, write } = repo(t);
  write('README.md', '# 测试\n\n## 中文 标题\n\n## 中文 标题\n\n<a id="custom"></a>\n\n[one](#中文-标题) [two](#中文-标题-1) [id](#custom)\n\n[reference][x]\n\n[x]: guide.md#开始\n\n```md\n[not a link](missing.md)\n```\n');
  write('guide.md', '# 开始\n');
  assert.deepEqual(checkDocs(root).findings, []);
});
test('docs reports missing targets and anchors, heading skips, and unclosed fences', t => {
  const { root, write } = repo(t);
  write('README.md', '# Heading\n\n### Skip\n\n[x](missing.md) [x](#absent)\n\n```js\nunclosed\n');
  write('vendor/ignored.md', '[x](missing.md)');
  const report = checkDocs(root); assert.equal(report.checked, 1); assert.equal(report.errors, 4);
});
test('docs respects fences inside list and quote containers and literal indented code', t => {
  const { root, write } = repo(t);
  for (const [index, body] of [
    '- ```js\n  const x = 1;\n  ```\n',
    '1. ```js\n   const x = 1;\n   ```\n',
    '> - ~~~txt\n>   text\n>   ~~~\n',
    '- item\n  - ```js\n    const x = 1;\n    ```\n',
    '    ```js\n    literal indented code\n',
    '````md\n```js\nnot a closing delimiter\n`````\n',
  ].entries()) write(`case-${index}.md`, `# Test\n\n${body}`);
  assert.deepEqual(checkDocs(root).findings, []);
});
test('docs still rejects unclosed fences inside Markdown containers', t => {
  const { root, write } = repo(t);
  for (const [index, body] of [
    '- ```js\n  const x = 1;\n',
    '> ~~~txt\n> text\n',
    '1. ````js\n   x\n   ```\n',
    '```js\nx\n``` trailing\n',
  ].entries()) write(`case-${index}.md`, `# Test\n\n${body}`);
  const report = checkDocs(root);
  assert.equal(report.errors, 4);
  assert.ok(report.findings.every(finding => finding.reason === 'unclosed fenced code block'));
});
test('docs ignores private ignored Markdown and never follows symlinks', t => {
  const { root, write } = repo(t);
  write('.gitignore', 'private/\n');
  write('README.md', '# Public\n');
  write('private/secret.md', '# Private\n\n[x](does-not-exist.md)\n');
  write('observer-data/session.md', '# Private\n\n[x](does-not-exist.md)\n');
  write('.code-review-graph/report.md', '[x](does-not-exist.md)\n');
  fs.symlinkSync(path.join(root, 'private/secret.md'), path.join(root, 'linked.md'));
  const report = checkDocs(root);
  assert.equal(report.checked, 1); assert.deepEqual(report.findings, []);
});
test('tree refs are resolved as revisions, never Git options', t => {
  const { root, git, write } = repo(t);
  write('README.md', '# Public'); git('add', '.'); git('commit', '-m', 'base');
  assert.throws(() => checkBinaries(root, { mode: 'tree', ref: '--help' }));
  assert.equal(checkBinaries(root, { mode: 'tree', ref: 'main' }).errors, 0);
});
test('history refuses shallow repositories rather than claiming completeness', t => {
  const { root, git, write } = repo(t);
  write('README.md', '# Public'); git('add', '.'); git('commit', '-m', 'base');
  fs.writeFileSync(path.join(root, '.git/shallow'), git('rev-parse', 'HEAD'));
  assert.throws(() => checkBinaries(root, { mode: 'history' }), /shallow repository/);
});
