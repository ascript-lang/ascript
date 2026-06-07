#!/usr/bin/env python3
"""Parse macOS `sample` call-graph output → self-time attribution by bucket.

`sample` prints an indented call tree per thread; each line is
    <indent markers> <inclusive-count> <symbol>  (in <lib>) + <off>  [0x..] file:line
Self-time(node) = node.count - sum(child counts). We aggregate self-time by
symbol across ALL threads, then bucket by substring. Usage:
    python3 parse_sample.py <name> file.sample.txt
"""
import re, sys, collections

BUCKETS = [
    ("alloc",       ["malloc", "free", "realloc", "calloc", "szone", "nanov2",
                     "tiny", "small_", "rust_alloc", "rust_dealloc", "RawVec",
                     "raw_vec", "memcpy", "memmove", "memset", "mmap", "munmap",
                     "madvise", "alloc::alloc", "Allocator", "::with_capacity"]),
    ("gc/refcount", ["gcmodule", "CcBox", "::Cc", "RcBox", "drop_in_place",
                     "ptr::drop", "::trace", "Trace", "collect_thread",
                     "ManuallyDrop", "Arc", "::clone"]),
    ("hashing",     ["SipHash", "siphash", "Hasher", "make_hash", "FxHash",
                     "ahash", "::hash", "indexmap", "IndexMap", "equivalent",
                     "RandomState", "get_index_of"]),
    ("async",       ["tokio", "spawn", "::poll", "poll_", "Waker", "waker",
                     "Notify", "LocalSet", "JoinHandle", "scheduler", "block_on",
                     "park", "SharedFuture", "ResultCell", "AbortHandle",
                     "enter_runtime", "Future", "async_task", "kevent",
                     "mach_absolute_time", "Driver::turn"]),
    ("string",      ["String", "str::", "to_string", "::fmt", "Display",
                     "format", "from_utf8", "write_str", "push_str", "Template"]),
    ("json/serde",  ["json", "serde", "stringify", "to_json"]),
    ("dispatch/vm", ["run_loop", "vm::run", "Op::", "CallFrame", "::fiber",
                     "Chunk", "adapt", "ic::", "shape", "vm::value_ext",
                     "read_member", "call_value"]),
    ("interp",      ["interp::", "apply_binop", "apply_unop", "eval_expr",
                     "::exec", "run_body", "global_env"]),
    ("workflow",    ["workflow", "det::", "determinism"]),
    ("fs/syscall",  ["::fs", "File", "write_all", "fsync", "read_to", "open",
                     "stat", "unlink", "::remove", "syscall", "fcntl", "close",
                     "::write", "bufwriter", "BufWriter"]),
    ("idle/wait",   ["ulock_wait", "psynch", "cvwait", "mach_msg",
                     "thread_start", "_pthread"]),
]

def bucket_of(name):
    for b, subs in BUCKETS:
        for s in subs:
            if s in name:
                return b
    return "other"

LINE = re.compile(r'^([ +!:|]*?)(\d+)\s+(.*)$')

def clean(sym):
    sym = sym.split("  (in ")[0].strip()
    sym = re.sub(r'::h[0-9a-f]{16}$', '', sym)
    return sym

THREAD_ROOT = re.compile(r'^\s*(\d+)\s+Thread_\S+(.*)$')

def parse(path):
    """Attribute self-time only on WORKER threads. The process has an idle main
    thread blocked in pthread_join (com.apple.main-thread / DispatchQueue_1) that
    would otherwise contribute a flat 50%; we skip it."""
    self_time = collections.Counter()
    total = 0
    stack = []
    in_calls = False
    keep = False  # is the current thread a worker (not the idle main thread)?

    def flush():
        while stack:
            d, c, nm, ch = stack.pop()
            self_time[nm] += max(0, c - ch)

    for raw in open(path):
        line = raw.rstrip("\n")
        if line.startswith("Call graph:"):
            in_calls = True
            continue
        if not in_calls:
            continue
        if line.strip().startswith("Total number") or line.startswith("Sort by"):
            break
        tr = THREAD_ROOT.match(line)
        if tr:
            flush()
            hdr = tr.group(2)
            keep = ("main-thread" not in hdr) and ("DispatchQueue_1" not in hdr)
            continue
        if not keep:
            continue
        m = LINE.match(line)
        if not m:
            continue
        indent, count, rest = len(m.group(1)), int(m.group(2)), m.group(3)
        name = clean(rest)
        while stack and stack[-1][0] >= indent:
            d, c, nm, ch = stack.pop()
            self_time[nm] += max(0, c - ch)
        if stack:
            stack[-1][3] += count
        else:
            total += count
        stack.append([indent, count, name, 0])
    flush()
    return total, self_time

def pct(n, d):
    return f"{100.0*n/d:5.1f}%" if d else "  n/a"

name, path = sys.argv[1], sys.argv[2]
total, st = parse(path)
buckets = collections.Counter()
for nm, c in st.items():
    buckets[bucket_of(nm)] += c
print(f"\n{'='*66}\n{name}   ({total} samples @ 1ms)\n{'='*66}")
print("  --- bucket self-time ---")
for b, c in buckets.most_common():
    print(f"    {b:14s} {pct(c,total)}  ({c})")
print("  --- top 14 leaf symbols (self-time) ---")
for nm, c in st.most_common(14):
    short = nm if len(nm) <= 70 else nm[:67] + "..."
    print(f"    {pct(c,total)}  {short}")
