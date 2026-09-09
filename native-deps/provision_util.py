"""Filesystem helpers shared by the provisioning recipe (`fetch-deps.py`) and the espeak
build recipe (`build-espeak.py`).

Small on purpose. The point of unifying those two scripts was that the RECIPE -- pins,
digests, checks -- must exist once; a helper copied into both files would be the same
mistake in miniature, so anything either of them needs lives here instead.
"""

import os
import shutil
import stat
import sys


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
