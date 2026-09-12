"""Helpers shared by the provisioning recipes: `fetch-deps.py` (ORT runtime + espeak),
`fetch-ocr-models.py` (the Cloud Reader OCR pair) and `build-espeak.py`.

Small on purpose. The point of unifying those scripts was that the RECIPE -- pins, digests,
checks -- must exist once; a helper copied into each file would be the same mistake in
miniature, so anything more than one of them needs lives here instead. `download` in
particular has a retry policy, and two copies of a retry policy is two behaviours.
"""

import hashlib
import os
import shutil
import ssl
import stat
import sys
import time
import urllib.request

USER_AGENT = {"User-Agent": "Kokoro-Kindle-Reader-dependency-provisioner/1.0"}


def fail(msg):
    """Print to stderr and exit non-zero. Provisioning failures are not exceptions to be
    caught somewhere else -- every one of them means the cache must not be marked.

    Flushes stdout first: it is block-buffered whenever it is not a terminal (a pipe, a CI
    log) while stderr is not, so without this the failure prints ABOVE the progress lines
    it is about -- which reads as a different, earlier failure."""
    sys.stdout.flush()
    print(msg, file=sys.stderr)
    raise SystemExit(1)


def sha256_file(path):
    """SHA-256 of a file's bytes, read in blocks so a 300 MB model is not held in memory."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def sha256_text(path):
    """SHA-256 with newlines normalized, so a checkout policy cannot change the digest.

    For text we hash to identify a *recipe* rather than to verify a download: the same
    script checked out with CRLF has to produce the same digest as one with LF, or the
    provision marker would invalidate itself on a fresh clone.
    """
    raw = open(path, "rb").read().replace(b"\r\n", b"\n").replace(b"\r", b"\n")
    return hashlib.sha256(raw).hexdigest()


def os_root_context():
    """A verifying SSL context built from the OS root store itself, or None.

    Exists because `ssl.create_default_context()` is not always the OS's own answer. On
    Windows `load_default_certs()` pulls in more than the ROOT store - the cached
    intermediate ("CA") store as well - and OpenSSL can then build a path through one of
    those and fail, while a valid path through the real roots was available all along.

    Measured on the dev machine: the default context loads 155 certificates and REJECTS
    downloads.sourceforge.net with CERTIFICATE_VERIFY_FAILED; a context built from the 101
    certificates in `enum_certificates("ROOT")` accepts it, along with PyPI, HuggingFace and
    GitHub. That server's chain is fine - it is Let's Encrypt's newer ISRG Root YE / X2,
    every certificate valid - so this is a local trust-store problem, and one that will get
    more common as that transition proceeds.

    `enum_certificates` is Windows-only; elsewhere the default context already reads the
    system trust store directly and there is nothing to correct, so this returns None and
    the caller keeps the error it had.
    """
    if not hasattr(ssl, "enum_certificates"):
        return None
    try:
        pem = "\n".join(ssl.DER_cert_to_PEM_cert(der)
                        for der, _encoding, _trust in ssl.enum_certificates("ROOT"))
    except Exception:  # noqa: BLE001 - a store we cannot read is not a store we can fix with
        return None
    return ssl.create_default_context(cadata=pem) if pem else None


def download(url, dest, attempts=3):
    """Fetch one immutable file, retrying.

    Every pinned artifact this project fetches is content-addressed by a digest the caller
    checks afterwards, so a retry can only ever produce the same bytes or fail. A flaky
    runner should not fail a provision that a second attempt would complete.

    A certificate-verification failure is retried ONCE against the OS root store (see
    `os_root_context`) rather than counted as a flake - repeating an identical handshake
    against an identical trust store cannot succeed, so without this a stale intermediate
    cache just burns the attempts and reports the same opaque error three times.
    **Verification is never disabled**: the retry uses a stricter, more accurate trust set,
    not a weaker one.
    """
    context = None
    for attempt in range(attempts):
        try:
            req = urllib.request.Request(url, headers=USER_AGENT)
            with urllib.request.urlopen(req, timeout=60, context=context) as r, \
                    open(dest, "wb") as f:
                shutil.copyfileobj(r, f)
            return
        except urllib.error.URLError as e:
            if context is None and isinstance(getattr(e, "reason", None), ssl.SSLError):
                context = os_root_context()
                if context is not None:
                    print("==> TLS verification failed (%s); retrying against the OS root "
                          "store" % e.reason)
                    continue
            if attempt == attempts - 1:
                fail("downloading %s failed: %s" % (url, e))
            time.sleep(2 * (attempt + 1))
        except Exception as e:  # noqa: BLE001 - every other failure is worth a retry
            if attempt == attempts - 1:
                fail("downloading %s failed: %s" % (url, e))
            time.sleep(2 * (attempt + 1))


def _clear_readonly(func, path, _exc):
    """Removal-failure handler: clear the read-only bit and try the removal again.

    Signature is deliberately compatible with BOTH `shutil.rmtree(onerror=...)` and its
    3.12+ replacement `onexc=...`, which differ only in what they pass as the third
    argument -- and this ignores it.
    """
    os.chmod(path, stat.S_IWRITE)
    func(path)


def rmtree_force(path):
    """`shutil.rmtree` that also removes read-only files, as `Remove-Item -Force` does.

    This exists because the plain version does not, and the difference is not academic: a
    CMake build tree contains `_deps/*-src/.git/objects/pack/*.idx`, and git marks pack
    files READ-ONLY. On Windows `os.unlink` then fails with "Access is denied" partway
    through the delete, leaving the build tree half-removed -- which is worse than not
    starting, because the artifacts the host links against are already gone by then.
    Measured: it deleted espeak's DLL, import lib and espeak-ng-data before failing.

    The PowerShell script this recipe replaced used `-Force` and never hit it; the port
    dropped that and the bug arrived with it.
    """
    if not os.path.exists(path):
        return
    if sys.version_info >= (3, 12):
        shutil.rmtree(path, onexc=_clear_readonly)
    else:
        shutil.rmtree(path, onerror=_clear_readonly)
