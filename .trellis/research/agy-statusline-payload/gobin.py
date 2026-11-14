"""Read-only readers for a Go PE32+ binary: sections, abi.Type records, pclntab.

Written against agy.exe 1.5.9 (go1.28 development toolchain, windows/amd64).
The file is opened read-only and memory-mapped; nothing is written to it.
Standard library only.
"""
import bisect
import mmap
import struct

KINDS = {
    1: "bool", 2: "int", 3: "int8", 4: "int16", 5: "int32", 6: "int64",
    7: "uint", 8: "uint8", 9: "uint16", 10: "uint32", 11: "uint64", 12: "uintptr",
    13: "float32", 14: "float64", 15: "complex64", 16: "complex128",
    17: "array", 18: "chan", 19: "func", 20: "interface", 21: "map", 22: "ptr",
    23: "slice", 24: "string", 25: "struct", 26: "unsafe.Pointer",
}
KIND_MAP = 21
KIND_PTR = 22
KIND_STRUCT = 25
TFLAG_UNCOMMON = 1
TFLAG_EXTRA_STAR = 2
RTYPE_SIZE = 48
PCLN_MAGIC = 0xFFFFFFF1


def uvarint(buf, pos):
    val = 0
    shift = 0
    while True:
        b = buf[pos]
        pos += 1
        val |= (b & 0x7F) << shift
        if b < 0x80:
            return val, pos
        shift += 7


def enc_uvarint(n):
    out = bytearray()
    while n >= 0x80:
        out.append((n & 0x7F) | 0x80)
        n >>= 7
    out.append(n)
    return bytes(out)


class PE:
    """Section table of a PE32+ image plus VA <-> file offset conversion."""

    def __init__(self, path):
        self.f = open(path, "rb")
        self.m = mmap.mmap(self.f.fileno(), 0, access=mmap.ACCESS_READ)
        m = self.m
        pe = struct.unpack_from("<I", m, 0x3C)[0]
        if m[pe:pe + 4] != b"PE\0\0":
            raise ValueError("not a PE file")
        nsec = struct.unpack_from("<H", m, pe + 6)[0]
        optsz = struct.unpack_from("<H", m, pe + 20)[0]
        opt = pe + 24
        if struct.unpack_from("<H", m, opt)[0] != 0x20B:
            raise ValueError("not PE32+")
        self.image_base = struct.unpack_from("<Q", m, opt + 24)[0]
        self.sections = []
        first = opt + optsz
        for i in range(nsec):
            o = first + i * 40
            name = m[o:o + 8].rstrip(b"\0").decode("latin1")
            vsize, vaddr, rsize, roff = struct.unpack_from("<IIII", m, o + 8)
            self.sections.append((name, self.image_base + vaddr, vsize, roff, rsize))

    def va2off(self, va):
        for _name, sva, _vsize, roff, rsize in self.sections:
            if sva <= va < sva + rsize:
                return roff + (va - sva)
        return None

    def off2va(self, off):
        for _name, sva, _vsize, roff, rsize in self.sections:
            if roff <= off < roff + rsize:
                return sva + (off - roff)
        return None

    def sec(self, name):
        for s in self.sections:
            if s[0] == name:
                return s
        return None

    def u8(self, off):
        return self.m[off]

    def u16(self, off):
        return struct.unpack_from("<H", self.m, off)[0]

    def u32(self, off):
        return struct.unpack_from("<I", self.m, off)[0]

    def i32(self, off):
        return struct.unpack_from("<i", self.m, off)[0]

    def u64(self, off):
        return struct.unpack_from("<Q", self.m, off)[0]

    def find_all(self, needle, aligned=1):
        m = self.m
        pos = 0
        while True:
            i = m.find(needle, pos)
            if i < 0:
                return
            pos = i + 1
            if i % aligned == 0:
                yield i


class GoTypes:
    """abi.Type reader. `types_base` is the VA that NameOff/TypeOff are relative to."""

    def __init__(self, pe, types_base=None):
        self.pe = pe
        self.types_base = types_base

    # --- abi.Name ---------------------------------------------------------
    def name_at(self, va):
        """Decode an abi.Name at VA: (name, tag, exported, embedded)."""
        off = self.pe.va2off(va)
        if off is None:
            return None
        m = self.pe.m
        flag = m[off]
        n, p = uvarint(m, off + 1)
        if n > 4096:
            return None
        name = bytes(m[p:p + n]).decode("utf-8", "replace")
        p += n
        tag = ""
        if flag & 2:
            tn, p = uvarint(m, p)
            tag = bytes(m[p:p + tn]).decode("utf-8", "replace")
        return name, tag, bool(flag & 1), bool(flag & 8)

    def find_name_records(self, text, tag=None):
        """File offsets (of the flag byte) of abi.Name records named `text`."""
        raw = text.encode()
        pat = enc_uvarint(len(raw)) + raw
        if tag is not None:
            t = tag.encode()
            pat += enc_uvarint(len(t)) + t
        return [i - 1 for i in self.pe.find_all(pat)]

    # --- abi.Type ---------------------------------------------------------
    def rtype(self, va):
        off = self.pe.va2off(va)
        if off is None:
            return None
        pe = self.pe
        return {
            "va": va,
            "off": off,
            "size": pe.u64(off),
            "tflag": pe.u8(off + 20),
            "kind": pe.u8(off + 23) & 0x1F,
            "str": pe.i32(off + 40),
            "ptrtothis": pe.i32(off + 44),
        }

    def type_string(self, va):
        t = self.rtype(va)
        if t is None:
            return "?%#x" % va
        nm = self.name_at(self.types_base + t["str"])
        if nm is None:
            return "?%#x" % va
        s = nm[0]
        if t["tflag"] & TFLAG_EXTRA_STAR and s.startswith("*"):
            s = s[1:]
        return s

    def kind(self, va):
        t = self.rtype(va)
        return KINDS.get(t["kind"], "kind%d" % t["kind"]) if t else "?"

    def elem(self, va):
        """Elem for ptr/slice/array/chan; (key, elem) for map."""
        t = self.rtype(va)
        off = t["off"] + RTYPE_SIZE
        if t["kind"] == KIND_MAP:
            return self.pe.u64(off), self.pe.u64(off + 8)
        return self.pe.u64(off)

    def struct_fields(self, va):
        t = self.rtype(va)
        if t is None or t["kind"] != KIND_STRUCT:
            return None
        pe = self.pe
        off = t["off"]
        fptr = pe.u64(off + 56)
        flen = pe.u64(off + 64)
        out = []
        if flen == 0:
            return out
        foff = pe.va2off(fptr)
        for i in range(flen):
            e = foff + i * 24
            nm = self.name_at(pe.u64(e))
            typ = pe.u64(e + 8)
            out.append({
                "name": nm[0], "tag": nm[1], "exported": nm[2], "embedded": nm[3],
                "typ": typ, "offset": pe.u64(e + 16),
                "type_string": self.type_string(typ), "kind": self.kind(typ),
            })
        return out

    def methods(self, va):
        """Method names from the uncommon type of a struct or pointer type."""
        t = self.rtype(va)
        if t is None or not t["tflag"] & TFLAG_UNCOMMON:
            return []
        if t["kind"] == KIND_STRUCT:
            uo = t["off"] + RTYPE_SIZE + 8 + 24
        elif t["kind"] == KIND_PTR:
            uo = t["off"] + RTYPE_SIZE + 8
        else:
            return []
        pe = self.pe
        mcount = pe.u16(uo + 4)
        moff = pe.u32(uo + 8)
        out = []
        for i in range(mcount):
            name_off = pe.i32(uo + moff + i * 16)
            nm = self.name_at(self.types_base + name_off)
            out.append(nm[0] if nm else "?")
        return out

    def pointer_to(self, va):
        t = self.rtype(va)
        if t and t["ptrtothis"]:
            return self.types_base + t["ptrtothis"]
        return None

    # --- discovery --------------------------------------------------------
    def find_struct_rtypes_by_name(self, qualified):
        """Struct rtypes whose string form is `qualified`, e.g. types.StatusLineData."""
        pe = self.pe
        out = []
        for rec in self.find_name_records("*" + qualified):
            name_va = pe.off2va(rec)
            if name_va is None:
                continue
            name_off = name_va - self.types_base
            if not 0 < name_off < 2**31:
                continue
            for i in pe.find_all(struct.pack("<i", name_off), aligned=8):
                cand = i - 40
                va = pe.off2va(cand)
                if va is None:
                    continue
                if pe.u8(cand + 23) & 0x1F == KIND_STRUCT and pe.u8(cand + 20) & TFLAG_EXTRA_STAR:
                    if self.type_string(va) == qualified:
                        out.append(va)
        return sorted(set(out))

    def owners_of_field_name(self, name_record_off):
        """Struct types that have a field whose abi.Name is at this file offset.

        A field entry is (name pointer, type pointer, offset). Walk back from the
        entry to the start of its array, then find the struct type whose
        Fields.ptr is that start.
        """
        pe = self.pe
        owners = []
        needle = struct.pack("<Q", pe.off2va(name_record_off))
        for j in pe.find_all(needle, aligned=8):
            start = j
            for _ in range(4096):
                prev = start - 24
                if pe.va2off(pe.u64(prev)) is None or pe.va2off(pe.u64(prev + 8)) is None:
                    break
                if pe.u64(prev + 16) > pe.u64(start + 16):
                    break
                start = prev
            for cand in range(start, j + 1, 24):
                for k in pe.find_all(struct.pack("<Q", pe.off2va(cand)), aligned=8):
                    roff = k - 56
                    if pe.u8(roff + 23) & 0x1F != KIND_STRUCT:
                        continue
                    flen = pe.u64(k + 8)
                    if flen != pe.u64(k + 16) or not 0 < flen <= 4096:
                        continue
                    if cand + flen * 24 <= j:
                        continue
                    owners.append(pe.off2va(roff))
        return sorted(set(owners))


def discover_types_base(pe, type_name, field_name, field_tag):
    """Locate the NameOff/TypeOff base without parsing moduledata.

    Find the struct that owns a known field, then solve
    types_base = VA(name record of "*<type_name>") - rtype.Str.
    Returns (types_base, rtype VA of the struct).
    """
    probe = GoTypes(pe)
    type_recs = [pe.off2va(r) for r in probe.find_name_records("*" + type_name)]
    for rec in probe.find_name_records(field_name, field_tag):
        for rtype_va in probe.owners_of_field_name(rec):
            str_off = pe.i32(pe.va2off(rtype_va) + 40)
            for tva in type_recs:
                cand = GoTypes(pe, tva - str_off)
                if cand.type_string(rtype_va) == type_name:
                    return tva - str_off, rtype_va
    raise ValueError("could not locate %s" % type_name)


class PclnTab:
    """Function table (go1.20+ pclntab header) for name <-> address lookups."""

    def __init__(self, pe):
        self.pe = pe
        m = pe.m
        i = m.find(struct.pack("<I", PCLN_MAGIC) + b"\x00\x00\x01\x08")
        if i < 0:
            raise ValueError("pclntab magic not found")
        self.hdr_va = pe.off2va(i)
        self.nfunc = pe.u64(i + 8)
        self.funcname_off = i + pe.u64(i + 32)
        self.pcln_off = i + pe.u64(i + 64)
        # moduledata starts with &pcHeader followed by the funcnametab slice.
        self.moduledata_off = None
        for j in pe.find_all(struct.pack("<Q", self.hdr_va), aligned=8):
            if pe.u64(j + 8) == pe.off2va(self.funcname_off):
                self.moduledata_off = j
                break
        if self.moduledata_off is None:
            raise ValueError("moduledata not found")
        # pcHeader, six slices, findfunctab, minpc, maxpc, text, etext
        base = self.moduledata_off + 8 + 6 * 24
        self.text = pe.u64(base + 24)
        self.etext = pe.u64(base + 32)
        raw = struct.unpack_from("<%dI" % (2 * (self.nfunc + 1)), m, self.pcln_off)
        self._entries = raw[0::2]
        self._funcoffs = raw[1::2]

    def func_name(self, idx):
        m = self.pe.m
        name_off = struct.unpack_from("<i", m, self.pcln_off + self._funcoffs[idx] + 4)[0]
        s = self.funcname_off + name_off
        return bytes(m[s:m.find(b"\0", s)]).decode("utf-8", "replace")

    def funcs(self):
        """Yield (start VA, end VA, name)."""
        for i in range(self.nfunc):
            yield self.text + self._entries[i], self.text + self._entries[i + 1], self.func_name(i)

    def lookup(self, va):
        """(start VA, name) of the function containing VA, or None."""
        i = bisect.bisect_right(self._entries, va - self.text) - 1
        if not 0 <= i < self.nfunc:
            return None
        return self.text + self._entries[i], self.func_name(i)
