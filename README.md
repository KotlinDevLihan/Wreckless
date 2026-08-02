<div align="center">
  <h1>Wreckless Chess Engine</h1>

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
</div>

Wreckless is a UCI chess engine, a fork of [Reckless](https://github.com/codedeliveryservice/Reckless)
by Lihan van der Westhuizen — an open source competitive engine that consistently
performs among the top engines in major tournaments including the
[Chess.com Computer Chess Championship (CCC)][ccc] and [Top Chess Engine Championship (TCEC)][tcec].

Wreckless inherits effectively all of its playing strength from Reckless, including its NNUE
networks. It exists as a place to try search ideas against that baseline. See
[Status](#status) for where it currently measures, and
[Changes relative to upstream](#changes-relative-to-upstream) for what differs and how much
evidence each change has behind it.

[ccc]: https://www.chess.com/computer-chess-championship
[tcec]: https://tcec-chess.com

## Contents

- [Status](#status)
- [Quick start](#quick-start)
- [Building from source](#building-from-source)
- [UCI options](#uci-options)
- [Custom commands](#custom-commands)
- [Changes relative to upstream](#changes-relative-to-upstream)
- [Testing and tuning](#testing-and-tuning)
- [Acknowledgements](#acknowledgements)

## Status

**Wreckless measures at or above parity with upstream Reckless, and the size of the margin is not
settled.** SPRT runs at 10+0.1, 1 thread, 128 MB hash have landed anywhere from roughly −7 to +3
Elo with 95% intervals of ±10 to ±20, while a 553-game run against the same upstream build under a
different tournament manager put a release build **+63 Elo** ahead. Those cannot both be right about
the same code, and the discrepancy has been traced to harness differences rather than to the engine.

Two lessons from that are worth carrying:

- **Check what the binary reports, not what you think you built.** A UCI `id name` line carries the
  version and commit hash. More than one conclusion here has been drawn from a run whose binary was
  not the code being discussed.
- **Paired openings are not optional.** The +63 run assigned each opening exactly once, so its
  variance is far wider than a naive binomial interval suggests — colour-swapped pairs are what make
  the pentanomial (and the interval it produces) meaningful.

What is not in doubt is the direction of travel. Earlier states of this fork measured as much as
17 Elo *behind* upstream, and the work that
closed that gap was almost entirely arithmetic and correctness repair rather than new ideas: unit
mismatches between the evaluation scale and `PieceType::value()`, coefficients ported from another
engine without rescaling, a fail-soft bound that was never raised, sums whose consumers were tuned
against a narrower distribution. The
[Correctness and scale fixes](#correctness-and-scale-fixes) section is the substantive part of this
document.

No search addition in this fork has been demonstrated to gain Elo over upstream. Several are
plausible and none is currently known to lose; they are listed under
[Unverified search changes](#unverified-search-changes) and should be treated as experiments.

Two of them are worth singling out, because they are the cases where a fork addition and an inherited
upstream mechanism act on the *same* signal:

- **A missing TT move** was penalised by both IIR and the LMR/FDS reduction bonus, at upstream's
  coefficient. Corrected — see [Correctness and scale fixes](#correctness-and-scale-fixes).
- **A bad-SEE capture at low depth** was tested by both Bad Noisy Futility Pruning and a fork-only
  noisy history prune, where BNFP always won. The noisy prune was unreachable and is
  [removed](#removed-and-why).

Both were invisible to per-feature review and to profiling, and neither would ever have shown up in
SPSA — an unreachable branch has no gradient, and a double-applied one looks like a well-tuned single
term. When adding a heuristic here, the question to ask is not only "is this sound" but "does
something else already act on this signal, and was that thing tuned before or after."

## Quick start

Wreckless is not a standalone chess program — it's an engine you plug into a UCI-compatible GUI, such as
[Cute Chess](https://github.com/cutechess/cutechess), [En Croissant](https://encroissant.org), or
[Nibbler](https://github.com/rooklift/nibbler). Build it, then point your GUI at the resulting binary:

```bash
make
# produces ./wreckless (or wreckless.exe on Windows)
```

Full build options, including Syzygy tablebase and PGO builds, are covered below.

## Building from source

**Requirements:**

- Rust 1.88.0 or later ([install guide](https://www.rust-lang.org/tools/install))
- Clang (only needed for Syzygy tablebase support, via the bundled [Fathom](https://github.com/jdart1/Fathom) library)

**Standard build** (with Syzygy support):

```bash
make
# ./wreckless
```

**Without Syzygy** (skips the Clang dependency):

```bash
make no-syzygy
# ./wreckless
```

### Profile-guided optimization (PGO) builds

PGO builds run a profiling pass before the final compile, typically worth a small but real NPS gain.
Use these for anything performance-sensitive (rated games, benchmarking) — the standard build is fine
for development.

One-time setup:

```bash
rustup component add llvm-tools
cargo install cargo-pgo
```

Then either:

```bash
make pgo
# ./wreckless
```

or run the three steps manually if you want more control:

```bash
cargo pgo instrument build --release --bin wreckless   # 1. build an instrumented binary
cargo pgo run -- bench                                  # 2. profile it against the bench suite
cargo pgo optimize build --release --bin wreckless      # 3. rebuild using the collected profile
# binary lands under target/<your-target-triple>/release/wreckless
```

> **When benchmarking, measure the optimized binary.** `cargo pgo run -- bench` executes the
> *instrumented* build, which carries profiling counters on every branch. Its NPS figure is not
> comparable to a normal build's.

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

Beyond standard UCI, Wreckless supports commands useful for testing and debugging:

| Command | Description |
| --- | --- |
| `perft <depth>` | Count leaf nodes at a given depth ([perft][perft]) |
| `bench` | Run the fixed [benchmark][bench] suite to measure performance |
| `d` | Print the current position as a board diagram plus FEN |
| `eval` | Print the NNUE evaluation of the current position, White's perspective |
| `compiler` | Print the compiler version, target, and flags used to build the binary |
| `speedtest <Threads> <Hash> <Seconds>` | Run a timed performance test across 50 positions |

[perft]: https://www.chessprogramming.org/Perft
[bench]: /src/tools/bench.rs

## Changes relative to upstream

Grouped by what is actually known about each change:

- **[Correctness and scale fixes](#correctness-and-scale-fixes)** — defects with a demonstrable
  wrong answer, independent of whether any feature helps or hurts. These are the changes worth
  trusting.
- **[Speed](#speed)** — behaviour-preserving. Verify with a node-identical `bench`.
- **[Unverified search changes](#unverified-search-changes)** — implemented, correct as far as
  testing shows, but never demonstrated to gain Elo. Experiments.
- **[Protocol and usability](#protocol-and-usability)** — no effect on playing strength.
- **[Removed, and why](#removed-and-why)** — worth reading before trusting anything above it.

### Correctness and scale fixes

The recurring defect in this codebase has been **scale drift**: a heuristic is ported or a term is
added to a sum, and the coefficients that consume it are never rescaled. Two unit systems collide
constantly, and mixing them is silent:

| quantity | a pawn is worth |
| --- | --- |
| evaluation / search scores | ~321 (startpos) to ~382 (middlegame), per `normalization()` |
| `PieceType::value()` | 109 |
| Stockfish `Value` (when porting constants) | ~208 material, ~328 eval-normalized |

Before judging any margin in this engine, convert it to pawns. A threshold that saturates its clamp
within a few hundredths of a pawn is a bug, not a tuning choice.

**Fixed:**

- **Qsearch delta pruning violated fail-soft.** A pruned capture's optimistic bound
  (`eval + material_gain + margin`) is strictly above the standing pat that seeds `best_score`, but
  the prune skipped the move without raising it. Qsearch therefore returned an upper bound *lower*
  than the one it had proven, stored it as `Bound::Upper`, and the parent negated it into a score
  *higher* than justified — capped by the margin, so it surfaced as many small overestimates rather
  than few large ones. Upstream raises `best_score` at both of its own futility prunes; this block
  was the sole violator of a convention the fork inherited. Measured effect: the fork had been
  taking 22% more half-pawn evaluation collapses than upstream (z = +11.0); after the fix the two
  are level.
- **Qsearch delta pruning mixed unit systems.** The captured piece was credited with a raw
  `PieceType::value()` against an `eval`-scaled margin, understating every capture roughly
  threefold. Now converted (`qs_delta_piece_scale`, 192/64 ≈ 3.0, against a true ratio of ~2.94).
- **The time manager's falling-eval factor is a two-level switch — diagnosed, not fixed.** The
  linear structure is Stockfish's and the coefficient ratio was preserved, but the constants were
  carried across unscaled from a scale where a pawn is ~208 to one where it is ~321–382. With
  `tm_trend_base 7426`, `tm_trend_diff 480` and the `[7214, 14031]` clamp, the ceiling is cleared
  after `(14031-7426)/480` = **13.8 units, or 0.043 pawns**, and the floor sits under any gain at
  all. It is bang-bang with essentially no proportional band, and `tm_trend_recent` stacks a second
  saturating term on an already-saturated one — which is also why SPSA has never found signal here.
  There is no gradient inside the clamp.

  An earlier revision rescaled these so the ceiling sat ~0.37 pawns out, matching Stockfish's band in
  pawn terms. **That was reverted** in favour of upstream's untouched values, on the grounds that
  those are the measured ones and a bad constant in a time-management path costs games rather than
  nodes. This README went on describing the rescale for some time after it no longer existed; the
  shipped behaviour is the 0.043-pawn switch described above.

  What has changed is only that the values live in `parameters.rs` as fixed point, and that their
  SPSA bounds were widened (`tm_trend_diff` floor 240 → 40, `tm_trend_recent` 115 → 20). The
  proportional band would need roughly `diff 51 / recent 25`, and the old floors were 4.7x too high
  to reach it — tuning was locked inside the saturated regime it was supposed to escape. Defaults
  are unchanged; this makes the region reachable, it does not assume it is better.
- **Continuation-history updates used a running prefix count.** `positive_count` was incremented and
  used to index the consistency multipliers within the same loop iteration, so lag 1 could only ever
  reach `multipliers[1]` while lag 6 could reach `multipliers[6]`. The nearest lags — the ones move
  ordering leans on hardest — were damped (94–103) against the distant ones (121–126) purely by
  position in the loop. Now counted across all eligible lags before any bonus is applied.
- **Continuation-history pointers broke Rust's aliasing rules.** `subtable_ptr` materialised a `&mut`
  reference and coerced it to a raw pointer, which the search then holds on its stack across many
  later borrows of the same table; `update` took `&mut self` while writing *through* that pointer,
  handing LLVM a `noalias` promise its own argument immediately breaks. Restored upstream's form
  (`&raw mut` and a `&self` receiver) for both `ContinuationHistory` and
  `ContinuationCorrectionHistory`.
- **Aspiration fail-low widened the re-search instead of narrowing it.** Collapsing `beta` to the
  old `alpha` made the window 1.5× wider on a shallow fail-low and 3× wider on a deep one — the
  opposite of the rationale the code carried. Now re-centres on the failing score, so every
  re-search is exactly `delta` wide, matching upstream.
- **The `history` sum silently rescaled seven tuned coefficients.** It feeds `lmp_history`,
  `fp_history`, `bnfp_history`, `hp_margin`, `see_q_hist`, `see_n_hist` and
  `lmr_quiet_hist`/`fds_quiet_hist`, all tuned against upstream's `quiet_history + conthist(1) +
  conthist(2)`. Lags added beyond that widened the sum without anyone adjusting the consumers. All
  six lags are now read at the same relative strengths the update writes them, then normalized back
  onto upstream's two units.
- **Null-move zugzwang guard tested the wrong quantity.** `board.material()` includes pawns, so a
  pawn-heavy, piece-empty endgame — precisely the zugzwang case the guard exists for — could clear
  the threshold and be null-move pruned anyway. Now uses `non_pawn_material()`.
- **Node limits below 2048 were never enforced.** `Limits::Nodes` was gated behind the same
  `& 2047` mask as the clock (reasonable — `aggregate()` sums at least 512 shards), but with a limit
  under 2048 the mask never fires before the limit passes. Now also gated on the cheap thread-local
  count, keeping the optimization without the gap.
- **TT verification-key race under multithreading.** `Cluster.keys` packs three slots' keys into one
  `u64` via a non-atomic read-modify-write, called concurrently by every lazy-SMP thread. Two threads
  updating *different* slots could silently revert each other — corrupting a sibling's key, not just
  the writer's own entry. Now an `AtomicU64` updated with `fetch_update`.
- **`TtDepth::NONE` was unreachable.** The depth encoding gave a real `SOME`-depth write and a
  never-written, zero-initialized slot the same raw byte, so the replacement logic's "found a free
  slot" path could never fire. Re-encoded (`NONE = -2` → `offset_depth = 0`) so an empty cluster is
  uniquely identifiable.
- **TT-only ProbCut trusted qsearch entries at shallow depth.** Its gate is
  `tt_depth >= depth - 4`, but `TtDepth::SOME` is `-1`, so at depth 3 or below that reduces to
  `tt_depth >= -1` — which a *qsearch* entry satisfies. The cutoff then returns a fabricated
  `beta + margin` on the strength of an entry that never came from a real search, contradicting the
  technique's own premise. The floor is now clamped at 0 (`(depth - 4).max(0)`), admitting only
  real-search entries; depths 4 and above are unaffected.
- **The continuation-history weight total is now enforced at compile time.** The six per-lag weights
  in quiet scoring must sum to upstream's four-lag total (4817), because `good_quiet_threshold` is
  calibrated against the resulting score distribution — redistributing weight between lags is safe,
  changing the total silently rescales the good/bad quiet split. That invariant was previously held
  by a comment; it is now `CONTHIST_WEIGHTS` plus a `const` assertion, so violating it fails the
  build. This is the same defect class as the `history` sum above, which silently rescaled seven
  tuned pruning coefficients at once.
- **Qsearch delta pruning ignored promotion value.** For a non-capture promotion, `type_on()` reads
  an empty square, crediting nothing for the ~1133cp actually gained. Now credited separately.
- **The soft/hard time split disabled itself in the middlegame.** `soft_limit` scales the soft bound
  by a multiplier that is the product of five independently-bounded factors — node fraction, PV
  stability, eval stability, score trend, best-move stability — with no joint cap. Its ceiling is
  ~8.4x with a perfectly stable best move and ~16–18x once the best move has changed a few times.
  The Fischer hard/soft ratio is `0.7281/soft_scale`, and `soft_scale` *rises* with move number: 28x
  at move 10, 19.9x at move 20, 16.6x at move 30, 13.3x at move 60. So from roughly move 20 onward
  an unstable position produces a scaled soft bound sitting past the hard bound, and the soft limit
  stops binding entirely — the hard bound decides the move, spending 72.8% of the remaining clock on
  it. Worse, the hard bound is polled mid-search rather than between iterations, so the cutoff lands
  part-way through an iteration and that partial work is discarded. The split silently switched
  itself off exactly when instability meant it mattered most. The scaled soft bound is now clamped to
  the hard bound, which keeps the stop on an iteration boundary at every move number and cannot make
  the engine think longer than it already would.
- **Cyclic (`movestogo`) hard bound could consume the whole clock.** With a small `movestogo`, five
  times the per-move allocation already exceeds what remains, collapsing the safety bound to
  "everything left" with another move still due. Now reserves one allocation.
- **A panicking worker thread hung the engine.** The completion signal was skipped, so
  `ReceiverHandle::join()` blocked forever with no output. It now fires via `catch_unwind` before the
  panic is re-raised, turning a silent freeze into a visible crash.
- **`ttMoveHistory`'s gravity update was missing its bonus clamp**, letting the multicut caller push
  the value past its documented ±8192 bound at high depth. Every other gravity update in the codebase
  clamps first.
- **FEN parsing accepted malformed input** — no rank/file bounds checks and no requirement of exactly
  one king per side.
- **The evaluation could come back sign-flipped.** The fiftymove damping factor is
  `(200 - clock) / 200`, and `clock` is a `u8` read straight from the FEN — so 201..=255 is
  representable and the multiplier goes negative. `is_draw` gates every node except the root, so the
  reachable case was a root FEN with a clock above 200 feeding a *negated* static eval into
  aspiration and every margin that reads it. Clamped. Unreachable from legal play (the rule ends the
  game at 100), so it is inert for normal games.
- **`go mate N` stopped when *being* mated.** The condition tested
  `Score::MATE - score.abs() <= moves * 2`, and the `.abs()` made a mate against us satisfy it just
  as well as one we had found -- so `go mate 3` in a lost position stopped and reported the mate
  being delivered *to* us as its answer. UCI asks for a mate in N, not for any decisive line within
  N; searching on is what already happens when no mate exists.
- **`go movetime` ignored Move Overhead.** Only the fixed 15 ms was subtracted, so a GUI on the
  default 100 ms overhead got `ms - 15` of thinking and could flag on a slow connection. Now
  `saturating_sub(move_overhead)`, matching the Fischer and Cyclic branches.
- **Dividing by a tunable parameter could crash the engine.** Four sites divide by an SPSA parameter
  (`probcut_score_div`, `qs_see_div`, `corr_weight_div`, `conthist_div`), and `set_parameter`
  accepted any value with no range check — so a tuner bug or a hand-typed
  `setoption name corr_weight_div value 0` was a division-by-zero panic mid-match. The `spsa.config`
  bounds only help if the tuner respects them. All four now clamp with `.max(1)` at the point the
  invariant matters, as does the `root_delta` division in LMR. In the default build these are
  `const fn`, so the clamp folds at compile time — zero cost in the binary that plays games.
- **The SPSA setter turned a typo into a forfeit.** `value.parse().unwrap()` panicked on a malformed
  value and `panic!` on an unrecognised name. A tuning run is a long-lived match; neither may take
  the process down, and UCI requires unknown options be ignored. Both now emit `info string` and
  continue.
- **A missing TT move was penalised twice.** Upstream reduces late moves by `2204` (LMR) / `2168`
  (FDS) when there is no TT move, tuned for a search that penalises that signal *once*. This fork
  also has Internal Iterative Reductions, which decrement depth by a full ply on the same signal — so
  at a cut node with no TT move at depth ≥ 6, both fired and late moves took roughly **3.15 plies of
  reduction where the coefficient assumes 2.15**, about 47% more. Cut nodes are most of the tree and
  a fresh node usually has no TT move, so this was not a corner case. The correction is exact rather
  than estimated: reductions are in 1/1024 plies (`reduced_depth = new_depth - reduction / 1024`), so
  IIR's ply is 1024 units, and `2204 − 1024 = 1180` / `2168 − 1024 = 1144` restores upstream's
  late-move treatment while leaving IIR's effect on the *first* move — the part it exists for —
  intact. Same defect class as the `history` sum and the conthist lags: a term added on top of a sum
  whose consumers were never rescaled.

  Stockfish confirms the result independently. It runs *both* mechanisms — `if (!ss->followPV &&
  !allNode && depth >= 6 && !ttData.move) depth--;` alongside `if (!ttData.move) r += 1127;` — so
  pairing IIR with an LMR bonus is correct; what matters is the bonus being sized for a search that
  *has* IIR. This fork's IIR is close to a transcription of Stockfish's condition, but it was added
  over Reckless's 2204, which predates it. The corrected 1180 lands within 5% of Stockfish's 1127,
  derived from the unit arithmetic rather than copied.
- **`LowPlyHistory` bounds lived in its callers.** `get`/`update` indexed `entries[ply]` directly
  while the search calls them from nodes at any ply up to `MAX_PLY` — correct only because all three
  call sites happened to test `ply < MAX_LOW_PLY`. The bound moved inside the type, so a fourth call
  site is inert rather than an out-of-bounds panic, and a `const` assertion pins the `>= 2` that
  `shift()`'s `rotate_left(2)` and `MAX_LOW_PLY - 2` slice assume.
- Lower-severity hygiene from the same audit: `CorrectionHistory::update` clamps its own bonus; the
  LMR `reduced_depth` PV bonus is applied *after* its clamp, so PV scout depth keeps the
  `new_depth + 4` ceiling instead of being capped two plies lower; Chess960 castling no longer
  records the friendly rook in `state.captured`; the `Zobrist` table's `transmute` is backed by
  `#[repr(C)]` rather than Rust's unguaranteed field ordering; a dead `append_evasions` wrapper was
  removed (nothing called it, and perft drives `generate_all_moves`, so it would have shipped
  unvalidated the moment anyone wired it up — its crash post-mortem now lives on `generate_moves`).

### Speed

Behaviour-preserving. A `bench` after any of these should report an **identical node count** —
a difference means something changed semantically and needs investigating.

- **PEXT bitboards** — sliding-piece attacks indexed with the BMI2 `pext` instruction where
  supported, with classical magic multiplication as fallback. Disabled automatically on AMD Zen 1/2,
  where `pext` is microcoded and slower than the fallback.
- **Windows large pages** — the transposition table, the continuation-history tables and the NNUE
  network use 2 MB pages where the OS grants the privilege.
- **The NNUE network on large pages.** The network was read straight out of the binary's `.rodata`
  via `include_bytes!` — zero-copy, which is the right instinct, but it leaves 60 MB of weights on
  4 KB pages: **15,446 page-table entries against an L2 TLB of roughly 2,048**. The feature
  transformer is indexed by feature number, so its accesses scatter across the whole table, and every
  accumulator update touches it. It is the hottest random-access structure in the engine and the one
  most punished by TLB misses, and it was the only large table not already using the large-page
  allocator the TT and history tables share. Copying it into a `HugeBox` at startup makes it **30
  entries**. Costs ~60 MB of RSS and one `memcpy`, and falls back to normal pages automatically when
  the privilege is unavailable.

  Measured on an 8C/16T Zen 3, PGO-instrumented builds, same bench: **2,826,642 nodes at 779,649 nps
  before, 2,826,642 nodes at 828,153 nps after** — node count identical to the unit, throughput
  **+6.2%**. Instrumented builds are noisy, so treat the magnitude as approximate; the node identity
  is what confirms the change is purely a memory-system one.
- **Hoisted mailbox reads in move scoring.** Continuation history was read through a per-lag helper
  that resolved `piece_on(mv.from())` internally, so six lag lookups meant six reads of the same
  square — loop-invariant, but the read through a raw pointer inside `get` stops the optimizer
  hoisting it. In `score_quiet` this cut **nine** reads of `mv.from()` per quiet move to one, since
  `type_on(sq)` is exactly `piece_on(sq).piece_type()` and `moved_piece(mv)` is `piece_on(mv.from())`.
- **Hoisted continuation-history subtable pointers.** The six `stack[ply - n].conthist` pointers
  depend only on `ply`, so they are constant for a whole node — but the per-lag helper re-read them
  on every call, making six strided loads into a large array *per move* where six per node suffice.
  Both consumers (the quiet `history` sum in `search`, and `score_quiet`) now resolve them once
  before their loop, and the helper is gone. The same commit hoists the low-ply term's bound test
  and its `1024 * (1 + 2 * ply)` divisor, which are likewise loop-invariant; the operands and their
  order are unchanged, so the arithmetic is bit-for-bit identical and only the branch moves.
- **Deferred pruning terms behind their guards.** Qsearch delta pruning computed a board probe and a
  `PieceType::value` match for *every* move, including all check evasions — where the prune is
  disabled outright by `!in_check`, and where the move lists are widest. The main search's noisy
  history-pruning value did the same on quiets, where `capture_sq()` holds nothing. Both now sit
  behind the cheap guards; `&&` short-circuits, so the conditions are unchanged.
- **Search stack reuse** — the per-ply `Stack` was reallocated from scratch on every aspiration retry
  and iterative-deepening iteration; it is now reset in place.
- **Unchecked hot-path indexing** — the per-ply search stack and ply-indexed arrays use
  `get_unchecked` behind `debug_assert!` bounds checks. Note this trades a bounds-check panic for
  undefined behaviour in release if an invariant is ever violated; debug builds and the test suite
  are the safety net.

### Unverified search changes

Implemented and correct as far as perft, bench, the test suite and game records show — but **none has
been demonstrated to gain Elo**. Every one is a candidate for an A/B run against upstream, and a
negative result on any of them would be useful information.

**Evaluation correction:**

- Material correction history, restored: a 6th term in `eval_correction()`'s blend, keyed on
  `material_key()` — piece counts only, no square information, so a cramped middlegame and an open
  endgame with the same material land in the same bucket. It's a weaker signal than the
  pawn/non-pawn tables upstream tuned, but it's summed in unweighted, at the same strength as the
  rest of the blend. `corr_weight_div` is rescaled to `76` (`64 * 6 / 5`) to match — the first time
  this table existed in this fork it was added on top of the unrescaled 5-term divisor, silently
  inflating every RFP/FP/LMR/NMP margin that reads `correction_value.abs()`, which is why it was
  pulled out entirely rather than patched (see [Removed](#removed-and-why)). This time the divisor
  moves with it. `Clear Hash`/`ucinewgame` now also clears this table specifically because that was
  missed on the first pass and would have carried stale values across games.
- `eval_correction()`'s `correction_value` feeds razoring, RFP, both singular margins, futility
  pruning, LMR, FDS, qsearch SEE, and — through `eval` — null move, stand-pat, improving,
  opponent-worsening, LMP and BNFP, in both search and qsearch. Any further change to this blend
  (the divisor, or reintroducing a per-term weight) should be tested in isolation from everything
  else on this list, for the same reason.

**Move ordering:**

- Low-ply history: a root-relative `[ply][from][to]` table for plies 0–4, shifted by two between
  searches. Its contribution decays as `1 / (1 + 2·ply)` and is comparable in magnitude to
  `quiet_history` at ply 0, falling below it by ply 1.
- Six continuation-history lags rather than upstream's four, with per-lag weights and a
  positive-consistency multiplier, near lags only when in check. The quiet-scoring weights sum to the
  same total (4817) as the four-lag set they replaced, so the score distribution — and therefore
  `good_quiet_threshold` — keeps its meaning.
- **Good/bad quiet split.** Quiets scoring at or below `good_quiet_threshold` are deferred to a sixth
  `BadQuiet` stage, after bad captures. Upstream has five stages and searches every quiet before any
  bad noisy; Stockfish does not split quiets this way either. Entirely fork-only.
- TT-move reliability statistic (`ttMoveHistory`), a gravity-updated record of how often the TT move
  proves best, feeding the singular double-extension margin.

**Pruning and extensions:**

- **Threat density as a contextual pruning signal.** The search had exactly one position-type signal
  feeding a pruning margin, and it was a boolean: `rfp_no_threats * (all_threats & our_pieces)
  .is_empty()`. A position with one loose knight and a position with the queen, both rooks and a
  bishop hanging are both merely "not empty", and were treated identically. Replaced with a capped
  popcount of our own attacked pieces, widening the RFP and futility margins (pruning *less*) as more
  of our material comes under attack — the case where a static eval is most likely to be refuted by a
  capture the search has not seen, and where a "quiet" move may be the one that saves the piece. The
  old boolean is kept and cannot fight the new term: it fires only at density zero, the new term only
  above it, so they are mutually exclusive by construction. Cost is one AND and one popcount per
  node; both bitboards are already materialised by `update_threats`.
- **ProbCut acceptance history.** ProbCut is speculative execution: the qsearch draft proposes and a
  shallow search verifies. Nothing recorded how often the verification actually agreed. A
  gravity-bounded counter now does, and feeds ProbCut's own threshold — raising the bar when
  verification keeps disagreeing, lowering it when it keeps agreeing. Only moves where a verification
  search actually ran are scored; if the draft never cleared the bar there is nothing to have agreed
  with, and counting those would measure the draft against itself.

- Qsearch delta pruning — a standard technique, but one upstream does not have at all, so every move
  it prunes is one the baseline searches.
- Recapture extension — a capture landing where the opponent's last move captured, that doesn't lose
  material itself, gets a full ply. Gated on square repetition and SEE, not on giving check. Bounded
  by construction: it only applies when `new_depth == 0`, and sets it to exactly 1, so it fires at
  the frontier and cannot chain.
- TT-only ProbCut — a lower-bound TT entry from a near-full-depth search, comfortably above beta, is
  trusted as a cutoff without any search. This is the only place in the search that returns a score
  nothing ever searched, so it is held to the same gating as its neighbours (non-PV, non-excluded,
  not decisive) and to a depth floor that excludes qsearch entries. Treat it as the highest-severity
  item on this list: everything else here mis-orders or over-prunes, but this one can put a
  fabricated score into the search result.
- **Singular double/triple extension margins, now clamped at 0.** The fork adds three terms
  upstream does not have: `-1175 * tt_move_history / 114178` (range ±84) and `-38` / `-43` when
  `ply > root_depth`. The extension test is `singular_score < singular_beta - margin`, evaluated
  inside the branch where `singular_score < singular_beta` already holds, so **once the margin goes
  negative the test is unconditionally true** and every singular extension becomes a double, then
  triple, extension. Upstream reaches that region too (its non-PV floor is about −16 − 16·|corr|/128),
  so this is not fork-introduced — but these three terms could push the floor to roughly −138, about
  8× deeper into it, and extensions multiply nodes. Both margins are now `.max(0)`'d, bounding the
  worst case to "extends at most as often as with the term absent" rather than "extends unboundedly
  more often as the term grows more negative." Still worth an isolated SPRT — the clamp changes the
  distribution of how often double/triple extensions fire, which is itself untested.
- SEE pruning thresholds respond to `cutoff_count`, extending a signal already used by
  `lmr_cutoff`/`fds_cutoff`.
- Shuffling guard — repetitive piece shuffling near the fifty-move rule disables singular extensions,
  limiting search explosions (Stockfish #6447).
- Opponent-worsening term in reverse futility pruning; "improving" also counts a node whose static
  eval already clears beta; the improving fallback chain extends to ply 6 for long same-side gaps.
- **Correction history updated on null-move fail-highs, moved to fire only once confirmed.** At
  `depth >= 16` a null-move fail-high is checked with a verification search before being trusted as a
  cutoff — but the correction-history update previously ran *before* that verification, so a value
  from a since-rejected fail-high could still land in the table. Moved to the two paths that actually
  return `score` (the immediate-trust branch, and after `verified_score >= bound`), the same
  statistical-validity concern documented for the singular-multicut correction update this fork tried
  and reverted: a sub-search result that hasn't been confirmed isn't comparable to the genuine
  `(full search result − static eval)` samples this table is built on elsewhere. Far-from-root
  singular-margin damping; a pre-qsearch TT-move extension at PV nodes that never overrides a
  negative singular decision.

**Structure and time:**

- **Root fail-lows extend the search.** PV stability, eval stability and best-move changes all
  describe how the answer is *moving*; none of them says the answer just got *worse*. A root fail-low
  means the move being played for is worse than the last iteration promised and there is no
  replacement yet — arguably the most valuable moment to keep thinking. Now counted per iteration and
  fed into the time multiplier, capped at two because repeated fail-lows at one depth are the
  aspiration window widening, not new evidence. `tm_fail_low` tunes to 0 to retire it.
- **A proven short mate stops the search.** Nothing did this outside `go mate`. The ordinary damping
  signals barely notice a mate: the node fraction collapses to ~0.66 and `score_trend` pins to its
  0.7214 floor, but `pv_stability`, `eval_stability` (which *resets* on the eval jump) and best-move
  stability push back, leaving **~0.83x of a full allocation** spent on a proven mate in 1. Any
  forced mate wins, so a shorter one found two iterations later is worth nothing on the clock.
  Guarded to wins only (being mated is exactly when to keep looking for a defence), to true mate
  scores rather than TB wins, to exact scores rather than bounds, to single-PV, and to thread 0 --
  there is no best-thread vote, so a helper stopping everyone could leave the reporting thread
  emitting a non-mating move. `depth` must clear the mate distance by `tm_mate_confirm`.
- **Single legal move stops early.** With one legal root move there is nothing to choose between.
  The search now runs to depth 8 — enough for a ponder move and a sane score — and then banks the
  clock. Forced recaptures and single-reply checks are common enough for this to be worth real time,
  but it does shorten the ponder line, which is the reason it is listed here rather than as a fix.

- **Internal Iterative Reductions** — PV and expected-cut nodes without a TT move are reduced a ply
  from depth 6, exempting nodes on the previous iteration's principal variation. Upstream has no IIR;
  it penalises a missing TT move only through the LMR/FDS reduction bonus, so this is a second
  penalty on the same signal and the two have to be kept in balance (see
  [Correctness](#correctness-and-scale-fixes)). Note also that the `follow_pv` exemption did not work
  until the `previous_pv` fix — `pv_table` slot 0 was never written, so `follow_pv` was always false
  and IIR applied at every eligible node. Its current behaviour is therefore newer than its Elo
  evidence.
- **Post-LMR reduction gated on the position not having improved.** The parent-reduction half is
  upstream's (`stack[ply-1].reduction > reduction + N` → extra reduction). The `!opponent_worsening`
  half follows [PlentyChess](https://github.com/Yoshie2000/PlentyChess), which fires the same idea
  only when `staticEval <= -(prev staticEval)` — exactly the negation of the `opponent_worsening`
  this search already computes for RFP. Reducing further because the parent was reduced is better
  evidence when the position has not turned our way. Free: the signal was already in scope.
- Correction values computed before the TT probe, overlapping the work with the prefetch.
- Two-horizon falling-eval scaling — the time manager's trend factor also compares against the best
  score from four iterations ago.

### Protocol and usability

No effect on playing strength.

- **Pondering** — `go ponder` / `ponderhit` support and `bestmove ... ponder ...` output.
- **`searchmoves`** — root move filtering on the `go` command.
- **`UCI_ShowWDL`** — win/draw/loss estimates in `info` lines.
- **`SyzygyProbeDepth` / `SyzygyProbeLimit`** — user-tunable tablebase engagement.
- **SPSA tunables** — 131 search constants exposed as UCI options under the `spsa` cargo feature,
  with matching bounds in [`spsa.config`](spsa.config).
- Robustness: EOF handling, spin options clamped to their declared ranges, `movestogo 0` guarded
  against division by zero, `position startpos` honouring `UCI_Chess960`.

### Removed, and why

- **Classical (hand-crafted) evaluation terms** — pawn structure, bishop pair, rook placement.
  Superseded by the NNUE.
- **Killer moves and countermoves** were not reintroduced. Both duplicate what continuation history
  already encodes.
- **Classic Internal Iterative Deepening** was not added; it is superseded by IIR.
- **Minor-piece and major-piece correction-history tables.** Added alongside the material table (see
  [Unverified search changes](#unverified-search-changes)) at full strength on top of a five-term
  blend without adjusting the shared divisor, silently inflating every RFP/FP/LMR/NMP margin that
  reads `eval_correction()`. All three tables were pulled out and the blend returned to upstream's
  terms while the divisor bug was isolated; material has since been reintroduced with the divisor
  rescaled to match (`corr_weight_div: 76`). Minor and major have not been — reintroducing either
  means recomputing the divisor again for the added term(s), the same way, and testing it as its own
  isolated patch rather than bundled with anything else.
- **Depth-indexed history divisors** for late-move and futility pruning, replaced by a flat 1024. The
  table was fork-only and its per-depth values were never measured.
- **History pruning for bad-SEE noisy moves** (`hp_noisy_margin` / `hp_noisy_eval_margin`) — removed
  because it could **never fire**. It and Bad Noisy Futility Pruning gated on the same conditions
  (`!in_check && !is_direct_check && Stage::BadNoisy`, at `depth < 5` vs BNFP's `depth < 11`), BNFP
  ran first in the move loop, and BNFP's offset over that depth range (+82 to +258) sits far below
  the noisy check's (`captured × 3 + 306`, at least +633 for even a pawn). BNFP therefore pruned a
  strict superset and then called `skip_bad_noisy()`, abandoning the rest of the pool — the window
  where the noisy check could fire was empty by 375+ centipawns at every depth, not marginally. It
  cost a board probe and a `PieceType::value` match per bad capture to reach an unreachable branch.
  Worth generalising: two prunes sharing a stage and a `<= alpha` test will usually have one dominate
  the other, and the loser is invisible in profiles and untunable by SPSA.
- **SEE-pruning and history-pruning exemptions for checking moves.** Extended by analogy with
  LMP/FP, but the analogy does not carry: those decline to prune a narrow, heavily qualified slice of
  quiet moves, whereas SEE pruning is the main filter for losing captures and `is_direct_check` is
  broad. Exempting it waved through every checking move at every depth however much material it
  dropped — in qsearch especially, where such nodes dominate.
- **Qsearch checks, shared continuation history, a second check extension, a correction-history
  update on singular multicut, `razor_worsening`, `nmp_r_tt_history`, and per-search history decay**
  were each tried and removed.

**The lesson from all of the above:** every measurable Elo recovery in this fork's history came from
fixing arithmetic, not from adding ideas. When a change underperforms, check the units before
questioning the heuristic — a mis-scaled term looks tuned, because SPSA will report a value for it,
but a saturated or unnormalized coefficient has no gradient and whatever number it lands on is
arbitrary.

## Testing and tuning

Nothing in [Unverified search changes](#unverified-search-changes) should be trusted until it passes
game testing. Build both sides identically — same toolchain, both PGO or neither — since a build
asymmetry is worth several Elo on its own.

### SPRT (does this patch gain Elo?)

Build the candidate and a baseline (e.g. `git worktree add ../wreckless-base <commit> && make pgo`),
then:

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

**Choose bounds that match the question.** `elo0=0 elo1=5` asks "does this gain at least 5 nElo" and
is right for a change meant to add strength. For a correctness fix or a simplification, use
`elo0=-5 elo1=0` — "does this lose anything" — which resolves far sooner. A test whose true effect
sits on one of its bounds can run indefinitely without reaching either.

Test one patch per run. Bundling several changes makes a negative result uninterpretable.

**Optional adjudication.** Playing games out keeps conversion and endgame technique inside the
measurement; adjudicating them roughly halves the wall-clock cost. If you adjudicate, these
thresholds were derived by replaying this engine pair's own game records and checking every verdict
against the real result:

```bash
  -resign movecount=3 score=400 twosided=true \
  -draw   movenumber=40 movecount=8 score=5
```

That combination adjudicated 1494 of 1612 games and saved 32% of all plies with **zero** incorrect
verdicts. Raising the resign threshold *reduces* savings sharply (600cp saves only 6%, because a
larger margin only triggers deep into an already-decided conversion); lowering it past 400 starts
misclassifying won games. These figures are specific to this engine pair and evaluation scale — re-derive
them before applying elsewhere.

### SPSA (what should the tunable constants be?)

Build with all constants exposed:

```bash
cargo rustc --release --bin wreckless --features spsa
```

Feed [`spsa.config`](spsa.config) to an [OpenBench](https://github.com/AndyGrant/OpenBench) SPSA test
— SPSA needs many games, and OpenBench distributes them. Paste the results into `src/parameters.rs`
and confirm with a normal SPRT before keeping them; SPSA on too few games converges to noise.

Always benchmark and play with the default (non-`spsa`) build. The `spsa` feature reads every
parameter through a mutable static instead of a constant, which is measurably slower.

Three cautions specific to this codebase. A parameter that saturates its clamp gives SPSA no
gradient, so it will return an arbitrary value with a confident-looking precision — check the
operating range in pawns before trusting a tuned constant. Some parameters are deliberately coupled
(the conthist weights sum to a fixed total, enforced by a `const` assertion): moving one without the
other reintroduces a scale bug the fork has already paid for once. And a tuned constant is only
meaningful if the code that reads it can actually fire — before tuning a pruning margin, check that
an earlier prune in the same move loop doesn't already cut a superset of what it would (that is how
the noisy history prune turned out to be unreachable; see
[Removed](#removed-and-why)).

## Acknowledgements

- [Reckless](https://github.com/codedeliveryservice/Reckless) and its
  [contributors](https://github.com/codedeliveryservice/Reckless/graphs/contributors) — Wreckless
  is a fork and inherits effectively all of its strength from their work, including the NNUE networks
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
