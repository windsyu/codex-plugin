import { codicons } from './icons/codicons';

const icons = {
  chat: 'comment-discussion', history: 'history', files: 'files',
  folder: 'folder', folderOpen: 'folder-opened', search: 'search', git: 'source-control', usage: 'graph',
  chevron: 'chevron-right', refresh: 'refresh', collapse: 'collapse-all', close: 'close', open: 'go-to-file',
  file: 'file', code: 'file-code', text: 'file-text', config: 'json', markdown: 'markdown', image: 'file-media', unavailable: 'circle-slash',
} as const satisfies Record<string, keyof typeof codicons>;

const outlineIcons = {
  monitor: 'M3 4h18v12H3z M8 21h8 M12 16v5',
  devices: 'M3 3h13v12H3z M7 19h5 M9 15v4 M18 9h4v12h-4z',
  globe: 'M3 12h18 M12 3c6 5 6 13 0 18 M12 3c-6 5-6 13 0 18',
  settings: 'M9 3h6l1 3 3 1 2 5-2 5-3 1-1 3H9l-1-3-3-1-2-5 2-5 3-1z',
  menu: 'M4 6h16 M4 12h16 M4 18h16',
  terminal: 'm5 7 5 5-5 5 M13 17h6',
  dock: 'M3 4h18v16H3z M3 14h18',
} as const;
export type IconName = keyof typeof icons | keyof typeof outlineIcons;

export function Icon({ name, className = '' }: { name: IconName; className?: string }) {
  if (name in outlineIcons) return <svg className={`wb-icon ${className}`} width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">{name === 'globe' && <circle cx="12" cy="12" r="9" />}<path d={outlineIcons[name as keyof typeof outlineIcons]} />{name === 'settings' && <circle cx="12" cy="12" r="3" />}</svg>;
  const icon = codicons[icons[name as keyof typeof icons]];
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
