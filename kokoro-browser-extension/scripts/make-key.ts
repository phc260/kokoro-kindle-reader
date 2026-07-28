// Generate the pinned extension identity.
//
// An unpacked extension's ID is derived from its absolute path, so it changes on every machine
// and every time the folder moves. kokoro-host's HTTP endpoint allowlists that ID as an origin
// (see kokoro-host/src/webserve.rs), so an unpinned extension means editing the allowlist
// constantly - and a mismatch surfaces only as a 403 on every request.
//
// Putting the public key in the manifest's `key` field pins the ID to the key instead. The ID is
// the first 128 bits of SHA-256 over the DER SPKI public key, hex mapped 0-9a-f -> a-p.
//
//   bun run make-key
//
// The private key is only needed to package a .crx; it is written to key.pem (gitignored) so a
// signed package stays possible later. Losing it costs nothing today.

import { generateKeyPairSync, createHash } from 'node:crypto';
import { existsSync } from 'node:fs';
import path from 'node:path';

const root = path.join(import.meta.dir, '..');
const pemPath = path.join(root, 'key.pem');
const manifestPath = path.join(root, 'manifest.chrome.json');

if (existsSync(pemPath) && !process.argv.includes('--force')) {
  console.error(`${path.relative(root, pemPath)} already exists - refusing to overwrite.`);
  console.error('Regenerating changes the extension ID and breaks existing registrations.');
  console.error('Pass --force if that is genuinely what you want.');
  process.exit(1);
}

const { publicKey, privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });

const spki = publicKey.export({ type: 'spki', format: 'der' }) as Buffer;
const keyField = spki.toString('base64');

// Chrome's ID derivation.
const digest = createHash('sha256').update(spki).digest('hex').slice(0, 32);
const id = [...digest].map((c) => 'abcdefghijklmnop'[parseInt(c, 16)]).join('');

await Bun.write(pemPath, privateKey.export({ type: 'pkcs8', format: 'pem' }).toString());

const manifest = JSON.parse(await Bun.file(manifestPath).text());
// `key` must sit near the top for readability; rebuild the object rather than appending.
const pinned = { manifest_version: manifest.manifest_version, name: manifest.name, version: manifest.version, description: manifest.description, key: keyField, ...manifest };
pinned.key = keyField;
await Bun.write(manifestPath, JSON.stringify(pinned, null, 2));

console.log(`extension id : ${id}`);
console.log(`private key  : ${path.relative(root, pemPath)} (gitignored)`);
console.log(`manifest     : key added to ${path.basename(manifestPath)}`);
console.log(
  `\nThe id is not written to a file on purpose - exactly one place consumes it, and a stale` +
    `\ncopy elsewhere is worse than none. Put it in DEFAULT_EXTENSION_ID in` +
    `\nkokoro-host/src/webserve.rs (the origin allowlist), rebuild the host, then re-run` +
    `\n\`bun run build\`, reload the extension, and confirm chrome://extensions shows this id.` +
    `\nA mismatch shows up only as 403 on every request.`,
);
