// The tar reader is the one piece of the network probe that can be tested without Amazon.
// Verified against a real archive produced by GNU tar.

import { test, expect } from 'bun:test';
import { listTar, readEntryText } from '../src/tar';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

async function makeTar(files: Record<string, string>): Promise<Uint8Array> {
  const dir = await mkdtemp(path.join(tmpdir(), 'kwr-tar-'));
  try {
    for (const [name, body] of Object.entries(files)) await writeFile(path.join(dir, name), body);
    // Relative paths with cwd, not absolute: GNU tar reads a Windows `C:\...` argument as a
    // remote `host:path` and refuses it.
    const proc = Bun.spawn(['tar', '-cf', 'archive.tar', ...Object.keys(files)], { cwd: dir, stderr: 'pipe' });
    if ((await proc.exited) !== 0) throw new Error(`tar failed: ${await new Response(proc.stderr).text()}`);
    return new Uint8Array(await Bun.file(path.join(dir, 'archive.tar')).arrayBuffer());
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

test('lists entries with names and sizes', async () => {
  const files = {
    'glyphs.json': JSON.stringify({ 42: 'M0 0 L10 10' }),
    'page_data_0.json': JSON.stringify({ words: [{ id: 42, x: 1, y: 2 }] }),
    'toc.json': '["chapter one"]',
  };
  const tar = await makeTar(files);
  const entries = listTar(tar);

  expect(entries.map((e) => e.name).sort()).toEqual(Object.keys(files).sort());
  for (const e of entries) expect(e.size).toBe(new TextEncoder().encode(files[e.name as keyof typeof files]).length);
});

test('reads entry contents back exactly', async () => {
  const body = JSON.stringify({ glyphs: Array.from({ length: 200 }, (_, i) => i) });
  const tar = await makeTar({ 'glyphs.json': body, 'small.json': '{}' });
  const entries = listTar(tar);

  const glyphs = entries.find((e) => e.name === 'glyphs.json')!;
  expect(readEntryText(tar, glyphs)).toBe(body);
  expect(readEntryText(tar, entries.find((e) => e.name === 'small.json')!)).toBe('{}');
});

test('handles an entry whose size is not a block multiple', async () => {
  // 513 bytes spans two blocks with one byte in the second - the padding maths has to be right
  // or every later entry is misaligned.
  const odd = 'x'.repeat(513);
  const tar = await makeTar({ 'a.txt': odd, 'b.txt': 'after' });
  const entries = listTar(tar);

  expect(readEntryText(tar, entries.find((e) => e.name === 'a.txt')!)).toBe(odd);
  expect(readEntryText(tar, entries.find((e) => e.name === 'b.txt')!)).toBe('after');
});

test('stops cleanly at the terminating zero blocks', async () => {
  const tar = await makeTar({ 'only.json': '{}' });
  expect(listTar(tar)).toHaveLength(1);
});

test('empty input yields no entries rather than throwing', () => {
  expect(listTar(new Uint8Array(0))).toEqual([]);
  expect(listTar(new Uint8Array(1024))).toEqual([]);
});
