"""The ledger the script tests start from: the real one, with G-0002 as it was while OPEN.

The tests copy the REAL ledger so they follow every other change to it. G-0002 is the
exception (fixtures/ledger-g0002-open.toml says why): its open form is swapped in, so the
`diverges` rules keep a real entry to exercise and the recorded candidate run keeps the
ledger it was judged against.
"""

from __future__ import annotations

import re
from pathlib import Path

OPEN_G0002 = Path(__file__).resolve().parent / "fixtures" / "ledger-g0002-open.toml"


def with_open_g0002(ledger: str) -> str:
    """`ledger` with its G-0002 [[gap]] block replaced by the frozen open one."""
    parts = re.split(r"(?m)^(?=\[\[gap\]\])", ledger)
    hits = [i for i, p in enumerate(parts) if re.search(r'(?m)^id\s*=\s*"G-0002"', p)]
    if len(hits) != 1:
        raise AssertionError(f"fixture drifted: {len(hits)} G-0002 entries in ledger.toml")
    block = OPEN_G0002.read_text()
    parts[hits[0]] = block[block.index("[[gap]]"):] + "\n"
    return "".join(parts)


def copy_ledger(src: Path, dst: Path) -> None:
    """Copy the ledger at `src` to `dst`, with G-0002 open."""
    dst.write_text(with_open_g0002(src.read_text()))
