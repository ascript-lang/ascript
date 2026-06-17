#!/usr/bin/env python3
"""
gen_module_tree.py — deterministic module-tree generator for WARM A benchmarks.

Generates N modules in a chain+fan import graph under OUTPUT_DIR:
  - One root entry module (main.as) that imports the first half of leaves
    plus the chain head.
  - A linear chain of ceil(N/3) modules (chain_0 imports chain_1 imports …).
    The chain tail imports leaf modules to create fan-out.
  - The remaining modules are plain leaf modules (each with a few fns/classes).
  - The entry module computes a checksum from all exported values and prints
    exactly ONE line: "checksum=<value>" — so cold vs warm output is comparable.

Usage:
    python3 bench/gen_module_tree.py --n 100 --out /tmp/bench_tree_100
    # Writes main.as, module_000.as, module_001.as … into /tmp/bench_tree_100/
    # Run: ascript run /tmp/bench_tree_100/main.as
    # Output: checksum=<deterministic number>

Graph structure for N modules total (including main.as):
  Let L = N - 1  (leaf + chain modules, not counting main)
  Chain length C = max(1, L // 3)
  Fan-out leaf count F = L - C
  - main.as: imports chain_0 + the first min(F, 10) leaf modules directly
  - chain_0 imports chain_1, …, chain_{C-2} imports chain_{C-1}
  - chain_{C-1} (tail) imports the remaining leaf modules
  - each leaf module exports unique-named functions (leaf_N_compute, leaf_N_double)
  - each chain module exports a unique chain_N_sum function

All exports are uniquely named (leaf_IDX_* / chain_IDX_*), so no aliasing is
needed and every module can be imported by name directly.
"""

import argparse
import hashlib
import os
import sys


def deterministic_seed(n: int, idx: int) -> int:
    """Deterministic integer seed from (n, idx) — for generating stable module content."""
    key = f"warm_bench_seed:{n}:{idx}"
    return int(hashlib.sha256(key.encode()).hexdigest()[:8], 16)


def write_leaf_module(path: str, idx: int, n_total: int) -> None:
    """Write a leaf module with a few fns and a class, all uniquely named."""
    seed = deterministic_seed(n_total, idx)
    base = (seed % 1000) + 1  # 1..1000, deterministic

    with open(path, "w") as f:
        f.write(f"""\
// Auto-generated leaf module {idx} (WARM bench, N={n_total})
// DO NOT EDIT — regenerate with bench/gen_module_tree.py

export fn leaf_{idx}_compute(x: int): int {{
  return x * {base} + {idx}
}}

export fn leaf_{idx}_double(x: int): int {{
  return leaf_{idx}_compute(x) * 2
}}

export class LeafNode{idx:04d} {{
  value: int
  seed: int = {seed % 10000}

  fn node_sum(): int {{
    return self.value + self.seed + {base}
  }}
}}
""")


def write_chain_module(path: str, chain_idx: int, n_total: int,
                       next_chain_idx: int | None,
                       leaf_indices: list[int]) -> None:
    """
    Write a chain module.
    next_chain_idx: index of the next chain link, or None for the tail.
    leaf_indices: list of leaf module indices imported at this chain node.
    """
    seed = deterministic_seed(n_total, 100000 + chain_idx)
    base = (seed % 500) + 1

    imports = []
    call_terms = []

    if next_chain_idx is not None:
        imports.append(
            f'import {{ chain_{next_chain_idx}_sum }} from "./chain_{next_chain_idx:04d}"')
        call_terms.append(f"chain_{next_chain_idx}_sum(x)")
    else:
        call_terms.append("0")

    for li in leaf_indices:
        imports.append(
            f'import {{ leaf_{li}_compute }} from "./module_{li:04d}"')
        call_terms.append(f"leaf_{li}_compute(x)")

    import_block = "\n".join(imports)
    sum_expr = " + ".join(call_terms) if call_terms else "0"

    with open(path, "w") as f:
        f.write(f"""\
// Auto-generated chain module {chain_idx} (WARM bench, N={n_total})
// DO NOT EDIT — regenerate with bench/gen_module_tree.py

{import_block}

export fn chain_{chain_idx}_sum(x: int): int {{
  return ({sum_expr}) + {base}
}}
""")


def write_main(path: str, n_total: int,
               first_chain_idx: int,
               direct_leaf_indices: list[int]) -> None:
    """Write the entry (main.as) that aggregates all values into a checksum."""
    imports = []
    call_terms = []

    imports.append(f'import {{ chain_{first_chain_idx}_sum }} from "./chain_{first_chain_idx:04d}"')
    call_terms.append(f"chain_{first_chain_idx}_sum(42)")

    for li in direct_leaf_indices:
        imports.append(
            f'import {{ leaf_{li}_compute }} from "./module_{li:04d}"')
        call_terms.append(f"leaf_{li}_compute(42)")

    import_block = "\n".join(imports)
    sum_expr = " + ".join(call_terms) if call_terms else "0"

    with open(path, "w") as f:
        f.write(f"""\
// Auto-generated entry module (WARM bench, N={n_total})
// DO NOT EDIT — regenerate with bench/gen_module_tree.py
//
// Prints exactly one line: checksum=<integer>
// The checksum is a deterministic function of all exported leaf/chain values,
// so cold and warm runs produce identical output.

{import_block}

fn mix(v: int): int {{
  // 32-bit avalanche mix — keeps values bounded and fully deterministic
  let x = v & 0xFFFFFFFF
  x = ((x ^ (x >> 16)) * 0x45d9f3b) & 0xFFFFFFFF
  x = ((x ^ (x >> 16)) * 0x45d9f3b) & 0xFFFFFFFF
  return x ^ (x >> 16)
}}

let total = {sum_expr}
print(`checksum=${{mix(total)}}`)
""")


def generate(n: int, out_dir: str) -> None:
    os.makedirs(out_dir, exist_ok=True)

    if n < 2:
        # Degenerate: just main + one leaf
        write_leaf_module(os.path.join(out_dir, "module_0000.as"), 0, n)
        with open(os.path.join(out_dir, "main.as"), "w") as f:
            f.write('import { leaf_0_compute } from "./module_0000"\n')
            f.write('print("checksum=" + leaf_0_compute(42))\n')
        return

    # L = number of non-main modules
    L = n - 1
    chain_len = max(1, L // 3)
    fan_len = L - chain_len

    # Generate leaf modules: module_0000.as … module_{F-1}.as
    for i in range(fan_len):
        write_leaf_module(os.path.join(out_dir, f"module_{i:04d}.as"), i, n)

    # Distribute leaves:
    # - main imports the first min(10, fan_len) directly
    # - the chain tail imports the rest
    direct_count = min(10, fan_len)
    tail_leaf_indices = list(range(direct_count, fan_len))

    # Generate chain modules: chain_0000.as … chain_{C-1}.as
    for ci in range(chain_len):
        next_ci = ci + 1 if ci + 1 < chain_len else None
        # Only the last chain node imports the overflow leaves
        leaves_at_this_chain = tail_leaf_indices if ci == chain_len - 1 else []
        write_chain_module(
            os.path.join(out_dir, f"chain_{ci:04d}.as"),
            ci, n, next_ci, leaves_at_this_chain)

    # Main module
    write_main(
        os.path.join(out_dir, "main.as"),
        n, 0, list(range(direct_count)))


def main():
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--n", type=int, required=True,
                        help="Total module count (including main.as); min 2")
    parser.add_argument("--out", required=True,
                        help="Output directory (created if absent)")
    args = parser.parse_args()

    if args.n < 2:
        print(f"error: --n must be >= 2 (got {args.n})", file=sys.stderr)
        sys.exit(1)

    generate(args.n, args.out)
    print(f"Generated {args.n}-module tree in {args.out}/")
    print(f"  Entry: {args.out}/main.as")


if __name__ == "__main__":
    main()
