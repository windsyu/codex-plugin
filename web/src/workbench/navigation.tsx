import type { ComponentChildren } from 'preact';
import { useLayoutEffect, useState } from 'preact/hooks';
import { navigationURL } from './context';
import { parseRoute } from './routes';
export const NAVIGATE = 'workbench-navigate';
export function navigatePage(path: string, replace = false) {
  const href = navigationURL(path);
  history[replace ? 'replaceState' : 'pushState']({ ...history.state }, '', href);
  window.dispatchEvent(new Event(NAVIGATE));
}
export function useRoute() {
  const [route, setRoute] = useState(() => parseRoute(new URL(location.href)));
  useLayoutEffect(() => {
    const update = () => setRoute(parseRoute(new URL(location.href)));
    addEventListener('popstate', update); addEventListener(NAVIGATE, update);
    return () => { removeEventListener('popstate', update); removeEventListener(NAVIGATE, update); };
  }, []);
  return route;
}
export function PageLink({ path, active, children, className = '' }: { path: string; active?: boolean; children: ComponentChildren; className?: string }) {
  return <a href={navigationURL(path)} className={className} aria-current={active ? 'page' : undefined} onClick={event => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || event.defaultPrevented) return;
    event.preventDefault(); navigatePage(path);
  }}>{children}</a>;
}
