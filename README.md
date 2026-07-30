<div align="center">
  <h1>Wreckless Chess Engine</h1>

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
</div>

Wreckless is a UCI chess engine, a fork of [Reckless](https://github.com/codedeliveryservice/Reckless)
by Arseniy Surkov, Shahin M. Shahin, and Styx — an open source competitive engine that consistently
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

**Wreckless currently measures at parity with upstream Reckless.** Recent SPRT runs at 10+0.1,
1 thread, 128 MB hash against the upstream baseline land between roughly −2 and +3 Elo, with 95%
confidence intervals of ±10 to ±20 — that is, indistinguishable from zero in either direction.

That is worth stating plainly because it is the honest reading, and because it has not always been
true. Earlier states of this fork measured as much as 17 Elo *behind* upstream, and the work that
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
- **The time manager's falling-eval factor was a two-level switch.** The linear structure is
  Stockfish's and the coefficient ratio was preserved, but the constants were carried across
  unscaled from a scale where a pawn is ~208 to one where it is ~321–382. The result cleared its
  clamp ceiling after a **0.04-pawn** score drop and sat on the floor for any gain at all — no
  proportional band, and therefore no gradient for SPSA to have tuned it against. Rescaled so the
  ceiling is reached at ~0.37 pawns, matching Stockfish's band in pawn terms, and moved into
  `parameters.rs` as fixed point so it can actually be tuned now that there is a gradient.
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
- **Windows large pages** — the transposition table and continuation-history tables use 2 MB pages
  where the OS grants the privilege.
- **Hoisted mailbox reads in move scoring.** `conthist(ply, i, mv)` resolved `piece_on(mv.from())`
  internally, so six lag lookups meant six reads of the same square — loop-invariant, but the read
  through a raw pointer inside `get` stops the optimizer hoisting it. `conthist_at` now takes the
  resolved piece and destination. In `score_quiet` this cut **nine** reads of `mv.from()` per quiet
  move to one, since `type_on(sq)` is exactly `piece_on(sq).piece_type()` and `moved_piece(mv)` is
  `piece_on(mv.from())`.
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