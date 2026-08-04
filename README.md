<div align="center">
  <h1>Wreckless Chess Engine</h1>

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
</div>

Wreckless is a UCI chess engine, a fork of [Reckless](https://github.com/codedeliveryservice/Reckless)
by Lihan van der Westhuizen — an open source competitive engine that consistently performs among the
top engines in major tournaments including the
[Chess.com Computer Chess Championship (CCC)][ccc] and [Top Chess Engine Championship (TCEC)][tcec].

Wreckless inherits effectively all of its playing strength from Reckless, including its NNUE
networks. It exists as a place to try search ideas against that baseline.

**This README is graded by evidence, not by enthusiasm.** Every change below sits in a section named
for what is actually known about it — measured, proven-correct, or merely implemented. If you only
read one section, read [Status](#status), then
[Correctness and scale fixes](#correctness-and-scale-fixes).

[ccc]: https://www.chess.com/computer-chess-championship
[tcec]: https://tcec-chess.com

## Contents

- [Status](#status)
- [Quick start](#quick-start)
- [Building from source](#building-from-source)
- [UCI options](#uci-options)
- [Custom commands](#custom-commands)
- [Memory architecture](#memory-architecture)
- [Changes relative to upstream](#changes-relative-to-upstream)
  - [Correctness and scale fixes](#correctness-and-scale-fixes)
  - [Performance](#performance)
  - [Unverified search changes](#unverified-search-changes)
  - [Protocol and usability](#protocol-and-usability)
  - [Removed, and why](#removed-and-why)
- [Testing and tuning](#testing-and-tuning)
- [Acknowledgements](#acknowledgements)

## Status

**Wreckless measures at or above parity with upstream Reckless, and the size of the margin is not
settled.** SPRT runs at 10+0.1, 1 thread, 128 MB hash have landed anywhere from roughly −7 to +3 Elo
with 95% intervals of ±10 to ±20, while a 553-game run against the same upstream build under a
different tournament manager put a release build **+63 Elo** ahead. Those cannot both be right about
the same code, and the discrepancy traced to harness differences rather than to the engine.

That disagreement was eventually pinned down with `bench`, which is deterministic and harness-free:

| | nodes | nps |
| --- | --- | --- |
| fork | 2,798,289 | 715,687 |
| upstream | 2,929,550 | 672,270 |

That is a **1.115× effective search, about +0.16 ply** at an effective branching factor of 2.0 — which
is consistent with the near-zero SPRT results and flatly inconsistent with +63 Elo, which would need
roughly 3.5×. Upstream sits at the same depth in every harness; only this fork's number moves between
them. **The asymmetry is still unexplained.** Until it is, treat any single harness's verdict with
suspicion and prefer `bench` for anything that should be behaviour-neutral.

Two lessons worth carrying:

- **Check what the binary reports, not what you think you built.** A UCI `id name` line carries the
  version and commit hash. More than one conclusion here was drawn from a run whose binary was not
  the code being discussed — including one analysis of a PGN that turned out to mix six different
  builds, because the harness was appending to a fixed filename.
- **Paired openings are not optional.** The +63 run assigned each opening exactly once, so its
  variance is far wider than a naive binomial interval suggests. Colour-swapped pairs are what make
  the pentanomial — and the interval it produces — meaningful.

What is not in doubt is the direction of travel. Earlier states of this fork measured as much as
17 Elo *behind* upstream, and the work that closed that gap was almost entirely arithmetic and
correctness repair rather than new ideas.

**No search addition in this fork has been demonstrated to gain Elo over upstream.** Several are
plausible and none is currently known to lose. The one change with a measurement behind it is a
memory-system one: see [Memory architecture](#memory-architecture).

### The defect class to know about

The recurring bug here is **scale drift**: a term is added to a sum, or a heuristic is ported, and the
coefficients that consume it are never rescaled. Two unit systems collide constantly, and mixing them
is silent:

| quantity | a pawn is worth |
| --- | --- |
| evaluation / search scores | ~321 (startpos) to ~382 (middlegame), per `normalization()` |
| `PieceType::value()` | 109 |
| Stockfish `Value` (when porting constants) | ~208 material, ~328 eval-normalized |

**Before judging any margin in this engine, convert it to pawns.** A threshold that saturates its
clamp within a few hundredths of a pawn is a bug, not a tuning choice.

Two related traps have each cost real Elo here:

- **A signal penalised twice.** A missing TT move was reduced for by both IIR and the LMR/FDS
  reduction bonus, at a coefficient tuned for a search that penalises it once.
- **A prune that can never fire.** Two prunes sharing a stage and a `<= alpha` test will usually have
  one dominate the other, and the loser is invisible in profiles and untunable by SPSA.

Neither shows up in per-feature review, and neither would ever appear in SPSA output — an unreachable
branch has no gradient, and a double-applied one looks like a well-tuned single term. When adding a
heuristic, the question is not only "is this sound" but **"does something else already act on this
signal, and was that thing tuned before or after?"**

## Quick start

Wreckless is not a standalone chess program — it's an engine you plug into a UCI-compatible GUI, such
as [Cute Chess](https://github.com/cutechess/cutechess), [En Croissant](https://encroissant.org), or
[Nibbler](https://github.com/rooklift/nibbler). Build it, then point your GUI at the binary:

```bash
make
# produces ./wreckless (or wreckless.exe on Windows)
```

## Building from source

**Requirements:**

- Rust 1.88.0 or later ([install guide](https://www.rust-lang.org/tools/install))
- Clang (only for Syzygy tablebase support, via the bundled [Fathom](https://github.com/jdart1/Fathom) library)

```bash
make              # standard build, with Syzygy
make no-syzygy    # skips the Clang dependency
```

### Profile-guided optimization (PGO)

PGO runs a profiling pass before the final compile, typically worth a small but real NPS gain. Use it
for anything performance-sensitive; the standard build is fine for development.

```bash
rustup component add llvm-tools
cargo install cargo-pgo

make pgo
```

Or run the three steps manually:

```bash
cargo pgo instrument build --release --bin wreckless   # 1. instrumented binary
cargo pgo run -- bench                                  # 2. profile against the bench suite
cargo pgo optimize build --release --bin wreckless      # 3. rebuild with the profile
# binary lands under target/<your-target-triple>/release/wreckless
```

> **When benchmarking, measure the optimized binary.** `cargo pgo run -- bench` executes the
> *instrumented* build, which carries profiling counters on every branch. Its NPS is not comparable
> to a normal build's — though it is comparable to *another instrumented build*, which is how the
> large-page measurement below was taken.

### Large pages

The transposition table, history tables, NNUE network and accumulator stacks all request 2 MB pages
and fall back silently to normal pages when the OS refuses. On Windows this needs the *Lock pages in
memory* privilege; without it everything still works, just with more TLB pressure. See
[Memory architecture](#memory-architecture) for why it matters.

## UCI options

| Name | Default | Description |
| --- | --- | --- |
| `Hash` | 16 | Transposition table size in MB `[1–262144]` |
| `Threads` | 1 | Number of search threads `[1–512]` |
| `MultiPV` | 1 | Number of principal variations to display `[1–218]` |
| `Ponder` | false | Allow the GUI to let the engine think on the opponent's time |
| `UCI_Chess960` | false | Enable Chess960 (Fischer Random) support |
| `UCI_ShowWDL` | false | Show win/draw/loss estimates in search output |
| `Minimal` | false | Enable minimal UCI output |
| `MoveOverhead` | 100 | Milliseconds reserved for overhead per move `[0–2000]` |
| `Clear Hash` | — | Clear the transposition table |
| `SyzygyPath` | — | Path to Syzygy endgame tablebases |
| `SyzygyProbeDepth` | 1 | Minimum depth to probe tablebases at the piece-count boundary `[1–100]` |
| `SyzygyProbeLimit` | 7 | Maximum number of pieces for tablebase probes `[0–7]` |

`SyzygyProbeDepth` is specific to this fork; upstream probes unconditionally. At the default of 1 the
two behave identically, since the main search never runs below depth 1.

## Custom commands

| Command | Description |
| --- | --- |
| `perft <depth>` | Count leaf nodes at a given depth ([perft][perft]) |
| `bench` | Run the fixed [benchmark][bench] suite — deterministic, and the best tool this repo has |
| `d` | Print the current position as a board diagram plus FEN |
| `eval` | Print the NNUE evaluation of the current position, White's perspective |
| `compiler` | Print the compiler version, target, and flags used to build the binary |
| `speedtest <Threads> <Hash> <Seconds>` | Run a timed performance test across 50 positions |

[perft]: https://www.chessprogramming.org/Perft
[bench]: /src/tools/bench.rs

## Memory architecture

A modern engine is bounded far more by the memory system than by arithmetic, and this section
collects the decisions that follow from that. It is separated out because the reasoning is shared
across the transposition table, the network and the accumulators — and because it contains the only
change in this fork with a real measurement behind it.

### The transposition table is one cache line wide

`Cluster` is 64 bytes, aligned to 64: six 8-byte entries plus two 8-byte key words, holding four
16-bit verification keys each. Probing scans the keys with a SWAR trick
(`x.wrapping_sub(ones) & !x & (ones << 15)` sets the high bit of every lane that matched), so a whole
cluster is checked in a couple of instructions.

It used to be 32 bytes with three entries. **A cache line and a DRAM burst are both 64 bytes, so
every probe already pulled 64 bytes and read half of them** — the sibling cluster sat in L1, paid
for, and was never examined. Widening to a full line raises associativity from 3 to 6 for an
identical total entry count and identical bytes fetched per probe; the three extra replacement
candidates are pure L1 hits. Replacement quality is `depth - 4 * relative_age`, so more candidates
means fewer useful deep entries evicted by a conflict.

Entries are `AtomicU64` and the key words are updated with `fetch_update`, because lazy-SMP threads
write concurrently. An earlier non-atomic read-modify-write on the packed key word let two threads
updating *different* slots silently revert each other.

### Everything large is on 2 MB pages

| structure | size | 4 KB pages | 2 MB pages |
| --- | --- | --- | --- |
| NNUE network | 60 MB | 15,446 | 30 |
| accumulator stacks (per thread) | 1.4 MB | 369 | 2 |
| transposition table | user-set | — | — |
| history tables | — | — | — |

A typical x86 L2 TLB holds about **2,048** entries. The network alone needed 7.5× that. It is indexed
by feature number, so its accesses scatter across the whole table, and every accumulator update
touches it — the hottest random-access structure in the engine and the one most punished by TLB
misses. It was being read straight out of the binary's `.rodata` via `include_bytes!`: zero-copy,
which is the right instinct, but it leaves all 60 MB on 4 KB pages.

**Measured**, on an 8C/16T Zen 3, PGO-instrumented builds, same bench:

```
before:  2,826,642 nodes @ 779,649 nps
after:   2,826,642 nodes @ 828,153 nps     (+6.2%)
```

Node count identical to the unit. That identity is what confirms the change is purely a
memory-system one; instrumented builds are noisy, so treat the magnitude as approximate. Costs ~60 MB
of RSS and one `memcpy` at startup.

The same argument applies to the per-thread accumulator stacks (`pst_stack`, `threat_stack`), which
are the NNUE's KV cache — touched on every make and unmake. At 240 plies × 3072 B each they are
~1.4 MB per thread, and `vec![X; MAX_PLY]` clones into every slot, so all of it is faulted in at
startup rather than left lazily untouched. Now one huge page each.

### Why not PagedAttention

vLLM's paged KV cache solves a **capacity** problem: sequence lengths are unbounded and unknown, so
reserving max-length KV per request wastes most of the GPU. Small blocks plus an indirection table
fix that.

Chess search has the opposite shape. The working set is fixed, small and known-bounded — 240 plies of
accumulators is 1.4 MB per thread, of which a typical search touches perhaps a quarter. The
over-reservation is about 1 MB per thread, which constrains nothing. What *does* constrain the engine
is TLB reach, and large pages and block-allocation pull in opposite directions: on Windows a
large-page allocation is `MEM_COMMIT`-backed immediately, so lazy block commitment saves nothing
unless you give up the pages that measured +6.2%.

The general point generalises past this one case: **borrowed techniques carry their original
bottleneck as an assumption.** LLM inference is capacity-bound at the KV cache; chess search is
latency-bound at a working set that fits in RAM many times over.

### Where the other LLM-inference analogies land

| concept | equivalent here | status |
| --- | --- | --- |
| Prefix cache | transposition table | present — and *stronger*: keyed on position hash, so it hits transpositions a prefix tree cannot |
| RadixAttention | ↑ same | subsumed. Radix trees share by token prefix; two move orders reaching one position share here and would not there |
| KV cache | NNUE accumulator stack | present — incremental deltas, `accurate` flags, lazy replay |
| Speculative decoding | ProbCut | present — qsearch drafts, shallow search verifies, rejection falls back to full depth |
| FlashAttention / FlashInfer | AVX2 / AVX-VNNI `dpbusd`, NNZ sparse skipping | present; the memory-pressure half is the large-page work above |
| Context parallelism | Lazy SMP over the tree | present |
| Tensor parallelism | — | **does not map.** The net is 768→16→32; splitting one forward pass across threads means a barrier per node in a latency-bound path where the work is nanoseconds. SIMD lanes already are the tensor parallelism |

## Changes relative to upstream

Grouped by what is actually known about each change:

- **[Correctness and scale fixes](#correctness-and-scale-fixes)** — defects with a demonstrable wrong
  answer, independent of whether any feature helps or hurts. The changes worth trusting.
- **[Performance](#performance)** — split by whether they preserve node counts.
- **[Unverified search changes](#unverified-search-changes)** — implemented, correct as far as
  testing shows, never demonstrated to gain Elo. Experiments.
- **[Protocol and usability](#protocol-and-usability)** — no effect on playing strength.
- **[Removed, and why](#removed-and-why)** — worth reading before trusting anything above it.

### Correctness and scale fixes

#### Search and evaluation

- **Qsearch delta pruning violated fail-soft.** A pruned capture's optimistic bound
  (`eval + material_gain + margin`) is strictly above the standing pat that seeds `best_score`, but
  the prune skipped the move without raising it. Qsearch returned an upper bound *lower* than the one
  it had proven, stored it as `Bound::Upper`, and the parent negated it into a score *higher* than
  justified — capped by the margin, so it surfaced as many small overestimates rather than few large
  ones. Upstream raises `best_score` at both of its own futility prunes; this block was the sole
  violator of a convention the fork inherited. Measured: the fork had been taking **22% more
  half-pawn evaluation collapses** than upstream (z = +11.0); after the fix the two are level.
- **Qsearch delta pruning mixed unit systems.** The captured piece was credited with a raw
  `PieceType::value()` against an `eval`-scaled margin, understating every capture roughly threefold.
  Now converted (`qs_delta_piece_scale`, 192/64 ≈ 3.0, against a true ratio of ~2.94).
- **Qsearch delta pruning ignored promotion value.** For a non-capture promotion, `type_on()` reads an
  empty square, crediting nothing for the ~1133cp actually gained. Now credited separately.
- **A missing TT move was penalised twice.** Upstream reduces late moves by `2204` (LMR) / `2168`
  (FDS) when there is no TT move, tuned for a search that penalises that signal *once*. This fork
  also has Internal Iterative Reductions, which decrement depth by a full ply on the same signal — so
  at a cut node with no TT move at depth ≥ 6, both fired and late moves took roughly **3.15 plies of
  reduction where the coefficient assumes 2.15**, about 47% more. Cut nodes are most of the tree and
  a fresh node usually has no TT move, so this was not a corner case. The correction is exact rather
  than estimated: reductions are in 1/1024 plies, so IIR's ply is 1024 units, and `2204 − 1024 = 1180`
  / `2168 − 1024 = 1144` restores upstream's late-move treatment while leaving IIR's effect on the
  *first* move — the part it exists for — intact.

  Stockfish confirms the result independently. It runs *both* mechanisms — `if (!ss->followPV &&
  !allNode && depth >= 6 && !ttData.move) depth--;` alongside `if (!ttData.move) r += 1127;` — so
  pairing IIR with an LMR bonus is correct; what matters is the bonus being sized for a search that
  *has* IIR. The corrected 1180 lands within 5% of Stockfish's 1127, derived from unit arithmetic
  rather than copied.
- **The `history` sum silently rescaled seven tuned coefficients.** It feeds `lmp_history`,
  `fp_history`, `bnfp_history`, `hp_margin`, `see_q_hist`, `see_n_hist` and
  `lmr_quiet_hist`/`fds_quiet_hist`, all tuned against upstream's `quiet_history + conthist(1) +
  conthist(2)`. Lags added beyond that widened the sum without anyone adjusting the consumers. Now
  read at the same relative strengths the update writes them, then normalized back onto upstream's
  two units.
- **The continuation-history weight total is enforced at compile time.** The six per-lag weights in
  quiet scoring must sum to upstream's four-lag total (4817), because `good_quiet_threshold` is
  calibrated against the resulting score distribution — redistributing weight between lags is safe,
  changing the total silently rescales the good/bad quiet split. That invariant was held by a comment;
  it is now `CONTHIST_WEIGHTS` plus a `const` assertion, so violating it fails the build.
- **Continuation-history updates used a running prefix count.** `positive_count` was incremented and
  used to index the consistency multipliers within the same loop iteration, so lag 1 could only ever
  reach `multipliers[1]` while lag 6 could reach `multipliers[6]`. The nearest lags — the ones move
  ordering leans on hardest — were damped (94–103) against the distant ones (121–126) purely by
  position in the loop. Now counted across all eligible lags before any bonus is applied.
- **Continuation-history pointers broke Rust's aliasing rules.** `subtable_ptr` materialised a `&mut`
  and coerced it to a raw pointer, which the search holds across many later borrows of the same
  table; `update` took `&mut self` while writing *through* that pointer, handing LLVM a `noalias`
  promise its own argument immediately breaks. Restored upstream's form (`&raw mut`, `&self`
  receiver).
- **Aspiration fail-low widened the re-search instead of narrowing it.** Collapsing `beta` to the old
  `alpha` made the window 1.5× wider on a shallow fail-low and 3× wider on a deep one — the opposite
  of the rationale the code carried. Now re-centres on the failing score.
- **Null-move zugzwang guard tested the wrong quantity.** `board.material()` includes pawns, so a
  pawn-heavy, piece-empty endgame — precisely the zugzwang case the guard exists for — could clear
  the threshold and be null-move pruned anyway. Now `non_pawn_material()`.
- **The evaluation could come back sign-flipped.** The fiftymove damping factor is `(200 - clock)/200`
  and `clock` is a `u8` read straight from the FEN, so 201..=255 is representable and the multiplier
  goes negative. `is_draw` gates every node except the root, so the reachable case was a root FEN with
  a clock above 200 feeding a *negated* static eval into aspiration and every margin that reads it.
  Clamped. Unreachable from legal play, so inert for normal games.
- **`LowPlyHistory` bounds lived in its callers.** `get`/`update` indexed `entries[ply]` directly
  while the search calls them from any ply up to `MAX_PLY` — correct only because all three call
  sites happened to test `ply < MAX_LOW_PLY`. The bound moved inside the type, and a `const` assertion
  pins the `>= 2` that `shift()`'s `rotate_left(2)` assumes.

#### Transposition table

- **A phantom hit on every never-written slot.** An untouched slot has key 0, so `lookup_key` matched
  it for any position whose verification key is also 0 — roughly 1 probe in 65,536 — and returned an
  all-zero payload as a hit. Not harmless: `raw_eval == 0` passes `is_valid`, so the node skipped the
  network and evaluated the position as dead level, and in qsearch `Bound::None` fell through to the
  permissive arm and returned 0 as a cutoff. Detectable only because `TtDepth::NONE` is −2 rather
  than 0: `to_tt` offsets by +2, so an untouched `offset_depth` decodes to `NONE`. The replacement
  scan already relied on exactly this test; the probe path never used it.
- **`TtDepth::NONE` was unreachable.** The depth encoding gave a real `SOME`-depth write and a
  never-written slot the same raw byte, so the replacement logic's "found a free slot" path could
  never fire. Re-encoded so an empty cluster is uniquely identifiable — which is also what made the
  phantom-hit fix above possible.
- **Verification-key race under multithreading.** The packed key word was updated with a non-atomic
  read-modify-write called concurrently by every lazy-SMP thread. Two threads updating *different*
  slots could silently revert each other — corrupting a sibling's key, not just the writer's own
  entry. Now `fetch_update` on an `AtomicU64`.
- **A deeper entry's move refresh was silently dropped.** Once entries became atomic, `entry` was a
  local copy rather than a `&mut` into the cluster, so the early return that keeps an existing deeper
  entry stopped persisting the refreshed best move. That path runs whenever a position with a deeper
  entry is re-reached — common — and the TT move is tried first at every node.
- **TT-only ProbCut trusted qsearch entries at shallow depth.** Its gate is `tt_depth >= depth - 4`,
  but `TtDepth::SOME` is −1, so at depth 3 or below that reduces to `tt_depth >= -1` — which a
  *qsearch* entry satisfies. The cutoff then returned a fabricated `beta + margin` on the strength of
  an entry that never came from a real search. Floor now clamped at 0.
- **`hashfull` under-reported on small tables.** It sampled `take(1000)` clusters but always divided
  by a full 1000 clusters' worth, so a table with fewer clusters than that read low — and would now
  read 2× low, since widening the cluster halved the cluster count for a given size. Now divides by
  what was actually sampled.

#### Time management

- **The soft/hard split disabled itself in the middlegame.** `soft_limit` scales the soft bound by a
  multiplier that is the product of five independently-bounded factors — node fraction, PV stability,
  eval stability, score trend, best-move stability — with no joint cap. Its ceiling is ~8.4× with a
  perfectly stable best move and ~16–18× once the best move has changed a few times. Meanwhile the
  Fischer hard/soft ratio *shrinks* as the game goes on:

  | move | hard/soft |
  | --- | --- |
  | 10 | 28.1× |
  | 20 | 19.9× |
  | 30 | 16.6× |
  | 60 | 13.3× |

  So from roughly move 20 onward an unstable position produces a scaled soft bound sitting **past**
  the hard bound, and the soft limit stops binding entirely — the hard bound decides the move,
  spending 72.8% of the remaining clock on it. Worse, the hard bound is polled mid-search rather than
  between iterations, so the cutoff lands part-way through an iteration and that work is discarded.
  The split switched itself off exactly when instability meant it mattered most. The scaled soft
  bound is now clamped to the hard bound, which keeps the stop on an iteration boundary at every move
  number and cannot make the engine think longer than it already would.
- **The falling-eval factor is a two-level switch — diagnosed, not fixed.** The linear structure is
  Stockfish's and the coefficient ratio was preserved, but the constants were carried across unscaled
  from a scale where a pawn is ~208 to one where it is ~321–382. With `tm_trend_base 7426`,
  `tm_trend_diff 480` and the `[7214, 14031]` clamp, the ceiling is cleared after
  `(14031-7426)/480` = **13.8 units, or 0.043 pawns**, and the floor sits under any gain at all. It is
  bang-bang with essentially no proportional band, and `tm_trend_recent` stacks a second saturating
  term on an already-saturated one — which is also why SPSA has never found signal here. There is no
  gradient inside the clamp.

  An earlier revision rescaled these so the ceiling sat ~0.37 pawns out. **That was reverted** in
  favour of upstream's untouched values, on the grounds that those are the measured ones and a bad
  constant in a time-management path costs games rather than nodes. This README went on describing
  the rescale for some time after it no longer existed; the shipped behaviour is the 0.043-pawn
  switch above.

  What changed is only that the values live in `parameters.rs` as fixed point, and that their SPSA
  bounds were widened (`tm_trend_diff` floor 240 → 40, `tm_trend_recent` 115 → 20). The proportional
  band would need roughly `diff 51 / recent 25`, and the old floors were 4.7× too high to reach it —
  tuning was locked inside the saturated regime it was supposed to escape. **Defaults are unchanged;
  this makes the region reachable, it does not assume it is better.**
- **`go mate N` stopped when *being* mated.** The condition tested
  `Score::MATE - score.abs() <= moves * 2`, and the `.abs()` made a mate against us satisfy it just
  as well as one we had found — so `go mate 3` in a lost position stopped and reported the mate being
  delivered *to* us as its answer. UCI asks for a mate in N, not for any decisive line within N.
- **Cyclic (`movestogo`) hard bound could consume the whole clock.** With a small `movestogo`, five
  times the per-move allocation already exceeds what remains, collapsing the safety bound to
  "everything left" with another move still due. Now reserves one allocation.
- **`go movetime` ignored Move Overhead.** Only the fixed 15 ms was subtracted, so a GUI on the
  default 100 ms overhead got `ms - 15` of thinking and could flag on a slow connection.
- **Node limits below 2048 were never enforced.** `Limits::Nodes` was gated behind the same `& 2047`
  mask as the clock (reasonable — `aggregate()` sums at least 512 shards), but with a limit under
  2048 the mask never fires before the limit passes. Now also gated on the cheap thread-local count.

#### Robustness

- **Dividing by a tunable parameter could crash the engine.** Four sites divide by an SPSA parameter
  (`probcut_score_div`, `qs_see_div`, `corr_weight_div`, `conthist_div`), and `set_parameter`
  accepted any value with no range check — so a tuner bug or a hand-typed
  `setoption name corr_weight_div value 0` was a division-by-zero panic mid-match. `spsa.config`
  bounds only help if the tuner respects them. All four now clamp with `.max(1)` where the invariant
  matters, as does the `root_delta` division in LMR. In the default build these are `const fn`, so
  the clamp folds at compile time — zero cost in the binary that plays games.
- **The SPSA setter turned a typo into a forfeit.** `value.parse().unwrap()` panicked on a malformed
  value and `panic!` on an unrecognised name. A tuning run is a long-lived match; neither may take
  the process down, and UCI requires unknown options be ignored.
- **Pondering deadlocked when the *opponent* lost on time.** A ponder search never terminates on its
  own — `soft_limit` and `check_time` both return false outright while `ponder` is set, so it runs to
  `MAX_PLY` and ends only when something clears the flag. The reader thread dropped any command
  arriving while `status == RUNNING`, per the spec's "ignore unexpected commands" rule.

  Those two are fine separately and fatal together. When *we* are the one being waited for, the GUI
  sends `stop` before moving on. When the **opponent** flags or disconnects, the game is simply over
  from the harness's point of view — it was never waiting on our move, so it has no reason to send
  `stop`, and goes straight to `ucinewgame`/`position`/`go` for the next game. Every one of those was
  dropped, `go()` stayed blocked in its ponder-wait loop, and so was everything after them.

  It surfaced as a *disconnect* rather than a hang because `isready` is answered directly from the
  reader thread: the GUI saw a live, responsive engine that then never replied to `position` or `go`,
  waited for a `bestmove` that could not come, and timed out. Commands meaning the game moved on now
  end a ponder search first. Scoped to pondering only — a real search has a result someone is waiting
  for, and the silent-ignore rule still applies to it.

  **Ending the search is only half of it.** `go()` would then fall through and print the `bestmove`
  for the position it had been pondering. UCI requires a `bestmove` after `stop`, and *only* after
  `stop` — a GUI that sent `position`/`go` instead is not waiting for one, and reads the next line
  the engine prints as the answer to the `go` it just sent. That answer is a move for the abandoned
  ponder position: illegal in the new one roughly half the time, and silently wrong the rest.

  Worse, it desynchronises the stream by exactly one `bestmove` for the remainder of the game — every
  later reply answers the previous `go`, so the engine is permanently one move behind and the GUI
  eventually waits on a reply that was already consumed. **That is the same root cause behind both
  reported symptoms, the illegal-move/disconnect and the loss on time.** A `ponder_abandoned` flag is
  now set before the search is released (so `go()` cannot slip past between the two stores) and
  checked before anything is printed.
- **A panicking worker thread hung the engine.** The completion signal was skipped, so
  `ReceiverHandle::join()` blocked forever with no output. It now fires via `catch_unwind` before the
  panic is re-raised, turning a silent freeze into a visible crash.
- **FEN parsing accepted malformed input** — no rank/file bounds checks, no requirement of exactly
  one king per side.
- **`ttMoveHistory`'s gravity update was missing its bonus clamp**, letting the multicut caller push
  the value past its documented ±8192 bound at high depth. Every other gravity update clamps first.
- Lower-severity hygiene from the same audit: `CorrectionHistory::update` clamps its own bonus; the
  LMR `reduced_depth` PV bonus is applied *after* its clamp, so PV scout depth keeps the
  `new_depth + 4` ceiling; Chess960 castling no longer records the friendly rook in `state.captured`;
  the `Zobrist` table's `transmute` is backed by `#[repr(C)]` rather than Rust's unguaranteed field
  ordering; a dead `append_evasions` wrapper was removed.

### Performance

#### Behaviour-preserving

A `bench` after any of these must report an **identical node count**. A difference means something
changed semantically and needs investigating.

- **NNUE network and accumulator stacks on 2 MB pages** — the only change here with a measurement.
  See [Memory architecture](#memory-architecture): 2,826,642 nodes at 779,649 → 828,153 nps, **+6.2%**
  at identical nodes.
- **PEXT bitboards** — sliding-piece attacks indexed with the BMI2 `pext` instruction where
  supported, with classical magic multiplication as fallback. Disabled automatically on AMD Zen 1/2,
  where `pext` is microcoded and slower than the fallback.
- **Hoisted mailbox reads in move scoring.** Continuation history was read through a per-lag helper
  that resolved `piece_on(mv.from())` internally, so six lag lookups meant six reads of the same
  square — loop-invariant, but the read through a raw pointer inside `get` stops the optimizer
  hoisting it. In `score_quiet` this cut **nine** reads of `mv.from()` per quiet move to one.
- **Hoisted continuation-history subtable pointers.** The six `stack[ply - n].conthist` pointers
  depend only on `ply`, so they are constant for a whole node — but the per-lag helper re-read them
  on every call, making six strided loads into a large array *per move* where six per node suffice.
  The same change hoists the low-ply term's bound test and its `1024 * (1 + 2 * ply)` divisor;
  operands and their order are unchanged, so the arithmetic is bit-for-bit identical.
- **Deferred pruning terms behind their guards.** Qsearch delta pruning computed a board probe and a
  `PieceType::value` match for *every* move, including all check evasions — where the prune is
  disabled outright by `!in_check`, and where move lists are widest. `&&` short-circuits, so the
  conditions are unchanged.
- **Hoisted redundant board reads in `update_threats`** — `occupancies()` three times to one, and the
  opponent's pawn/knight/queen bitboards twice each to one.
- **Search stack reuse** — the per-ply `Stack` was reallocated from scratch on every aspiration retry
  and iterative-deepening iteration; it is now reset in place.
- **Unchecked hot-path indexing** — the per-ply search stack and ply-indexed arrays use
  `get_unchecked` behind `debug_assert!` bounds checks. This trades a bounds-check panic for
  undefined behaviour in release if an invariant is violated; debug builds and the test suite are the
  safety net.

#### Behaviour-affecting

**These change node counts.** A `bench` diff is expected, not a bug.

- **64-byte transposition table clusters** — associativity 3 → 6 at identical memory traffic. See
  [Memory architecture](#memory-architecture). Replacement decisions change, so bench nodes change.
  Untested by SPRT.

### Unverified search changes

Implemented and correct as far as perft, bench, the test suite and game records show — but **none has
been demonstrated to gain Elo**. Every one is a candidate for an A/B run, and a negative result on any
would be useful information.

**Evaluation correction:**

- Material correction history, restored: a 6th term in `eval_correction()`'s blend, keyed on
  `material_key()` — piece counts only, no square information, so a cramped middlegame and an open
  endgame with the same material land in the same bucket. `corr_weight_div` is rescaled to `76`
  (`64 * 6 / 5`) to match — the first time this table existed it was added on top of the unrescaled
  5-term divisor, silently inflating every RFP/FP/LMR/NMP margin that reads `correction_value.abs()`,
  which is why it was pulled entirely rather than patched. `Clear Hash`/`ucinewgame` now clears this
  table specifically.
- `correction_value` feeds razoring, RFP, both singular margins, futility pruning, LMR, FDS, qsearch
  SEE, and — through `eval` — null move, stand-pat, improving, opponent-worsening, LMP and BNFP, in
  both search and qsearch. **Any further change to this blend should be tested in isolation from
  everything else on this list.**

**Move ordering:**

- Low-ply history: a root-relative `[ply][from][to]` table for plies 0–4, shifted by two between
  searches. Its contribution decays as `1 / (1 + 2·ply)`.
- Six continuation-history lags rather than upstream's four, with per-lag weights and a
  positive-consistency multiplier. The quiet-scoring weights sum to the same total (4817) as the
  four-lag set they replaced.
- **Good/bad quiet split.** Quiets at or below `good_quiet_threshold` are deferred to a sixth
  `BadQuiet` stage, after bad captures. Upstream has five stages and searches every quiet before any
  bad noisy; Stockfish does not split quiets this way either. Entirely fork-only.
- TT-move reliability statistic (`ttMoveHistory`), a gravity-updated record of how often the TT move
  proves best, feeding the singular double-extension margin.

**Pruning and extensions:**

- **Threat density as a contextual pruning signal.** The search had exactly one position-type signal
  feeding a pruning margin, and it was a boolean:
  `rfp_no_threats * (all_threats & our_pieces).is_empty()`. A position with one loose knight and one
  with the queen, both rooks and a bishop hanging are both merely "not empty", and were treated
  identically. Replaced with a capped popcount of our own attacked pieces, widening the RFP and
  futility margins (pruning *less*) as more of our material comes under attack — the case where a
  static eval is most likely to be refuted by a capture the search has not seen, and where a "quiet"
  move may be the one that saves the piece. The old boolean is kept and cannot fight the new term: it
  fires only at density zero, the new term only above it, so they are mutually exclusive by
  construction. Cost is one AND and one popcount per node.
- **ProbCut acceptance history.** ProbCut is speculative execution: the qsearch draft proposes and a
  shallow search verifies. Nothing recorded how often the verification agreed. A gravity-bounded
  counter now does, and feeds ProbCut's own threshold. Only moves where a verification search
  actually ran are scored — if the draft never cleared the bar there is nothing to have agreed with,
  and counting those would measure the draft against itself.
- **Singular double/triple extension margins, clamped at 0.** The fork adds three terms upstream does
  not have: `-1175 * tt_move_history / 114178` (range ±84) and `-38` / `-43` when `ply > root_depth`.
  The test is `singular_score < singular_beta - margin`, evaluated inside a branch where
  `singular_score < singular_beta` already holds, so **once the margin goes negative the test is
  unconditionally true** and every singular extension becomes a double, then triple, extension.
  Upstream reaches that region too, so this is not fork-introduced — but these terms could push the
  floor about 8× deeper into it, and extensions multiply nodes. Both margins are now `.max(0)`'d.
- **Double-extension counter gating.** A per-ply count of accumulated double extensions, propagated
  down the first move only, above which further double/triple extensions are refused.
- Qsearch delta pruning — standard, but upstream does not have it at all, so every move it prunes is
  one the baseline searches.
- Recapture extension — a capture landing where the opponent's last move captured, that doesn't lose
  material itself, gets a full ply. Bounded by construction: applies only when `new_depth == 0` and
  sets it to exactly 1, so it fires at the frontier and cannot chain.
- **TT-only ProbCut** — a lower-bound TT entry from a near-full-depth search, comfortably above beta,
  trusted as a cutoff without any search. **Treat this as the highest-severity item on the list:**
  everything else here mis-orders or over-prunes, but this one can put a fabricated score into the
  search result.
- SEE pruning thresholds respond to `cutoff_count`, extending a signal already used by
  `lmr_cutoff`/`fds_cutoff`.
- Shuffling guard — repetitive piece shuffling near the fifty-move rule disables singular extensions
  (Stockfish #6447).
- Opponent-worsening term in RFP; "improving" also counts a node whose static eval already clears
  beta; the improving fallback chain extends to ply 6 for long same-side gaps.
- **Correction history updated on null-move fail-highs, moved to fire only once confirmed.** At
  `depth >= 16` a null-move fail-high is checked with a verification search before being trusted —
  but the correction-history update previously ran *before* that verification, so a value from a
  since-rejected fail-high could land in the table. A sub-search result that hasn't been confirmed
  isn't comparable to the genuine `(full search result − static eval)` samples this table is built on.

**Structure, time and parallelism:**

- **Root fail-lows extend the search.** PV stability, eval stability and best-move changes all
  describe how the answer is *moving*; none says the answer just got *worse*. A root fail-low means
  the move being played for is worse than the last iteration promised and there is no replacement yet
  — arguably the most valuable moment to keep thinking. Counted per iteration and fed into the time
  multiplier, capped at two because repeated fail-lows at one depth are the window widening, not new
  evidence.
- **A proven short mate stops the search.** Nothing did this outside `go mate`. The ordinary damping
  signals barely notice a mate: the node fraction collapses to ~0.66 and `score_trend` pins to its
  0.7214 floor, but `pv_stability`, `eval_stability` (which *resets* on the eval jump) and best-move
  stability push back, leaving **~0.83× of a full allocation** spent on a proven mate in 1. Any forced
  mate wins, so a shorter one found two iterations later is worth nothing on the clock. Guarded to
  wins only (being mated is exactly when to keep looking for a defence), true mate scores rather than
  TB wins, exact scores rather than bounds, single-PV, and **thread 0 only** — there is no
  best-thread vote, so a helper stopping everyone could leave the reporting thread emitting a
  non-mating move.
- **Single legal move stops early.** Runs to depth 8 — enough for a ponder move and a sane score —
  then banks the clock. It does shorten the ponder line, which is why it is listed here rather than
  as a fix.
- **Lazy SMP depth differentiation.** Helper threads previously ran an identical `1..MAX_PLY`, with
  the only divergence coming from `td.id`-seeded jitter on the LMR reduction. Helpers now walk
  distinct depth subsequences via a skip-size/skip-phase schedule, so a wider spread of depths and
  bounds reaches the shared TT — which is the entire mechanism by which Lazy SMP gains anything.

  Two details matter. Every `(size, phase)` pair is distinct with `phase < size`: Stockfish's
  historical table contains pairs where `phase >= size`, which alias onto `phase % size`, so two
  threads draw an identical subsequence and duplicate each other outright — exactly what the schedule
  exists to prevent. And the skip path **votes before continuing**: the soft stop needs a 65%
  majority cast at iteration boundaries, so an early version that `continue`d past the whole loop
  body left helpers unable to cast *or retract* a vote, stalling the majority and letting the hard
  bound decide the move. Skip sizes are capped at 3 regardless, since a size-6 helper jumping depth
  20 → 26 does ~64× the work of one iteration at EBF 2. `lazy_smp_skip` retires the schedule.
- **Internal Iterative Reductions** — PV and expected-cut nodes without a TT move are reduced a ply
  from depth 6, exempting nodes on the previous iteration's principal variation. Upstream has no IIR.
  Note the `follow_pv` exemption did not work until the `previous_pv` fix — `pv_table` slot 0 was
  never written, so `follow_pv` was always false and IIR applied at every eligible node. **Its current
  behaviour is newer than its Elo evidence.**
- **Post-LMR reduction gated on the position not having improved.** The parent-reduction half is
  upstream's; the `!opponent_worsening` half follows
  [PlentyChess](https://github.com/Yoshie2000/PlentyChess). Reducing further because the parent was
  reduced is better evidence when the position has not turned our way. Free: the signal was already
  in scope.
- Correction values computed before the TT probe, overlapping the work with the prefetch.
- Two-horizon falling-eval scaling — the trend factor also compares against the best score from four
  iterations ago.

### Protocol and usability

No effect on playing strength.

- **Pondering** — `go ponder` / `ponderhit` support and `bestmove ... ponder ...` output.
- **`searchmoves`** — root move filtering on the `go` command.
- **`UCI_ShowWDL`** — win/draw/loss estimates in `info` lines.
- **`SyzygyProbeDepth` / `SyzygyProbeLimit`** — user-tunable tablebase engagement.
- **SPSA tunables** — **148** search constants exposed as UCI options under the `spsa` cargo feature,
  with matching bounds in [`spsa.config`](spsa.config).
- Robustness: EOF handling, spin options clamped to their declared ranges, `movestogo 0` guarded
  against division by zero, `position startpos` honouring `UCI_Chess960`.

### Removed, and why

- **Classical (hand-crafted) evaluation terms** — pawn structure, bishop pair, rook placement.
  Superseded by the NNUE.
- **Killer moves and countermoves** were not reintroduced. Both duplicate what continuation history
  already encodes.
- **Classic Internal Iterative Deepening** was not added; superseded by IIR.
- **Minor-piece and major-piece correction-history tables.** Added alongside the material table at
  full strength on top of a five-term blend without adjusting the shared divisor, silently inflating
  every RFP/FP/LMR/NMP margin that reads `eval_correction()`. All three were pulled and the blend
  returned to upstream's terms while the divisor bug was isolated; material has since been
  reintroduced with the divisor rescaled. Reintroducing minor or major means recomputing the divisor
  again, the same way, and testing it as its own isolated patch.
- **Depth-indexed history divisors** for late-move and futility pruning, replaced by a flat 1024. The
  table was fork-only and its per-depth values were never measured.
- **History pruning for bad-SEE noisy moves** — removed because it could **never fire**. It and Bad
  Noisy Futility Pruning gated on the same conditions, BNFP ran first in the move loop, and BNFP's
  offset over that depth range (+82 to +258) sits far below the noisy check's (at least +633 for even
  a pawn). BNFP pruned a strict superset and then called `skip_bad_noisy()`, abandoning the rest of
  the pool — the window where the noisy check could fire was empty by 375+ centipawns at every depth,
  not marginally. It cost a board probe and a `PieceType::value` match per bad capture to reach an
  unreachable branch.
- **SEE-pruning and history-pruning exemptions for checking moves.** Extended by analogy with LMP/FP,
  but the analogy does not carry: those decline to prune a narrow, heavily qualified slice of quiet
  moves, whereas SEE pruning is the main filter for losing captures and `is_direct_check` is broad.
- **Qsearch checks, shared continuation history, a second check extension, a correction-history update
  on singular multicut, `razor_worsening`, `nmp_r_tt_history`, and per-search history decay** were
  each tried and removed.

**The lesson from all of the above:** every measurable Elo recovery in this fork's history came from
fixing arithmetic, not from adding ideas. When a change underperforms, check the units before
questioning the heuristic — a mis-scaled term looks tuned, because SPSA will report a value for it,
but a saturated or unnormalized coefficient has no gradient and whatever number it lands on is
arbitrary.

## Testing and tuning

Nothing in [Unverified search changes](#unverified-search-changes) should be trusted until it passes
game testing. Build both sides identically — same toolchain, both PGO or neither — since a build
asymmetry is worth several Elo on its own.

**Start with `bench`.** It is deterministic, needs no harness, and settled a question that thousands
of games could not (see [Status](#status)). For anything that should be behaviour-neutral, a
node-identical bench is stronger evidence than any SPRT of comparable cost.

### SPRT (does this patch gain Elo?)

```bash
fastchess \
  -engine cmd=./wreckless      name=Test \
  -engine cmd=./wreckless-base name=Base \
  -each tc=10+0.1 option.Threads=1 option.Hash=128 proto=uci \
  -openings file=UHO_Lichess_4852_v1.epd format=epd order=sequential \
  -sprt elo0=0 elo1=5 alpha=0.05 beta=0.05 model=normalized \
  -rounds 30000 -repeat -concurrency 8 -recover \
  -pgnout file=games.pgn -ratinginterval 20
```

**Set `-concurrency` to your physical core count, not your logical one.** At 10+0.1, saturating every
logical core leaves nothing for the harness and makes per-move timing erratic, which shows up as
spurious losses on time.

**Halve that again if both engines ponder.** Without pondering, only one engine per game is searching
at a time, so `concurrency` games cost `concurrency` busy threads. With `Ponder` enabled on both
sides there is no idle gap — one engine thinks while the other ponders, continuously — so the same
setting costs **twice** the threads. On an 8-core machine, `-concurrency 4` with both engines
pondering is 8 permanently busy threads plus the harness, and the resulting jitter produces losses on
time that have nothing to do with either engine's time management.

Pondering also makes every game's node counts irreproducible, so most SPRT testing here is done with
it off. Turn it on to test pondering itself, not as a default.

**Choose bounds that match the question.** `elo0=0 elo1=5` asks "does this gain at least 5 nElo" and
is right for a change meant to add strength. For a correctness fix or a simplification, use
`elo0=-5 elo1=0` — "does this lose anything" — which resolves far sooner. A test whose true effect
sits on one of its bounds can run indefinitely without reaching either.

**Test one patch per run.** Bundling several changes makes a negative result uninterpretable.

**Use a fresh PGN filename per run.** Appending to a fixed one silently mixes builds, and a PGN that
mixes builds will produce a confident, meaningless Elo figure.

**Optional adjudication.** Playing games out keeps conversion and endgame technique inside the
measurement; adjudicating roughly halves the wall-clock cost. These thresholds were derived by
replaying this engine pair's own game records and checking every verdict against the real result:

```bash
  -resign movecount=3 score=400 twosided=true \
  -draw   movenumber=40 movecount=8 score=5
```

That combination adjudicated 1494 of 1612 games and saved 32% of all plies with **zero** incorrect
verdicts. Raising the resign threshold *reduces* savings sharply (600cp saves only 6%, because a
larger margin only triggers deep into an already-decided conversion); lowering it past 400 starts
misclassifying won games. These figures are specific to this engine pair and evaluation scale.

### SPSA (what should the tunable constants be?)

```bash
cargo rustc --release --bin wreckless --features spsa
```

Feed [`spsa.config`](spsa.config) to an [OpenBench](https://github.com/AndyGrant/OpenBench) SPSA test
— SPSA needs many games, and OpenBench distributes them. Paste results into `src/parameters.rs` and
confirm with a normal SPRT before keeping them; SPSA on too few games converges to noise.

Always benchmark and play with the default (non-`spsa`) build. The `spsa` feature reads every
parameter through a mutable static instead of a constant, which is measurably slower.

Four cautions specific to this codebase:

1. **A parameter that saturates its clamp gives SPSA no gradient**, so it returns an arbitrary value
   with confident-looking precision. Check the operating range in pawns before trusting a tuned
   constant. `tm_trend_diff` is the worked example.
2. **Check the SPSA bounds can reach the answer.** `tm_trend_diff`'s floor was 4.7× above the value
   the code's own comment identified as correct — tuning was locked inside the regime it was meant to
   escape.
3. **Some parameters are deliberately coupled.** The conthist weights sum to a fixed total, enforced
   by a `const` assertion. Moving one without the other reintroduces a scale bug this fork has already
   paid for once.
4. **A tuned constant is only meaningful if the code that reads it can fire.** Before tuning a pruning
   margin, check that an earlier prune in the same move loop doesn't already cut a superset of what it
   would. That is how the noisy history prune turned out to be unreachable.

Fork-invented parameters carry a lower bound of 0 in `spsa.config` specifically so tuning can retire
them.

## Acknowledgements

- [Reckless](https://github.com/codedeliveryservice/Reckless) and its
  [contributors](https://github.com/codedeliveryservice/Reckless/graphs/contributors) — Wreckless is
  a fork and inherits effectively all of its strength from their work, including the NNUE networks
  from [RecklessNetworks](https://github.com/codedeliveryservice/RecklessNetworks)
- [OpenBench](https://github.com/AndyGrant/OpenBench) and
  [fastchess](https://github.com/Disservin/fastchess), the testing frameworks
- [Bullet](https://github.com/jw1912/bullet), the NNUE trainer
- [Stockfish](https://github.com/official-stockfish/Stockfish),
  [PlentyChess](https://github.com/Yoshie2000/PlentyChess),
  [Ethereal](https://github.com/AndyGrant/Ethereal), [Berserk](https://github.com/jhonnold/berserk),
  and many other open source chess engines
- [Chess Programming Wiki](https://www.chessprogramming.org/Main_Page)

## License

This project is licensed under the [GNU Affero General Public License v3.0](LICENSE).
