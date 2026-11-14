#!/usr/bin/env python3
"""Read-only Go pclntab reader and x86-64 disassembler for PE (Windows) Go binaries.

Written to inspect agy.exe (windows/amd64, externally linked, no COFF symbol
table), where `go tool objdump` fails with "no runtime.pclntab symbol found".
It never writes to the target binary: the file is opened with mmap ACCESS_READ.

Usage:
    python gopclntab_pe.py <exe> info
    python gopclntab_pe.py <exe> list  <regex> [--file <regex>]
    python gopclntab_pe.py <exe> dis   <regex> [--max N]
    python gopclntab_pe.py <exe> xref  <regex>          # callers of matching functions
    python gopclntab_pe.py <exe> str   <hex-va> [len]   # dump bytes at a VA as text
    python gopclntab_pe.py <exe> type  <hex-va> [--depth N]  # dump an abi.Type (struct fields + tags)
    python gopclntab_pe.py <exe> inl   <regex> --callee <regex>  # functions that inline a matching callee

`dis` needs the `capstone` package (use a throwaway venv). Each instruction is
annotated with the source file:line from pclntab, the inlined callee when the
PC belongs to an inlined body, the callee name for direct CALL/JMP, and a text
preview for RIP-relative references into read-only data.
"""

import argparse
import bisect
import mmap
import re
import struct
import sys

PCLN_MAGIC = b"\xf1\xff\xff\xff\x00\x00\x01\x08"  # Go 1.20+ header, minLC=1, ptrSize=8
PCDATA_INL_TREE_INDEX = 2
FUNCDATA_INL_TREE = 3
GOFUNC_OFFSETS = (320, 344)  # candidate offsets of moduledata.gofunc, tried in order
TYPES_OFFSET = 296  # moduledata.types: base for abi.Type name offsets


class PE:
    def __init__(self, path):
        self.f = open(path, "rb")
        self.m = mmap.mmap(self.f.fileno(), 0, access=mmap.ACCESS_READ)
        m = self.m
        pe = struct.unpack_from("<I", m, 0x3C)[0]
        if m[pe:pe + 4] != b"PE\0\0":
            raise SystemExit("not a PE file")
        nsec = struct.unpack_from("<H", m, pe + 6)[0]
        optsz = struct.unpack_from("<H", m, pe + 20)[0]
        opt = pe + 24
        if struct.unpack_from("<H", m, opt)[0] != 0x20B:
            raise SystemExit("not PE32+")
        self.base = struct.unpack_from("<Q", m, opt + 24)[0]
        self.sections = []
        sec = opt + optsz
        for i in range(nsec):
            o = sec + i * 40
            name = m[o:o + 8].rstrip(b"\0").decode("latin1")
            vsize, vaddr, rsize, roff = struct.unpack_from("<IIII", m, o + 8)
            self.sections.append((name, self.base + vaddr, vsize, roff, rsize))

    def va2off(self, va):
        for name, sva, vsize, roff, rsize in self.sections:
            if sva <= va < sva + max(vsize, rsize):
                d = va - sva
                if d >= rsize:
                    return None
                return roff + d
        return None

    def off2va(self, off):
        for name, sva, vsize, roff, rsize in self.sections:
            if roff <= off < roff + rsize:
                return sva + off - roff
        return None

    def section_of(self, va):
        for name, sva, vsize, roff, rsize in self.sections:
            if sva <= va < sva + max(vsize, rsize):
                return name
        return None

    def read(self, va, n):
        off = self.va2off(va)
        if off is None:
            return None
        return self.m[off:off + n]

    def u32(self, va):
        return struct.unpack_from("<I", self.m, self.va2off(va))[0]

    def u64(self, va):
        return struct.unpack_from("<Q", self.m, self.va2off(va))[0]


class PclnTab:
    def __init__(self, pe):
        self.pe = pe
        m = pe.m
        self.hdr_off = None
        pos = 0
        while True:
            pos = m.find(PCLN_MAGIC, pos)
            if pos < 0:
                break
            nfunc, nfiles, text_start, fn_off, cu_off, ft_off, pc_off, pcln_off = struct.unpack_from("<8Q", m, pos + 8)
            if 0 < nfunc < 10_000_000 and fn_off < cu_off < ft_off < pc_off < pcln_off < (1 << 31):
                self.hdr_off = pos
                break
            pos += 1
        if self.hdr_off is None:
            raise SystemExit("pclntab header not found")
        self.nfunc, self.nfiles, self.text_start = nfunc, nfiles, text_start
        self.funcname = pos + fn_off
        self.cutab = pos + cu_off
        self.filetab = pos + ft_off
        self.pctab = pos + pc_off
        self.pcln = pos + pcln_off
        self.hdr_va = pe.off2va(pos)
        self.moduledata_va = None
        self.gofunc = None
        self.types_va = None
        self._find_moduledata()
        if not self.text_start:
            raise SystemExit("text start unknown (no textStart in header, moduledata not found)")
        # ftab: nfunc+1 entries of (entryoff u32, funcoff u32)
        self.entries = []
        self.funcoffs = []
        for i in range(self.nfunc + 1):
            e, fo = struct.unpack_from("<II", m, self.pcln + i * 8)
            self.entries.append(self.text_start + e)
            self.funcoffs.append(fo)
        self._names = {}

    def _find_moduledata(self):
        """moduledata starts with a pointer to the pclntab header; text is at +176."""
        pe, m = self.pe, self.pe.m
        needle = struct.pack("<Q", self.hdr_va)
        for name, sva, vsize, roff, rsize in pe.sections:
            if name not in (".data", ".rdata", ".noptrdata"):
                continue
            pos = roff
            while True:
                pos = m.find(needle, pos, roff + rsize)
                if pos < 0:
                    break
                # funcnametab slice pointer must equal the funcname table address
                fn_ptr = struct.unpack_from("<Q", m, pos + 8)[0]
                if fn_ptr == pe.off2va(self.funcname):
                    self.moduledata_va = pe.off2va(pos)
                    minpc, maxpc, text, etext = struct.unpack_from("<4Q", m, pos + 160)
                    self.minpc, self.maxpc, self.text, self.etext = minpc, maxpc, text, etext
                    if not self.text_start:
                        self.text_start = text
                    # gofunc sits at +320 up to Go 1.25; the go1.28 toolchain that built
                    # agy 1.5.9 has extra type-table fields and keeps it at +344.
                    self.types_va = struct.unpack_from("<Q", m, pos + TYPES_OFFSET)[0]
                    for cand in GOFUNC_OFFSETS:
                        gofunc = struct.unpack_from("<Q", m, pos + cand)[0]
                        if pe.section_of(gofunc) == ".rdata":
                            self.gofunc = gofunc
                            break
                    return
                pos += 8

    def cstr(self, off):
        end = self.pe.m.find(b"\0", off)
        return self.pe.m[off:end].decode("utf-8", "replace")

    def func(self, i):
        """Return the decoded _func record for function index i."""
        m = self.pe.m
        o = self.pcln + self.funcoffs[i]
        (entry_off, name_off, args, deferreturn, pcsp, pcfile, pcln, npcdata, cu_off, start_line) = struct.unpack_from("<IiiIIIIIIi", m, o)
        func_id, flag, _pad, nfuncdata = struct.unpack_from("<BBBB", m, o + 40)
        return {
            "off": o, "entry": self.text_start + entry_off, "name_off": name_off,
            "pcsp": pcsp, "pcfile": pcfile, "pcln": pcln, "npcdata": npcdata,
            "cu": cu_off, "start_line": start_line, "nfuncdata": nfuncdata,
        }

    def name(self, i):
        if i not in self._names:
            o = self.pcln + self.funcoffs[i]
            name_off = struct.unpack_from("<i", self.pe.m, o + 4)[0]
            self._names[i] = self.cstr(self.funcname + name_off)
        return self._names[i]

    def find(self, pc):
        i = bisect.bisect_right(self.entries, pc) - 1
        if i < 0 or i >= self.nfunc:
            return None
        return i

    def _varint(self, p):
        m = self.pe.m
        v = 0
        shift = 0
        while True:
            b = m[p]
            p += 1
            v |= (b & 0x7F) << shift
            if not b & 0x80:
                return v, p
            shift += 7

    def pcvalues(self, off, entry):
        """Decode a pc-value table into [(start_pc, end_pc, value)]."""
        if off == 0:
            return []
        p = self.pctab + off
        pc = entry
        val = -1
        out = []
        first = True
        while True:
            uv, p = self._varint(p)
            if uv == 0 and not first:
                break
            first = False
            val += -(uv >> 1) - 1 if uv & 1 else uv >> 1
            d, p = self._varint(p)
            out.append((pc, pc + d, val))
            pc += d
        return out

    def file_name(self, fn, fileno):
        if fileno < 0:
            return "?"
        off = struct.unpack_from("<I", self.pe.m, self.cutab + (fn["cu"] + fileno) * 4)[0]
        if off == 0xFFFFFFFF:
            return "?"
        return self.cstr(self.filetab + off)

    def inl_tree(self, fn):
        """Return ([(start, end, index)], tree_va) for the inline tree, or ([], None)."""
        if self.gofunc is None or fn["npcdata"] <= PCDATA_INL_TREE_INDEX or fn["nfuncdata"] <= FUNCDATA_INL_TREE:
            return [], None
        m = self.pe.m
        base = fn["off"] + 44
        pcdata_off = struct.unpack_from("<I", m, base + PCDATA_INL_TREE_INDEX * 4)[0]
        fd_off = struct.unpack_from("<I", m, base + fn["npcdata"] * 4 + FUNCDATA_INL_TREE * 4)[0]
        if fd_off == 0xFFFFFFFF or pcdata_off == 0:
            return [], None
        return self.pcvalues(pcdata_off, fn["entry"]), self.gofunc + fd_off

    def inl_name(self, tree_va, idx):
        """Chain of inlined function names for inline-tree index idx (innermost first)."""
        names = []
        fn_entry = None
        seen = 0
        while idx >= 0 and seen < 16:
            raw = self.pe.read(tree_va + idx * 16, 16)
            if raw is None:
                break
            _fid, name_off, parent_pc, _start = struct.unpack("<B3xiii", raw)
            names.append(self.cstr(self.funcname + name_off))
            # parent: the inline index at parent_pc is resolved by the caller
            return names, parent_pc
        return names, None


def lookup(ranges, pc):
    for s, e, v in ranges:
        if s <= pc < e:
            return v
    return None


def fmt_duration(ns):
    if ns < 1_000_000 or ns % 1_000_000:
        return None
    ms = ns // 1_000_000
    if ms > 86_400_000 * 30:
        return None
    if ms % 1000 == 0:
        s = ms // 1000
        if s % 60 == 0:
            return f"{s // 60}m as time.Duration"
        return f"{s}s as time.Duration"
    return f"{ms}ms as time.Duration"


def preview(pe, va, n=72):
    raw = pe.read(va, n)
    if not raw:
        return None
    out = []
    for b in raw:
        if 32 <= b < 127:
            out.append(chr(b))
        elif b in (10, 9):
            out.append("\\n" if b == 10 else "\\t")
        elif b >= 0x80:
            out.append("\\x%02x" % b)
        else:
            break
    s = "".join(out)
    return s if len(s) >= 2 else None


def cmd_info(pe, tab, args):
    print(f"image base      {pe.base:#x}")
    for name, sva, vsize, roff, rsize in pe.sections:
        print(f"  {name:<9} va={sva:#x} vsize={vsize:#x} fileoff={roff:#x} rawsize={rsize:#x}")
    print(f"pclntab header  fileoff={tab.hdr_off:#x} va={tab.hdr_va:#x}")
    print(f"nfunc={tab.nfunc} nfiles={tab.nfiles} textStart={tab.text_start:#x}")
    print(f"moduledata va   {tab.moduledata_va and hex(tab.moduledata_va)}")
    print(f"gofunc          {tab.gofunc and hex(tab.gofunc)}")


def matching(tab, pattern):
    rx = re.compile(pattern)
    for i in range(tab.nfunc):
        if rx.search(tab.name(i)):
            yield i


def cmd_list(pe, tab, args):
    file_rx = re.compile(args.file) if args.file else None
    for i in matching(tab, args.pattern):
        fn = tab.func(i)
        size = tab.entries[i + 1] - tab.entries[i]
        files = tab.pcvalues(fn["pcfile"], fn["entry"])
        fname = tab.file_name(fn, files[0][2]) if files else "?"
        if file_rx and not file_rx.search(fname):
            continue
        print(f"{tab.entries[i]:#x} size={size:<6} {tab.name(i)}  [{fname}:{fn['start_line']}]")


def cmd_dis(pe, tab, args):
    import capstone
    from capstone import x86 as cx

    md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
    md.detail = True
    count = 0
    for i in matching(tab, args.pattern):
        count += 1
        if count > args.max:
            print(f"... more matches omitted (--max {args.max})")
            break
        fn = tab.func(i)
        start, end = tab.entries[i], tab.entries[i + 1]
        code = pe.read(start, end - start)
        lines = tab.pcvalues(fn["pcln"], fn["entry"])
        files = tab.pcvalues(fn["pcfile"], fn["entry"])
        inl, tree_va = tab.inl_tree(fn)
        print(f"\n=== {tab.name(i)}  {start:#x}..{end:#x} (size {end - start}) start_line={fn['start_line']}")
        last_pos = None
        pending = None  # (va) of last rdata LEA, to pair with a following length immediate
        for ins in md.disasm(code, start):
            pc = ins.address
            if ins.id == cx.X86_INS_INT3:
                continue  # inter-function padding
            line = lookup(lines, pc)
            fileno = lookup(files, pc)
            fname = tab.file_name(fn, fileno) if fileno is not None else "?"
            inl_s = ""
            if tree_va is not None:
                idx = lookup(inl, pc)
                if idx is not None and idx >= 0:
                    names, _ = tab.inl_name(tree_va, idx)
                    if names:
                        inl_s = f" <inl {names[0]}>"
            pos = f"{fname.rsplit('/', 2)[-1]}:{line}{inl_s}"
            if pos != last_pos:
                print(f"  ; {fname}:{line}{inl_s}")
                last_pos = pos
            notes = []
            if ins.id in (cx.X86_INS_CALL, cx.X86_INS_JMP) and ins.operands and ins.operands[0].type == cx.X86_OP_IMM:
                tgt = ins.operands[0].imm
                if not (start <= tgt < end):
                    j = tab.find(tgt)
                    if j is not None:
                        d = tgt - tab.entries[j]
                        notes.append(tab.name(j) + (f"+{d:#x}" if d else ""))
            for op in ins.operands:
                if op.type == cx.X86_OP_MEM and op.mem.base == cx.X86_REG_RIP:
                    va = pc + ins.size + op.mem.disp
                    sec = pe.section_of(va)
                    if ins.id == cx.X86_INS_LEA:
                        j = tab.find(va) if sec == ".text" else None
                        if j is not None and tab.entries[j] == va:
                            notes.append("&" + tab.name(j))
                        else:
                            p = preview(pe, va) if sec in (".rdata",) else None
                            notes.append(f"{sec}:{va:#x}" + (f' "{p}"' if p else ""))
                            pending = (va, 4) if p else None
                    else:
                        raw = pe.read(va, 8)
                        if raw and sec == ".rdata":
                            q = struct.unpack("<Q", raw)[0]
                            dbl = struct.unpack("<d", raw)[0]
                            tgt = tab.find(q) if pe.section_of(q) == ".text" else None
                            if tgt is not None and tab.entries[tgt] == q:
                                notes.append(f"{sec}:{va:#x} -> &{tab.name(tgt)}")
                            else:
                                notes.append(f"{sec}:{va:#x} q={q:#x} f64={dbl:g}")
                        else:
                            notes.append(f"{sec}:{va:#x}")
                elif op.type == cx.X86_OP_IMM and ins.id in (cx.X86_INS_MOV, cx.X86_INS_MOVABS, cx.X86_INS_CMP, cx.X86_INS_ADD, cx.X86_INS_SUB):
                    imm = op.imm & 0xFFFFFFFFFFFFFFFF
                    d = fmt_duration(imm)
                    if d:
                        notes.append(d)
                    if pending and ins.id == cx.X86_INS_MOV and 0 < imm <= 4096:
                        raw = pe.read(pending[0], imm)
                        if raw:
                            notes.append("str=" + repr(raw.decode("utf-8", "replace")))
                        pending = None
            if pending:
                pending = (pending[0], pending[1] - 1)
                if pending[1] <= 0:
                    pending = None
            note = ("  ; " + " | ".join(notes)) if notes else ""
            print(f"    {pc:#x}: {ins.mnemonic} {ins.op_str}{note}")


def cmd_xref(pe, tab, args):
    """Find direct CALL/JMP rel32 sites targeting matching functions (linear byte scan of Go text)."""
    targets = {tab.entries[i]: tab.name(i) for i in matching(tab, args.pattern)}
    if not targets:
        print("no matching functions")
        return
    m = pe.m
    lo, hi = tab.entries[0], tab.entries[-1]
    off_lo, off_hi = pe.va2off(lo), pe.va2off(hi - 1)
    hits = {}
    for opcode in (b"\xe8", b"\xe9"):
        pos = off_lo
        while True:
            pos = m.find(opcode, pos, off_hi)
            if pos < 0:
                break
            rel = struct.unpack_from("<i", m, pos + 1)[0]
            va = pe.off2va(pos)
            tgt = va + 5 + rel
            if tgt in targets:
                j = tab.find(va)
                key = (targets[tgt], tab.name(j) if j is not None else "?")
                hits.setdefault(key, []).append(va)
            pos += 1
    for (callee, caller), sites in sorted(hits.items()):
        print(f"{callee}  <-  {caller}  @ " + ", ".join(hex(s) for s in sites))


KINDS = {1: "bool", 2: "int", 3: "int8", 4: "int16", 5: "int32", 6: "int64", 7: "uint", 8: "uint8", 9: "uint16",
         10: "uint32", 11: "uint64", 12: "uintptr", 13: "float32", 14: "float64", 15: "complex64", 16: "complex128",
         17: "array", 18: "chan", 19: "func", 20: "interface", 21: "map", 22: "ptr", 23: "slice", 24: "string",
         25: "struct", 26: "unsafe.Pointer"}


def read_name(pe, va):
    """Decode a runtime name: flag byte, varint length, bytes, then an optional tag."""
    raw = pe.read(va, 4096)
    if not raw:
        return "?", ""
    flags = raw[0]
    p = 1
    n = 0
    shift = 0
    while True:
        b = raw[p]
        p += 1
        n |= (b & 0x7F) << shift
        if not b & 0x80:
            break
        shift += 7
    name = raw[p:p + n].decode("utf-8", "replace")
    p += n
    tag = ""
    if flags & 2:
        t = 0
        shift = 0
        while True:
            b = raw[p]
            p += 1
            t |= (b & 0x7F) << shift
            if not b & 0x80:
                break
            shift += 7
        tag = raw[p:p + t].decode("utf-8", "replace")
    return name, tag


def type_info(pe, tab, va):
    """Return (kind, name, size) for an abi.Type at va."""
    size = pe.u64(va)
    kind = pe.read(va + 23, 1)[0] & 0x1F
    name_off = pe.u32(va + 40)
    name = "?"
    if tab.types_va:
        name, _ = read_name(pe, tab.types_va + name_off)
        if pe.read(va + 20, 1)[0] & 2 and name.startswith("*"):  # tflagExtraStar
            name = name[1:]
    return kind, name, size


def dump_type(pe, tab, va, depth, indent=""):
    kind, name, size = type_info(pe, tab, va)
    kname = KINDS.get(kind, f"kind{kind}")
    print(f"{indent}{name}  ({kname}, size {size}) @ {va:#x}")
    if depth <= 0:
        return
    if kind in (22, 23, 17, 18):  # ptr, slice, array, chan: elem pointer follows the 48-byte header
        dump_type(pe, tab, pe.u64(va + 48), depth, indent + "  ")
    elif kind == 25:  # struct: pkgPath name (8), then fields slice
        fields, nfields = pe.u64(va + 56), pe.u64(va + 64)
        for i in range(nfields):
            f = fields + i * 24
            fname, tag = read_name(pe, pe.u64(f))
            ftyp, foff = pe.u64(f + 8), pe.u64(f + 16)
            fk, ftn, fsz = type_info(pe, tab, ftyp)
            print(f"{indent}  +{foff:#05x} {fname:<28} {ftn:<40} `{tag}`")
            if depth > 1 and fk in (21, 22, 23, 25):
                target = ftyp
                while type_info(pe, tab, target)[0] in (21, 22, 23):
                    # ptr/slice keep elem at +48; a map keeps key at +48 and elem at +56
                    target = pe.u64(target + (56 if type_info(pe, tab, target)[0] == 21 else 48))
                if type_info(pe, tab, target)[0] == 25:
                    dump_type(pe, tab, target, depth - 1, indent + "      ")


def cmd_type(pe, tab, args):
    dump_type(pe, tab, int(args.va, 16), args.depth)


def cmd_inl(pe, tab, args):
    """List functions (matching pattern) whose inline tree contains a callee matching --callee."""
    callee_rx = re.compile(args.callee)
    for i in matching(tab, args.pattern):
        fn = tab.func(i)
        ranges, tree = tab.inl_tree(fn)
        if tree is None:
            continue
        seen = set()
        for start, end, idx in ranges:
            if idx < 0 or idx in seen:
                continue
            seen.add(idx)
            names, _ = tab.inl_name(tree, idx)
            if names and callee_rx.search(names[0]):
                print(f"{tab.name(i)}  inlines {names[0]} at {start:#x}..{end:#x}")


def cmd_str(pe, tab, args):
    va = int(args.va, 16)
    raw = pe.read(va, args.len)
    print(repr(raw.decode("utf-8", "replace")))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("exe")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("info")
    p = sub.add_parser("list")
    p.add_argument("pattern")
    p.add_argument("--file", help="only functions whose source file matches this regex")
    p = sub.add_parser("dis")
    p.add_argument("pattern")
    p.add_argument("--max", type=int, default=40)
    p = sub.add_parser("xref")
    p.add_argument("pattern")
    p = sub.add_parser("str")
    p.add_argument("va")
    p.add_argument("len", type=int, nargs="?", default=200)
    p = sub.add_parser("type")
    p.add_argument("va")
    p.add_argument("--depth", type=int, default=1)
    p = sub.add_parser("inl")
    p.add_argument("pattern")
    p.add_argument("--callee", required=True)
    args = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    pe = PE(args.exe)
    tab = PclnTab(pe)
    commands = {"info": cmd_info, "list": cmd_list, "dis": cmd_dis, "xref": cmd_xref, "str": cmd_str,
                "type": cmd_type, "inl": cmd_inl}
    commands[args.cmd](pe, tab, args)


if __name__ == "__main__":
    main()
