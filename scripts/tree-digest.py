#!/usr/bin/env python3
"""Print a content digest of a git working tree, uncommitted and untracked files included.

    scripts/tree-digest.py <dir>

A candidate run (PARITY_MODE=candidate) tests a tree that is not a commit, so a revision
cannot say what was built. This digest can: it covers every file git would track — the
tracked files as they are on disk, plus untracked files that .gitignore does not exclude
— so two trees get the same digest only if those files have the same paths, contents and
executable bits. A `git diff` hash would miss new files.

Only .gitignore files decide what is left out, never .git/info/exclude or a user's
core.excludesFile: those live outside the tree, so a file hidden by them could change
what cargo builds without changing the digest. A symlink is refused: its target may be
ignored, a directory, or outside the tree, so its link text says nothing about what cargo
reads through it. Neither repository tracks one.

READ-ONLY: it writes nothing into the repository, not even objects (unlike `git add` into
a scratch index, which would write blobs into ../client-rust's object store).
"""

import hashlib
import os
import stat
import subprocess
import sys

root = sys.argv[1]
out = subprocess.run(
    ["git", "-C", root, "ls-files", "-z", "--cached", "--others",
     "--exclude-per-directory=.gitignore"],
    check=True, capture_output=True).stdout
h = hashlib.sha256()
for rel in sorted({f for f in out.split(b"\0") if f}):
    p = os.path.join(os.fsencode(root), rel)
    st = os.lstat(p) if os.path.lexists(p) else None
    if st is None:
        continue  # a tracked file deleted on disk: its absence changes the digest
    if stat.S_ISLNK(st.st_mode):
        sys.exit(f"tree-digest: {rel.decode(errors='replace')} is a symlink, so what cargo "
                 "reads through it is not digested")
    if stat.S_ISREG(st.st_mode):
        kind = b"x" if st.st_mode & 0o111 else b"f"
        with open(p, "rb") as f:
            data = f.read()
    else:
        sys.exit(f"tree-digest: {rel.decode(errors='replace')} is neither a file nor a "
                 "symlink (a submodule?), so its contents cannot be digested")
    h.update(rel + b"\0" + kind + b"\0" + hashlib.sha256(data).digest())
print(h.hexdigest())
