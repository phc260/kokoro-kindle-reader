// Minimal USTAR reader. The reader fetches each batch of pages from /renderer/render as an
// uncompressed tar, so indexing one is the first step in understanding what the client is
// actually given. ~40 lines beats a dependency.

export interface TarEntry {
  name: string;
  size: number;
  /** Byte offset of the entry's data within the archive. */
  offset: number;
  type: string;
}

const BLOCK = 512;

function str(bytes: Uint8Array, from: number, len: number): string {
  const slice = bytes.subarray(from, from + len);
  const end = slice.indexOf(0);
  return new TextDecoder().decode(end === -1 ? slice : slice.subarray(0, end)).trim();
}

/** Octal header fields; blank/!octal means 0. */
function octal(bytes: Uint8Array, from: number, len: number): number {
  const s = str(bytes, from, len).replace(/[^0-7]/g, '');
  return s ? parseInt(s, 8) : 0;
}

export function listTar(buf: ArrayBuffer | Uint8Array): TarEntry[] {
  const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
  const out: TarEntry[] = [];

  for (let off = 0; off + BLOCK <= bytes.length; ) {
    const name = str(bytes, off, 100);
    if (!name) break; // two zero blocks terminate the archive

    const size = octal(bytes, off + 124, 12);
    const type = str(bytes, off + 156, 1) || '0';
    // USTAR prefix, for paths longer than 100 bytes.
    const prefix = str(bytes, off + 345, 155);

    out.push({ name: prefix ? `${prefix}/${name}` : name, size, offset: off + BLOCK, type });
    off += BLOCK + Math.ceil(size / BLOCK) * BLOCK;
  }

  return out;
}

export function readEntry(buf: ArrayBuffer | Uint8Array, entry: TarEntry): Uint8Array {
  const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
  return bytes.subarray(entry.offset, entry.offset + entry.size);
}

export function readEntryText(buf: ArrayBuffer | Uint8Array, entry: TarEntry): string {
  return new TextDecoder().decode(readEntry(buf, entry));
}
