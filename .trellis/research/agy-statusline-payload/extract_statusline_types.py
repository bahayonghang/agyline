"""Dump the Go struct metadata behind agy's status line payload.

Usage:
    python extract_statusline_types.py [path to agy.exe] [--owners json_key ...]

Prints every struct reachable from types.StatusLineData with field offsets,
Go types, and struct tags, read from the binary's type metadata. With
--owners, also lists which struct types own a field tagged json:"<key>...".
Read-only; standard library only.
"""
import argparse
import os
import sys

from gobin import PE, GoTypes, discover_types_base

DEFAULT_EXE = os.path.join(os.environ.get("LOCALAPPDATA", ""), "agy", "bin", "agy.exe")
ROOT_TYPE = "types.StatusLineData"
ANCHOR_FIELD = "ToolConfirmationPending"
ANCHOR_TAG = 'json:"tool_confirmation_pending,omitempty"'
SKIP_PREFIXES = ("time.",)


def dump_struct(gt, va, seen, out):
    if va in seen:
        return
    seen.add(va)
    name = gt.type_string(va)
    t = gt.rtype(va)
    ptr = gt.pointer_to(va)
    out.append("")
    out.append("### %s  (rtype %#x, size %d)" % (name, va, t["size"]))
    out.append("methods (value): %s" % (", ".join(gt.methods(va)) or "none"))
    if ptr is not None:
        out.append("methods (pointer): %s" % (", ".join(gt.methods(ptr)) or "none"))
    nested = []
    for f in gt.struct_fields(va):
        flags = "embedded" if f["embedded"] else ""
        if not f["exported"]:
            flags = (flags + " unexported").strip()
        out.append("  +%-4d %-24s %-42s %-34s %s" % (
            f["offset"], f["name"], f["type_string"], f["tag"], flags))
        nested.append(f["typ"])
    for typ in nested:
        walk(gt, typ, seen, out)


def walk(gt, typ, seen, out):
    kind = gt.kind(typ)
    if kind == "struct":
        if not gt.type_string(typ).startswith(SKIP_PREFIXES):
            dump_struct(gt, typ, seen, out)
    elif kind in ("ptr", "slice", "array"):
        walk(gt, gt.elem(typ), seen, out)
    elif kind == "map":
        key, el = gt.elem(typ)
        walk(gt, key, seen, out)
        walk(gt, el, seen, out)


def owners(gt, key, out):
    out.append("")
    out.append('### owners of json:"%s..."' % key)
    pe = gt.pe
    found = False
    for prefix in ('json:"%s"' % key, 'json:"%s,' % key):
        for i in pe.find_all(prefix.encode()):
            # The tag is preceded by its uvarint length; the field name precedes that.
            # Scan back to the flag byte of the abi.Name that contains this tag
            # (far enough to pass a protobuf:"..." prefix in the same tag).
            for back in range(2, 400):
                rec = i - back
                nm = gt.name_at(pe.off2va(rec)) if pe.off2va(rec) else None
                if nm and prefix in nm[1] and nm[0].isidentifier():
                    for rt in gt.owners_of_field_name(rec):
                        found = True
                        out.append("  %-28s %-40s in %s" % (nm[0], nm[1], gt.type_string(rt)))
                    break
    if not found:
        out.append("  (no struct field in the binary carries this tag)")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("exe", nargs="?", default=DEFAULT_EXE)
    ap.add_argument("--owners", nargs="*", default=[], metavar="JSON_KEY")
    args = ap.parse_args()

    pe = PE(args.exe)
    base, root = discover_types_base(pe, ROOT_TYPE, ANCHOR_FIELD, ANCHOR_TAG)
    gt = GoTypes(pe, base)
    out = ["binary: %s (%d bytes)" % (os.path.basename(args.exe), pe.m.size()),
           "types base: %#x" % base]
    dump_struct(gt, root, set(), out)
    for key in args.owners:
        owners(gt, key, out)
    sys.stdout.write("\n".join(out) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
