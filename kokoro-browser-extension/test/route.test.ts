// Which page of the Cloud Reader are we on?
//
// This is the whole basis for where the panel appears, and it is two different kinds of test: the
// reader is a QUERY PARAM (`?asin=`) and the library is a PATH (`/kindle-library`). Getting either
// wrong fails silently and identically - the panel simply never mounts, on a page that looks
// exactly like the one it should have mounted on - so the shapes are pinned here rather than
// rediscovered against a live Amazon page.

import { test, expect, afterAll } from 'bun:test';

/** Stand in for the document's location; `route()` reads hostname, pathname and search off it. */
function at(href: string): void {
  const u = new URL(href);
  (globalThis as { location?: unknown }).location = {
    href: u.href,
    hostname: u.hostname,
    pathname: u.pathname,
    search: u.search,
  };
}

// capture.ts reads `location` when called, not at import, but the import is still done under a
// stub so nothing in this file depends on the order the module graph happens to load in.
at('https://read.amazon.com/kindle-library');
const { route } = await import('../src/content/capture');

afterAll(() => {
  delete (globalThis as { location?: unknown }).location;
});

test('the library is recognized, and it is not the reader', () => {
  at('https://read.amazon.com/kindle-library');
  expect(route()).toMatchObject({ onLibrary: true, onReader: false, asin: null });
});

test('the library keeps its identity under a sub-path and a query', () => {
  // The shelf pushes filter/sort state into the URL; none of it makes this a different page.
  at('https://read.amazon.com/kindle-library/search?query=rabbit');
  expect(route().onLibrary).toBe(true);
  at('https://read.amazon.com/kindle-library?sortType=recency');
  expect(route().onLibrary).toBe(true);
});

test('an open book is the reader, not the library', () => {
  at('https://read.amazon.com/?asin=B0TEST1234');
  expect(route()).toMatchObject({ onReader: true, onLibrary: false, asin: 'B0TEST1234' });
});

test('a path that merely starts with the library name is not the library', () => {
  // `startsWith` would take this; the trailing boundary in LIBRARY_PATH is what refuses it.
  at('https://read.amazon.com/kindle-library-something-else');
  expect(route().onLibrary).toBe(false);
});

test('neither fact survives a different site', () => {
  // The content script is matched on read.amazon.com only, but `route()` is also the guard the
  // console path and the panel mount both go through - it may not assume its own manifest.
  at('https://www.amazon.com/kindle-library');
  expect(route()).toMatchObject({ onLibrary: false, onReader: false });

  // The reader's name as a PREFIX of someone else's domain. `[a-z.]+$` used to take this.
  at('https://read.amazon.com.evil.test/kindle-library');
  expect(route().onLibrary).toBe(false);
  at('https://notread.amazon.com/kindle-library');
  expect(route().onLibrary).toBe(false);
});

test('the regional readers are the same site, as far as route() is concerned', () => {
  // NB this is a claim about the REGEX, not about shipped support: the Chrome manifest injects on
  // `https://read.amazon.com/*` only, so the content script never runs on these hosts today. The
  // rule still has to hold - `route()` is the guard the console path and the panel mount read, and
  // tightening the host check must not be done by assuming a single TLD.
  at('https://read.amazon.co.uk/kindle-library');
  expect(route().onLibrary).toBe(true);
  at('https://read.amazon.com.br/kindle-library');
  expect(route().onLibrary).toBe(true);
  at('https://read.amazon.de/kindle-library');
  expect(route().onLibrary).toBe(true);
  at('https://read.amazon.co.jp/?asin=B0TEST1234');
  expect(route().onReader).toBe(true);
});
