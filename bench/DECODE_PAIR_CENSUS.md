# DECODE §5.1 — pair/triple census (Unit B part 1)

The **measured** dynamic-instruction-adjacency census whose top entries Task 8 fuses
into superinstructions. The DECODE spec mandates the `FUSION_CANDIDATES` set be chosen
from MEASURED data — never guessed — so this file is the committed verbatim record.

## How this was produced

- **Command** (the census feature + harness are FULLY `#[cfg(feature = "decode-census")]`
  — compiled out of every default/production build, the JIT-spec §2.1 "not there"
  discipline; zero Gate-12 hot-path exposure):

  ```
  cargo test --release --features decode-census --test decode_census -- --ignored --nocapture
  ```

- **What runs:** the curated `bench/profiling/*.as` programs (the perf-shaped hot loops)
  PLUS the runnable example corpus (`examples/**`), each executed in **forced-decode**
  census mode (decode threshold = 0 → every proto decoded, so the record driver sees the
  real stream). Blocking server / relative-import examples are skipped (they cannot run
  headless to completion); nondeterministic examples ARE run (the census never compares
  output, only the record stream).

- **Counting (the load-bearing correctness rule):** the record driver records consecutive
  `(prev, op)` PAIRS and `(prev2, prev, op)` TRIPLES via burst-local `prev`/`prev2`
  tracked on the `RecordSource`, **within basic blocks ONLY**. `prev`/`prev2` reset to
  `None` at every basic-block boundary — a taken jump or frame push/pop (the `fetch` →
  `resync` discontinuity), a block-terminator op (every jump/branch/loop + the
  control-leaving and call/suspension ops — a conditional branch ends a block even on its
  not-taken fall-through), and burst entry (a fresh `RecordSource` per burst, so
  escalations and fallbacks reset too). So NO pair/triple is ever counted across a boundary
  a fused superinstruction could not legally cross.

## Machine / date

- **Date:** 2026-06-14 (UTC)
- **Host:** Darwin 25.5.0 arm64 — Apple M4
- **Toolchain:** rustc 1.96.0 (ac68faa20 2026-05-25), `--release`

> Re-runs may differ by a handful of records (a couple of corpus examples use
> time/crypto/network nondeterminism) — the **RANKS are stable**, which is what Task 8
> consumes.

## Verbatim output

```
=== DECODE §5.1 PAIR/TRIPLE CENSUS ===
programs run = 124 (skipped 5 blocking/relative; 0 errored)
total records retired (within basic blocks) = 370029796

--- TOP 40 PAIRS (prev → op) ---
         count    %recs   pair
      31100513   8.405%   GetLocal -> GetProp
      27086420   7.320%   GetLocal -> GetLocal
      21332341   5.765%   GetLocal -> Const
      21058046   5.691%   GetProp -> Add
      20526569   5.547%   Const -> GetLocal
      12882831   3.482%   Pop -> GetLocal
      12600023   3.405%   Add -> GetLocal
      12263224   3.314%   Const -> Const
      10413301   2.814%   SetLocal -> GetGlobal
      10317357   2.788%   GetGlobal -> GetLocal
      10173155   2.749%   GetLocal -> Add
       8758119   2.367%   Const -> Mul
       7807217   2.110%   Add -> SetLocal
       7807215   2.110%   SetLocal -> Loop
       7807144   2.110%   SetGlobal -> Pop
       7807133   2.110%   GetLocal -> SetLocal
       7807111   2.110%   GetLocal -> RangeHasNext
       7807111   2.110%   RangeHasNext -> JumpIfFalse
       7805122   2.109%   Add -> SetGlobal
       7009520   1.894%   Const -> Add
       7003266   1.893%   SetLocal -> Const
       6400000   1.730%   Mul -> Const
       6033474   1.631%   GetProp -> Const
       6000009   1.621%   NewObject -> SetLocal
       6000000   1.621%   Add -> NewObject
       5316159   1.437%   SetLocal -> GetLocal
       4806497   1.299%   GetLocal -> ArrayElem
       4806283   1.299%   ArrayElem -> SetLocal
       4003048   1.082%   Dup -> JumpIfFalse
       4000006   1.081%   GetProp -> Dup
       2666079   0.721%   Const -> Gt
       2403243   0.649%   GetLocal -> CheckArrayDestructure
       2403243   0.649%   CheckArrayDestructure -> Pop
       2358071   0.637%   Mul -> GetLocal
       1814495   0.490%   GetGlobal -> GetGlobal
       1006038   0.272%   Const -> Mod
       1006030   0.272%   Mod -> Const
       1003461   0.271%   Const -> Template
       1000003   0.270%   Template -> SetLocal
        800963   0.216%   Const -> GetIndex

--- TOP 40 TRIPLES (prev2 → prev → op) ---
         count    %recs   triple
      21058040   5.691%   GetLocal -> GetProp -> Add
      19514253   5.274%   Const -> GetLocal -> Const
      12000006   3.243%   Add -> GetLocal -> GetProp
      12000003   3.243%   GetProp -> Add -> GetLocal
      10171139   2.749%   GetLocal -> GetLocal -> Add
       8606186   2.326%   SetLocal -> GetGlobal -> GetLocal
       7807215   2.110%   Add -> SetLocal -> Loop
       7807123   2.110%   GetLocal -> GetLocal -> GetLocal
       7807111   2.110%   GetLocal -> GetLocal -> RangeHasNext
       7807111   2.110%   GetLocal -> RangeHasNext -> JumpIfFalse
       7807081   2.110%   GetLocal -> Add -> SetLocal
       7805122   2.109%   Add -> SetGlobal -> Pop
       7804068   2.109%   Pop -> GetLocal -> GetLocal
       7804028   2.109%   SetGlobal -> Pop -> GetLocal
       7003013   1.893%   SetLocal -> Const -> GetLocal
       6706080   1.812%   GetGlobal -> GetLocal -> GetProp
       6700003   1.811%   GetProp -> Add -> SetGlobal
       6606394   1.785%   GetLocal -> Const -> Add
       6500018   1.757%   GetLocal -> SetLocal -> Const
       6400110   1.730%   GetLocal -> Const -> Mul
       6400000   1.730%   Const -> Mul -> Const
       6256011   1.691%   Const -> Const -> GetLocal
       6033297   1.630%   GetLocal -> GetProp -> Const
       6003690   1.622%   Const -> Const -> Const
       6003050   1.622%   GetLocal -> Const -> Const
       6000004   1.621%   NewObject -> SetLocal -> GetGlobal
       6000000   1.621%   Const -> Add -> NewObject
       6000000   1.621%   Add -> NewObject -> SetLocal
       6000000   1.621%   Mul -> Const -> GetLocal
       4806283   1.299%   GetLocal -> ArrayElem -> SetLocal
       4000006   1.081%   GetLocal -> GetProp -> Dup
       4000000   1.081%   GetProp -> Dup -> JumpIfFalse
       2669016   0.721%   Pop -> GetLocal -> GetProp
       2666020   0.720%   GetProp -> Const -> Gt
       2406253   0.650%   ArrayElem -> SetLocal -> GetLocal
       2403243   0.649%   Pop -> GetLocal -> ArrayElem
       2403243   0.649%   GetLocal -> CheckArrayDestructure -> Pop
       2403243   0.649%   SetLocal -> GetLocal -> CheckArrayDestructure
       2403243   0.649%   CheckArrayDestructure -> Pop -> GetLocal
       2403148   0.649%   SetLocal -> GetLocal -> ArrayElem

```

## Reading the table for Task 8

The **top pairs ARE the deliverable** — Task 8 selects ≤ 8 fused forms from the
highest-frequency legal pairs (spec §5.1/§5.3), subject to: base operands fitting the
record payload (≤ u16 each, packable two-per-u32) and composing ONLY shared-helper calls
(a candidate needing reimplemented semantics is recorded-and-rejected). Notable
high-frequency, well-shaped candidates from the data above:

- `GetLocal -> GetProp` (8.4%) and the triple `GetLocal -> GetProp -> Add` (5.7%) — the
  dominant field-read-then-use shape.
- `GetLocal -> GetLocal` (7.3%) — two adjacent local reads (operand-stack staging).
- `GetLocal -> Const` (5.8%) / `Const -> GetLocal` (5.5%) — local/const staging into a binop.
- `GetProp -> Add` (5.7%) — field-read feeding arithmetic.
- `GetLocal -> RangeHasNext -> JumpIfFalse` / `RangeHasNext -> JumpIfFalse` (2.1%) — the
  for-range loop-condition spine (a terminator-anchored in-block pair).
- `Add -> SetLocal` / `GetLocal -> SetLocal` (~2.1%) — accumulate-store shapes.

Final selection, payload-fit checks, and the recorded-and-rejected list live in Task 8's
`FUSION_CANDIDATES` doc-comment.
