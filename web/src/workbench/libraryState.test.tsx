import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, expect, it } from 'vitest';
import { LibraryStateScope, useLibraryState } from './libraryState';
const root = document.createElement('div');
afterEach(() => { render(null, root); history.replaceState(null, '', '/'); });
it('keeps reading selections scoped to application instance and preserves navigation state', async () => {
  history.replaceState({ router: { id: 'keep' }, libraryReading: { entry: 'legacy' } }, '', '/history');
  let store: ReturnType<typeof useLibraryState>;
  function Probe() { store = useLibraryState(); return null; }
  await act(async () => render(<LibraryStateScope.Provider value="first"><Probe /></LibraryStateScope.Provider>, root));
  expect(store!.read().libraryReading).toBeUndefined();
  store!.replace({ libraryReading: { entry: 'first-entry' } });
  await act(async () => render(<LibraryStateScope.Provider value="second"><Probe /></LibraryStateScope.Provider>, root));
  expect(store!.read().libraryReading).toBeUndefined();
  store!.replace({ libraryReading: { entry: 'second-entry' } });
  expect(history.state.router).toEqual({ id: 'keep' });
  expect(history.state.libraryReading).toEqual({ entry: 'legacy' });
  expect(history.state['wb-library:first'].libraryReading).toEqual({ entry: 'first-entry' });
  expect(history.state['wb-library:second'].libraryReading).toEqual({ entry: 'second-entry' });
});
