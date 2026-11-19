"""List code locations in agy.exe whose rel32 operand resolves to a given VA.

Usage:
    python find_xrefs.py 0x143829980 [more VAs] [--exe path]

Covers direct calls and jumps and RIP-relative operands (lea/mov/cmp). Used to
show that store.BuildStatusLineData has two callers and that
types.StatusLineSandbox is allocated in one place.

Needs numpy (pip install numpy, in a throwaway venv). Read-only.
"""
import argparse
import os

import numpy as np

from gobin import PE, PclnTab

DEFAULT_EXE = os.path.join(os.environ.get("LOCALAPPDATA", ""), "agy", "bin", "agy.exe")
# Bytes between the start of the disp32 and the end of the instruction:
# 4 when the displacement is last, 5 or 8 when an imm8 or imm32 follows it.
TAILS = (4, 5, 8)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("targets", nargs="+")
    ap.add_argument("--exe", default=DEFAULT_EXE)
    args = ap.parse_args()

    pe = PE(args.exe)
    pc = PclnTab(pe)
    _name, text_va, _vsize, roff, rsize = pe.sec(".text")
    buf = np.frombuffer(pe.m, dtype=np.uint8, count=rsize, offset=roff)
    n = rsize - 8
    b = buf.astype(np.uint32)
    disp = b[0:n] | (b[1:n + 1] << 8) | (b[2:n + 2] << 16) | (b[3:n + 3] << 24)
    pos = np.arange(n, dtype=np.uint64) + np.uint64(text_va)
    for arg in args.targets:
        target = int(arg, 16)
        print("== refs to %#x" % target)
        for tail in TAILS:
            want = (np.uint64(target) - pos - np.uint64(tail)).astype(np.uint32)
            for h in np.nonzero(disp == want)[0]:
                va = text_va + int(h)
                hit = pc.lookup(va)
                where = "%s+%#x" % (hit[1], va - hit[0]) if hit else "?"
                print("  disp@%#x tail=%d prev=%s in %s" % (va, tail, bytes(buf[max(0, h - 3):h]).hex(), where))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
