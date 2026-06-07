#!/usr/bin/env python3
"""Phase-0 profile analyzer for AScript.

Reads samply (Firefox Profiler format) JSON profiles and reports:
  - top leaf self-time functions (where the CPU instruction pointer sits)
  - bucketed attribution (alloc / gc-refcount / hashing / async / dispatch / ...)
  - inclusive "% of samples whose stack touches the async/tokio machinery"

Usage: python3 analyze.py bench/out/*.json
"""
import json, sys, collections

# (bucket, substrings) — first match wins, so order matters.
BUCKETS = [
    ("alloc",      ["malloc", "free", "realloc", "calloc", "szone", "nanov2",
                     "tiny_", "small_", "__rust_alloc", "__rust_dealloc",
                     "RawVec", "raw_vec", "reserve", "memcpy", "memmove", "memset",
                     "mmap", "munmap", "madvise", "alloc::alloc"]),
    ("gc/refcount",["gcmodule", "CcBox", "Cc<", "RcBox", "drop_in_place",
                    "ptr::drop", "Trace", "trace", "collect_thread", "decrement",
                    "increment", "ManuallyDrop"]),
    ("hashing",    ["SipHash", "siphash", "Hasher", "make_hash", "FxHash", "ahash",
                    "::hash", "indexmap", "IndexMap", "equivalent", "RandomState"]),
    ("async",      ["tokio", "spawn", "::poll", "poll_", "Waker", "waker", "Notify",
                    "LocalSet", "JoinHandle", "scheduler", "runtime", "park",
                    "SharedFuture", "ResultCell", "AbortHandle", "yield_now"]),
    ("string",     ["String", "str::", "to_string", "::fmt", "Display", "format",
                    "from_utf8", "write_str", "push_str"]),
    ("json/serde", ["json", "serde", "stringify"]),
    ("dispatch/vm",["run_loop", "vm::run", "Op::", "CallFrame", "fiber", "Chunk",
                    "adapt", "ic::", "shape", "vm::value", "compile::"]),
    ("interp",     ["interp::", "apply_binop", "apply_unop", "eval_expr", "exec",
                    "call_value", "run_body"]),
    ("workflow",   ["workflow", "det::", "determinism"]),
    ("fs/syscall", ["fs::", "File", "write_all", "fsync", "read_to", "open", "stat",
                    "unlink", "remove", "syscall"]),
]

def bucket_of(name):
    for b, subs in BUCKETS:
        for s in subs:
            if s in name:
                return b
    return "other"

def analyze(path):
    d = json.load(open(path))
    leaf = collections.Counter()
    bucket = collections.Counter()
    total = 0
    async_incl = 0
    for t in d["threads"]:
        strings = t["stringArray"]
        func_name = t["funcTable"]["name"]
        frame_func = t["frameTable"]["func"]
        st_frame = t["stackTable"]["frame"]
        st_prefix = t["stackTable"]["prefix"]
        weights = t["samples"].get("weight") or [1] * t["samples"]["length"]

        def name_of_stack(si):
            fi = st_frame[si]
            return strings[func_name[frame_func[fi]]]

        for si, w in zip(t["samples"]["stack"], weights):
            if si is None:
                continue
            w = w or 1
            total += w
            nm = name_of_stack(si)
            leaf[nm] += w
            bucket[bucket_of(nm)] += w
            # inclusive: walk to root, does any frame touch async machinery?
            cur, touched = si, False
            while cur is not None:
                n = name_of_stack(cur)
                if any(s in n for s in ("tokio", "spawn", "Future", "Waker",
                                        "Notify", "LocalSet", "SharedFuture",
                                        "poll")):
                    touched = True
                    break
                cur = st_prefix[cur]
            if touched:
                async_incl += w
    return total, leaf, bucket, async_incl

def pct(n, d):
    return f"{100.0*n/d:5.1f}%" if d else "  n/a"

for path in sys.argv[1:]:
    total, leaf, bucket, async_incl = analyze(path)
    name = path.split("/")[-1].replace(".json", "")
    print(f"\n{'='*64}\n{name}   ({total} weighted samples)\n{'='*64}")
    print(f"  inclusive: {pct(async_incl,total)} of samples are under the async/tokio machinery")
    print("  --- bucket self-time ---")
    for b, c in bucket.most_common():
        print(f"    {b:14s} {pct(c,total)}  ({c})")
    print("  --- top 15 leaf functions (self-time) ---")
    for nm, c in leaf.most_common(15):
        short = nm if len(nm) <= 68 else nm[:65] + "..."
        print(f"    {pct(c,total)}  {short}")
