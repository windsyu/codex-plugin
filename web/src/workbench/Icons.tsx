import { codicons } from './icons/codicons';

const icons = {
  chat: 'comment-discussion', history: 'history', files: 'files',
  folder: 'folder', folderOpen: 'folder-opened', search: 'search', git: 'source-control', usage: 'graph',
  chevron: 'chevron-right', refresh: 'refresh', collapse: 'collapse-all', close: 'close', open: 'go-to-file',
  file: 'file', code: 'file-code', text: 'file-text', config: 'json', markdown: 'markdown', image: 'file-media', unavailable: 'circle-slash',
} as const satisfies Record<string, keyof typeof codicons>;

export type IconName = keyof typeof icons;

export function Icon({ name, className = '' }: { name: IconName; className?: string }) {
  const icon = codicons[icons[name]];
  return <svg className={`wb-icon ${className}`} width="16" height="16" viewBox={icon.viewBox} fill="currentColor" aria-hidden="true" focusable="false">{icon.paths.map((path, index) => <path key={index} d={path.d} fill-rule={path.fillRule} clip-rule={path.clipRule} />)}</svg>;
}

export function FileIcon({ name, unavailable = false }: { name: string; unavailable?: boolean }) {
  const ext = name.split('.').pop()?.toLowerCase() || '';
  const kind = unavailable ? 'unavailable'
    : /^(tsx?|jsx?|rs|py|go|java|c|cpp|h|rb|sh|html|css|vue|svelte)$/.test(ext) ? 'code'
    : /^(json|toml|ya?ml|xml|ini|lock)$/.test(ext) ? 'config'
    : /^(md|mdx)$/.test(ext) ? 'markdown'
    : /^(txt|rst)$/.test(ext) ? 'text'
    : /^(png|jpe?g|gif|webp|svg|ico)$/.test(ext) ? 'image' : 'file';
  return <Icon name={kind} className={`wb-file-icon is-${kind}`} />;
}
