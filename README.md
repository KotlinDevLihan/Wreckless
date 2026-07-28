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
- Lower-severity hygiene from the same audit: `CorrectionHistory::update` clamps its own bonus; the
  LMR `reduced_depth` PV bonus is applied before its clamp rather than after; Chess960 castling no
  longer records the friendly rook in `state.captured`; the `Zobrist` table's `transmute` is backed
  by `#[repr(C)]` rather than Rust's unguaranteed field ordering.

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
- **A more permissive good/bad noisy split.** Once several good captures have been tried behind a
  quiet TT move, upstream sends every remaining noisy move to `bad_noisy` regardless of SEE; this
  fork instead re-tests them against a fixed threshold, so material-winning captures still sort
  early.
- TT-move reliability statistic (`ttMoveHistory`), a gravity-updated record of how often the TT move
  proves best, feeding the singular double-extension margin.
- Captured-piece value credited in noisy reductions (Stockfish's capture `statScore`), strength
  tunable via `lmr_capture_stat`.

**Pruning and extensions:**

- Qsearch delta pruning — a standard technique, but one upstream does not have at all, so every move
  it prunes is one the baseline searches.
- History pruning extended to bad-SEE noisy moves (`hp_noisy_margin`), gated by an eval check so a
  capture that wins material back survives regardless of its history. Note `hp_margin` itself is the
  one fork-only pruning gate with no anchor — the others carry Stockfish's measured constants for
  mechanisms copied verbatim, but 948 was never measured against anything.
- Recapture extension — a capture landing where the opponent's last move captured, that doesn't lose
  material itself, gets a full ply. Gated on square repetition and SEE, not on giving check.
- TT-only ProbCut — a lower-bound TT entry from a near-full-depth search, comfortably above beta, is
  trusted as a cutoff without any search. This is the only place in the search that returns a score
  nothing ever searched, so it is held to the same gating as its neighbours (non-PV, non-excluded,
  not decisive).
- SEE pruning thresholds respond to `cutoff_count`, extending a signal already used by
  `lmr_cutoff`/`fds_cutoff`.
- Shuffling guard — repetitive piece shuffling near the fifty-move rule disables singular extensions,
  limiting search explosions (Stockfish #6447).
- Opponent-worsening term in reverse futility pruning; "improving" also counts a node whose static
  eval already clears beta; the improving fallback chain extends to ply 6 for long same-side gaps.
- Correction history updated on confirmed null-move fail-highs; far-from-root singular-margin
  damping; a pre-qsearch TT-move extension at PV nodes that never overrides a negative singular
  decision.

**Structure and time:**

- Internal Iterative Reductions in Stockfish's current form — PV and expected-cut nodes without a TT
  move reduced a ply from depth 6, exempting nodes on the previous iteration's principal variation.
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
- **Material, minor-piece and major-piece correction-history tables.** These were added at full
  strength on top of a five-term blend without adjusting the shared divisor, silently inflating every
  RFP/FP/LMR/NMP margin that reads `eval_correction()`. Rather than keep coupling a hand-computed
  divisor to a hand-picked weight, the three extra tables were removed and the blend returned to
  upstream's terms.
- **Depth-indexed history divisors** for late-move and futility pruning, replaced by a flat 1024. The
  table was fork-only and its per-depth values were never measured.
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

Two cautions specific to this codebase. A parameter that saturates its clamp gives SPSA no gradient,
so it will return an arbitrary value with a confident-looking precision — check the operating range
in pawns before trusting a tuned constant. And several parameters are deliberately coupled
(`hp_noisy_margin` is derived from `hp_margin`; the conthist weights sum to a fixed total): moving one
without the other reintroduces a scale bug the fork has already paid for once.

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
