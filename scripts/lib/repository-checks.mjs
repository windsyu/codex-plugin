import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const git = (root, args, encoding = 'utf8') => execFileSync('git', ['-C', root, ...args], { encoding, stdio: ['pipe', 'pipe', 'pipe'], maxBuffer: 512 * 1024 * 1024 });
const utf8 = new TextDecoder('utf-8', { fatal: true });
function image(b) {
  if (b.length >= 45 && b.subarray(0, 8).equals(Buffer.from('89504e470d0a1a0a', 'hex'))) {
    let p = 8; let dimensions = false; let data = false;
    while (p + 12 <= b.length) {
      const n = b.readUInt32BE(p); const type = b.toString('ascii', p + 4, p + 8);
      if (n > b.length - p - 12) return false;
      if (p === 8) dimensions = type === 'IHDR' && n === 13 && b.readUInt32BE(p + 8) > 0 && b.readUInt32BE(p + 12) > 0;
      if (type === 'IDAT') data = true;
      p += n + 12;
      if (type === 'IEND') return dimensions && data && n === 0 && p === b.length;
    }
    return false;
  }
  if (b.length >= 12 && b[0] === 255 && b[1] === 216 && b[b.length - 2] === 255 && b[b.length - 1] === 217) {
    let p = 2; let frame = false;
    while (p + 4 < b.length) {
      if (b[p++] !== 255) return false;
      while (b[p] === 255) p++;
      const marker = b[p++];
      if (p + 2 > b.length) return false;
      if (marker === 0xda) return frame && b.readUInt16BE(p) >= 2;
      if (marker === 0xd9) return false;
      const size = b.readUInt16BE(p);
      if (size < 2 || p + size > b.length) return false;
      if ([0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf].includes(marker)) frame = size >= 8 && b.readUInt16BE(p + 3) > 0 && b.readUInt16BE(p + 5) > 0;
      p += size;
    }
  }
  if (b.length >= 14 && /^GIF8[79]a$/.test(b.toString('ascii', 0, 6))) return b.readUInt16LE(6) > 0 && b.readUInt16LE(8) > 0 && b[b.length - 1] === 0x3b && b.includes(0x2c, 13);
  if (b.length >= 20 && b.toString('ascii', 0, 4) === 'RIFF' && b.toString('ascii', 8, 12) === 'WEBP') return b.readUInt32LE(4) + 8 === b.length && ['VP8 ', 'VP8L', 'VP8X'].includes(b.toString('ascii', 12, 16)) && b.readUInt32LE(16) <= b.length - 20;
  if (b.length >= 54 && b.toString('ascii', 0, 2) === 'BM') return b.readUInt32LE(2) === b.length && b.readUInt32LE(10) < b.length && b.readUInt32LE(14) >= 40 && b.readInt32LE(18) > 0 && b.readInt32LE(22) !== 0;
  if (b.length >= 22 && b.readUInt32LE(0) === 0x10000) {
    const n = b.readUInt16LE(4); if (!n || 6 + n * 16 > b.length) return false;
    for (let p = 6; p < 6 + n * 16; p += 16) { const size = b.readUInt32LE(p + 8); const offset = b.readUInt32LE(p + 12); if (!size || offset < 6 + n * 16 || offset + size > b.length) return false; }
    return true;
  }
  if (b.length >= 10 && ['49492a00', '4d4d002a'].includes(b.subarray(0, 4).toString('hex'))) {
    const le = b[0] === 73; const offset = le ? b.readUInt32LE(4) : b.readUInt32BE(4);
    if (offset < 8 || offset + 2 > b.length) return false;
    const count = le ? b.readUInt16LE(offset) : b.readUInt16BE(offset);
    return count > 0 && offset + 2 + count * 12 + 4 <= b.length;
  }
  if (b.length >= 24 && b.toString('ascii', 4, 8) === 'ftyp') {
    const size = b.readUInt32BE(0);
    if (size < 16 || size > b.length || size % 4) return false;
    const brands = [b.toString('ascii', 8, 12)];
    for (let p = 16; p < size; p += 4) brands.push(b.toString('ascii', p, p + 4));
    return brands.some(x => ['avif', 'avis', 'heic', 'heix', 'mif1'].includes(x)) && size < b.length;
  }
  return false;
}
function binary(b) {
  // PDF may be entirely ASCII; its container format is still a binary artifact.
  if (b.subarray(0, 5).toString('ascii') === '%PDF-' || b.includes(0)) return true;
  try { const s = utf8.decode(b); return /[\x01-\x08\x0b\x0e-\x1f]/.test(s); } catch { return true; }
}
function base64File(text) {
  // Join conventional encoded lines, never natural-language words separated by
  // spaces or arbitrarily short/unequal lines. Decoding alone cannot prove intent.
  const lines = text.trim().split(/\r?\n/);
  if (!lines.every(line => /^[A-Za-z0-9+/]+={0,2}$/.test(line))) return null;
  const width = lines[0].length;
  if (lines.length > 1 && (width < 16 || width % 4 !== 0
    || lines.slice(0, -1).some(line => line.length !== width || line.includes('='))
    || lines.at(-1).length > width)) return null;
  const encoded = lines.join('');
  return encoded.length >= 16 ? encoded : null;
}
export function classifyContent(b) {
  if (image(b)) return null;
  if (binary(b)) return 'non-image binary content';
  const s = b.toString('utf8');
  // Detect complete base64 files, data URIs (64+ chars), and long tokens.
  // Small embedded synthetic fixtures are intentionally not treated as stored artifacts.
  const candidates = [];
  const encodedFile = base64File(s);
  if (encodedFile) candidates.push(encodedFile);
  for (const m of s.matchAll(/data:[^\s,"'<>;]*;base64,([A-Za-z0-9+/]+={0,2})/g)) if (m[1].length >= 64) candidates.push(m[1]);
  for (const m of s.matchAll(/[A-Za-z0-9+/]{1024,}={0,2}/g)) candidates.push(m[0]);
  for (const candidate of candidates) {
    const decoded = Buffer.from(candidate, 'base64');
    if (decoded.toString('base64').replace(/=+$/, '') === candidate.replace(/=+$/, '') && binary(decoded) && !image(decoded)) return 'base64-encoded non-image binary content';
  }
  return null;
}
export function checkBinaries(root, { mode = 'staged', ref = 'HEAD' } = {}) {
  let entries = [];
  if (mode === 'staged') {
    const changed = new Set(git(root, ['diff', '--cached', '--name-only', '--diff-filter=ACMR', '-z']).split('\0').filter(Boolean));
    entries = git(root, ['ls-files', '--stage', '-z']).split('\0').filter(Boolean).map(x => { const separator = x.indexOf('\t'); const head = x.slice(0, separator); return { path: x.slice(separator + 1), oid: head.split(' ')[1], mode: head.split(' ')[0] }; }).filter(x => changed.has(x.path) && x.mode !== '160000');
  } else if (mode === 'tree') {
    const tree = git(root, ['rev-parse', '--verify', '--end-of-options', `${ref}^{tree}`]).trim();
    entries = git(root, ['ls-tree', '-r', '-z', tree]).split('\0').filter(Boolean).map(x => { const i = x.indexOf('\t'); const fields = x.slice(0, i).split(' '); return { path: x.slice(i + 1), oid: fields[2], type: fields[1] }; }).filter(x => x.type === 'blob');
  } else if (mode === 'history') {
    if (git(root, ['rev-parse', '--is-shallow-repository']).trim() === 'true') throw new Error('Cannot verify complete history in a shallow repository; fetch full history first.');
    const refs = git(root, ['for-each-ref', '--format=%(refname)', 'refs/heads/', 'refs/remotes/', 'refs/tags/']).trim().split('\n').filter(Boolean);
    if (refs.length) {
      const objects = git(root, ['rev-list', '--objects', ...refs]).trim().split('\n').filter(Boolean).map(x => { const i = x.indexOf(' '); return { oid: i < 0 ? x : x.slice(0, i), path: i < 0 ? '(unnamed object)' : x.slice(i + 1) }; });
      const types = execFileSync('git', ['-C', root, 'cat-file', '--batch-check=%(objecttype)'], { input: objects.map(x => x.oid).join('\n') + '\n', encoding: 'utf8', maxBuffer: 512 * 1024 * 1024 }).trim().split('\n');
      entries = objects.filter((_, i) => types[i] === 'blob');
    }
  } else throw new Error(`Unknown binary check mode: ${mode}`);
  const findings = []; const cache = new Map();
  for (const entry of entries) {
    if (!cache.has(entry.oid)) cache.set(entry.oid, classifyContent(git(root, ['cat-file', 'blob', entry.oid], null)));
    const reason = cache.get(entry.oid); if (reason) findings.push({ path: entry.path, oid: entry.oid, reason });
  }
  return { mode, errors: findings.length, checked: entries.length, uniqueBlobs: cache.size, findings };
}

const excluded = new Set(['.git', 'target', 'node_modules', 'vendor', 'dist', 'build', '.cache', 'playwright-report', 'test-results', 'observer-data', '.code-review-graph']);
function markdownFiles(root) {
  const candidates = new Set(git(root, ['ls-files', '--cached', '--others', '--exclude-standard', '-z']).split('\0').filter(Boolean));
  return [...candidates].filter(name => /\.md$/i.test(name) && !name.split('/').some(part => excluded.has(part))).flatMap(name => {
    let file = root;
    for (const part of name.split('/')) {
      file = path.join(file, part);
      try { if (fs.lstatSync(file).isSymbolicLink()) return []; }
      catch (error) { if (error.code === 'ENOENT' || error.code === 'ENOTDIR') return []; throw error; }
    }
    return fs.statSync(file).isFile() ? [file] : [];
  });
}
function slug(s) { return s.toLowerCase().replace(/<[^>]*>/g, '').replace(/&amp;/g, '&').replace(/[\p{P}\p{S}]/gu, ch => ch === '-' || ch === '_' ? ch : '').replace(/ /g, '-'); }
export function checkDocs(root) {
  const require = createRequire(path.join(path.dirname(fileURLToPath(import.meta.url)), '../../web/package.json'));
  const { marked } = require('marked');
  const files = markdownFiles(root); const docs = new Map(); const findings = [];
  const add = (file, reason) => findings.push({ path: path.relative(root, file), reason });
  for (const file of files) {
    const source = fs.readFileSync(file, 'utf8'); const tokens = marked.lexer(source); const anchors = new Set(); const counts = new Map(); const links = []; let depth = 0;
    marked.walkTokens(tokens, token => {
      // Marked removes list/quote container prefixes from code token raw text.
      // Inspect only parsed fenced blocks; indented code can contain literal fences.
      if (token.type === 'code' && token.codeBlockStyle !== 'indented') {
        const [firstLine, ...body] = token.raw.split('\n');
        const opening = firstLine.match(/^ {0,3}(`{3,}|~{3,})/);
        if (opening) {
          const closing = new RegExp(`^ {0,3}${opening[1][0]}{${opening[1].length},}[ \\t]*$`);
          if (!body.some(line => closing.test(line))) add(file, 'unclosed fenced code block');
        }
      }
      if (token.type === 'heading') {
        if (depth && token.depth > depth + 1) add(file, `heading level jumps from ${depth} to ${token.depth}`);
        depth = token.depth;
        const text = token.tokens?.map(t => t.text ?? t.raw ?? '').join('') ?? token.text;
        const base = slug(text); let anchor = base; let n = counts.get(base) ?? 0;
        while (anchors.has(anchor)) anchor = `${base}-${++n}`;
        counts.set(base, n); anchors.add(anchor);
      }
      if (token.type === 'link' || token.type === 'image') links.push(token.href);
      if (token.type === 'html') {
        for (const m of token.text.matchAll(/\b(?:id|name)\s*=\s*["']([^"']+)["']/g)) anchors.add(m[1]);
        for (const m of token.text.matchAll(/\b(?:href|src)\s*=\s*["']([^"']+)["']/g)) links.push(m[1]);
      }
    });
    docs.set(file, { anchors, links });
  }
  for (const [file, doc] of docs) for (const href of doc.links) {
    if (/^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(href)) continue;
    let decoded;
    try { decoded = decodeURIComponent(href); } catch { add(file, `invalid URL encoding: ${href}`); continue; }
    const hash = decoded.indexOf('#'); const pathname = (hash < 0 ? decoded : decoded.slice(0, hash)).split('?')[0]; const anchor = hash < 0 ? '' : decoded.slice(hash + 1);
    const target = pathname ? path.resolve(path.dirname(file), pathname) : file;
    if (!fs.existsSync(target)) { add(file, `missing relative link target: ${href}`); continue; }
    if (anchor && docs.has(target) && !docs.get(target).anchors.has(anchor)) add(file, `missing Markdown anchor: ${href}`);
  }
  return { errors: findings.length, checked: files.length, findings };
}
