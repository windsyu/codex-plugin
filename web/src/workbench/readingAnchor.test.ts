import { expect, it } from 'vitest';
import { readAnchor, restoreAnchor } from './readingAnchor';

it('preserves the visible stable card when late evidence is inserted ahead of it', () => {
  const list = document.createElement('div'), card = document.createElement('div');
  card.dataset.readingKey = 'model:stable'; list.append(card);
  let cardTop = 75;
  list.getBoundingClientRect = () => ({ top: 100 } as DOMRect);
  card.getBoundingClientRect = () => ({ top: cardTop, bottom: cardTop + 800 } as DOMRect);
  const anchor = readAnchor(list);
  expect(anchor).toEqual({ key: 'model:stable', offset: -25 });
  cardTop += 120;
  restoreAnchor(list, anchor);
  expect(list.scrollTop).toBe(120);
  card.remove();
  restoreAnchor(list, anchor);
  expect(list.scrollTop).toBe(120);
});
