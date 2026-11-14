"""Annotated disassembly of Go functions in agy.exe, selected by a name regex.

Usage:
    python disasm_go_func.py 'store\\.BuildStatusLineData$' [--exe path] [--list]

Each call is resolved to its function name through the pclntab, and each
RIP-relative operand is labelled as a Go type (through the type metadata), a
read-only string candidate, or a data address. --list prints matching function
names and sizes without disassembling.

Needs capstone (pip install capstone, in a throwaway venv). Read-only.
"""
import argparse
import os
import re

import capstone
from capstone import x86_const as x

from gobin import KINDS, PE, GoTypes, PclnTab, discover_types_base

DEFAULT_EXE = os.path.join(os.environ.get("LOCALAPPDATA", ""), "agy", "bin", "agy.exe")
ROOT_TYPE = "types.StatusLineData"
ANCHOR_FIELD = "ToolConfirmationPending"
ANCHOR_TAG = 'json:"tool_confirmation_pending,omitempty"'
PREVIEW = 56


def printable(raw):
    return "".join(chr(c) if 0x20 <= c < 0x7F else "." for c in raw)


def describe_rodata(pe, gt, target, neighbours):
    """Label a .rdata address as a Go type, else as string candidates."""
    off = pe.va2off(target)
    kind = pe.u8(off + 23) & 0x1F
    str_off = pe.i32(off + 40)
    if target % 8 == 0 and 1 <= kind <= 26 and 0 < str_off < 0x2000000 and pe.u8(off + 21) in (1, 2, 4, 8, 16):
        ts = gt.type_string(target)
        if ts and len(ts) < 200 and all(0x20 <= ord(c) < 0x7F for c in ts):
            return "type %s (%s)" % (ts, KINDS.get(kind))
    raw = bytes(pe.m[off:off + PREVIEW])
    # A Go string is (pointer, length); the length is usually an immediate nearby.
    lens = []
    for ins in neighbours:
        if ins.id not in (x.X86_INS_MOV, x.X86_INS_CMP):
            continue
        for op in ins.operands:
            if op.type == x.X86_OP_IMM and 1 <= op.imm <= PREVIEW:
                lens.append(op.imm)
    cands = " | ".join(repr(printable(raw[:n])) for n in dict.fromkeys(lens))
    return "rodata %r  str? %s" % (printable(raw), cands)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("pattern")
    ap.add_argument("--exe", default=DEFAULT_EXE)
    ap.add_argument("--list", action="store_true")
    args = ap.parse_args()

    pe = PE(args.exe)
    pc = PclnTab(pe)
    base, _root = discover_types_base(pe, ROOT_TYPE, ANCHOR_FIELD, ANCHOR_TAG)
    gt = GoTypes(pe, base)
    rdata = pe.sec(".rdata")
    data = pe.sec(".data")
    rx = re.compile(args.pattern)
    md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
    md.detail = True

    for start, end, name in pc.funcs():
        if not rx.search(name):
            continue
        if args.list:
            print("%#x %6d %s" % (start, end - start, name))
            continue
        print("\n===== %s  %#x..%#x (%d bytes)" % (name, start, end, end - start))
        off = pe.va2off(start)
        inss = list(md.disasm(bytes(pe.m[off:off + (end - start)]), start))
        for idx, ins in enumerate(inss):
            note = ""
            first = ins.operands[0] if ins.operands else None
            if ins.id == x.X86_INS_CALL and first is not None and first.type == x.X86_OP_IMM:
                hit = pc.lookup(first.imm)
                note = "; -> " + hit[1] if hit else ""
            elif ins.id == x.X86_INS_JMP and first is not None and first.type == x.X86_OP_IMM:
                if not start <= first.imm < end:
                    hit = pc.lookup(first.imm)
                    note = "; => " + hit[1] if hit else ""
            else:
                for op in ins.operands:
                    if op.type != x.X86_OP_MEM or op.mem.base != x.X86_REG_RIP:
                        continue
                    target = ins.address + ins.size + op.mem.disp
                    if rdata[1] <= target < rdata[1] + rdata[4]:
                        note = "; %#x %s" % (target, describe_rodata(pe, gt, target, inss[max(0, idx - 3):idx + 4]))
                    elif data[1] <= target < data[1] + max(data[2], data[4]):
                        note = "; data %#x" % target
                    elif pc.text <= target < pc.etext:
                        hit = pc.lookup(target)
                        note = "; code %s+%#x" % (hit[1], target - hit[0]) if hit else ""
            print("%#x  %-9s %-46s %s" % (ins.address, ins.mnemonic, ins.op_str, note))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
