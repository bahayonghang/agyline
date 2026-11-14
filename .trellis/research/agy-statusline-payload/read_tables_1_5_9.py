"""Read the constant tables that decide enumerated payload values in agy.exe 1.5.9.

Usage:
    python read_tables_1_5_9.py [path to agy.exe]

The virtual addresses below were taken from the 1.5.9 windows/amd64 build
(disasm_go_func.py shows where each one is referenced) and are valid only for
that build. Read-only; standard library only.
"""
import os
import sys

from gobin import PE, GoTypes, discover_types_base

DEFAULT_EXE = os.path.join(os.environ.get("LOCALAPPDATA", ""), "agy", "bin", "agy.exe")
EXPECTED_SIZE = 231726744

# store.UIPhase.String: 26 (pointer, length) pairs.
UIPHASE_NAMES = 0x144D625C0
UIPHASE_COUNT = 26
# store.BuildStatusLineData: jump table indexed by UIPhase, and what each
# target block assigns to StatusLineData.AgentState.
AGENT_STATE_JUMPS = 0x144D06D40
AGENT_STATE_TARGETS = {
    0x14382A414: "authenticating",
    0x14382A425: "error",
    0x14382A433: "initializing",
    0x14382A442: "idle",
    0x14382A450: "working",
    0x14382A45E: "tool_use",
    0x14382A46C: "reviewing",
    0x14382A47A: "idle",
    0x14382A488: "working",
    0x14382A496: "idle",
    0x14382A4A4: "idle",
}
# backend.IsValidEffort / parseModelSlug: []string of effort suffixes.
EFFORTS_SLICE = 0x14D7F3110
# store.(*Store).UserVisibleModes: []types.AgentMode cycled by shift+tab.
CYCLE_MODES_SLICE = 0x14D7F0CD0
# Option passed to encoding/json/v2.Marshal in (*StatusLineRunner).run.
MARSHAL_OPTION_ITAB = 0x14B7788E8
MARSHAL_OPTION_DATA = 0x14D862060


def gostr(pe, ptr, n):
    if n == 0:
        return ""
    off = pe.va2off(ptr)
    return bytes(pe.m[off:off + n]).decode("utf-8", "replace")


def string_slice(pe, va):
    off = pe.va2off(va)
    ptr, n = pe.u64(off), pe.u64(off + 8)
    arr = pe.va2off(ptr)
    return [gostr(pe, pe.u64(arr + i * 16), pe.u64(arr + i * 16 + 8)) for i in range(n)]


def main():
    exe = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_EXE
    pe = PE(exe)
    if pe.m.size() != EXPECTED_SIZE:
        print("warning: file size %d differs from the 1.5.9 build (%d); addresses may be wrong" % (
            pe.m.size(), EXPECTED_SIZE))
    base, _root = discover_types_base(
        pe, "types.StatusLineData", "ToolConfirmationPending", 'json:"tool_confirmation_pending,omitempty"')
    gt = GoTypes(pe, base)

    print("UIPhase -> agent_state")
    names = pe.va2off(UIPHASE_NAMES)
    jumps = pe.va2off(AGENT_STATE_JUMPS)
    for i in range(UIPHASE_COUNT):
        name = gostr(pe, pe.u64(names + i * 16), pe.u64(names + i * 16 + 8))
        target = pe.u64(jumps + i * 8)
        print("  %2d %-24s -> %s" % (i, name, AGENT_STATE_TARGETS.get(target, "?%#x" % target)))

    print("effort suffixes: %s" % ", ".join(string_slice(pe, EFFORTS_SLICE)))
    print("cycle modes: %s" % ", ".join(string_slice(pe, CYCLE_MODES_SLICE)))

    itab = pe.va2off(MARSHAL_OPTION_ITAB)
    data = pe.va2off(MARSHAL_OPTION_DATA)
    print("json option: interface %s, concrete type %s" % (
        gt.type_string(pe.u64(itab)), gt.type_string(pe.u64(itab + 8))))
    print("  Flags.Presence = %#x" % pe.u64(data))
    print("  Flags.Values   = %#x" % pe.u64(data + 8))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
