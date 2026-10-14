import { createContext } from 'preact';
import { useContext } from 'preact/hooks';
import { scopedStorageKey } from './context';
export const LibraryStateScope = createContext<string | undefined>(undefined);
/** Keep library paging/reading separate from navigation and other accounts/instances. */
export function useLibraryState() {
  const instance = useContext(LibraryStateScope);
  const key = instance ? scopedStorageKey(`wb-library:${instance}`) : undefined;
  const read = () => (key ? history.state?.[key] : history.state) || {};
  const merge = (value: Record<string, unknown>) => key
    ? { ...history.state, [key]: { ...read(), ...value } }
    : { ...history.state, ...value };
  return { read, merge, replace: (value: Record<string, unknown>) => history.replaceState(merge(value), '') };
}
