<div align="center">
  <h1>Wreckless Chess Engine</h1>

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
</div>

Wreckless is a UCI chess engine, a fork of [Reckless](https://github.com/codedeliveryservice/Reckless)
by Arseniy Surkov, Shahin M. Shahin, and Styx — an open source competitive engine that consistently
performs among the top engines in major tournaments including the
[Chess.com Computer Chess Championship (CCC)][ccc] and [Top Chess Engine Championship (TCEC)][tcec].
Wreckless inherits virtually all of its playing strength from Reckless, including its NNUE networks,
and layers additional search techniques on top — see [Changes relative to upstream](#changes-relative-to-upstream)
for what's different and how well-tested each change currently is.

[ccc]: https://www.chess.com/computer-chess-championship
[tcec]: https://tcec-chess.com

## Contents

- [Quick start](#quick-start)
- [Building from source](#building-from-source)
- [UCI options](#uci-options)
- [Custom commands](#custom-commands)
- [Changes relative to upstream](#changes-relative-to-upstream)
- [Testing and tuning](#testing-and-tuning)
- [Acknowledgements](#acknowledgements)

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

Every change below is labeled with how confident you should be in it. **Verified** means it passed
SPRT testing against the upstream baseline. **Pending** means it's implemented and passes correctness
checks (perft, bench, clippy) but hasn't cleared game testing yet — treat these as experimental.
Reasoning for what was tried and removed is in [Removed](#removed-and-why) below; it's worth reading
if you're deciding whether to trust a "pending" item.

### Search (pending SPRT verification)

**Correction history** — additional tables beyond upstream's pawn/non-pawn/continuation set:

- Material-key table (piece-count-only Zobrist key)
- Minor-piece table (knight/bishop/king placement, as in Stockfish)
- Major-piece table (rook/queen/king placement, as in Stormphrax)
- Minor/major/material blend weight is SPSA-tunable (`corr_minor_major`)
- The blend's shared divisor (`corr_weight_div`) is rescaled to match: upstream tuned it for a
  5-term sum (pawn, non-pawn ×2, continuation ×2), and this fork's 3 extra tables were originally
  added at full strength on top of that sum without adjusting the divisor — silently inflating
  every RFP/FP/LMR/NMP margin that reads `eval_correction()`. This was the actual source of the
  persistent Elo losses that were, for a long time, mistakenly attributed to the qsearch-checks
  batch below. Fixed by folding material into the existing `corr_minor_major` weight and scaling
  `corr_weight_div` from 64 to 102 (proportional to the 5→8 term increase)

**Move ordering:**

- Low-ply history: root-relative `[ply][from][to]` table for plies 0–4, carried over between searches
- Continuation history: all six lags updated with per-lag weights and a positive-consistency
  multiplier (as in Stockfish), near lags limited when in check, overall scale SPSA-tunable
  (`conthist_div`); shared across search threads via atomic storage, matching Stockfish's own
  `sharedHistory.continuationHistory` (verified directly against Stockfish source, not a guess)
- Good/bad quiet split: quiets with strongly negative history are deferred until after bad captures
  (Stockfish's `GOOD_QUIET`/`BAD_QUIET` ordering); the threshold is SPSA-tunable (`good_quiet_threshold`)
- Depth-indexed history divisors for late-move and futility pruning, replacing a flat divisor
- TT-move reliability statistic (`ttMoveHistory`): a gravity-updated track record of how often the
  TT move turns out best, feeding the singular double-extension margin
- Best-move history bonus scaled by how many other moves were searched first, at non-PV nodes
- Captured-piece value credited in noisy-move reductions (Stockfish's capture `statScore`),
  strength SPSA-tunable (`lmr_capture_stat`)

**Pruning and extensions:**

- TT-only ProbCut check: a lower-bound TT entry from a near-full-depth search, comfortably above
  beta, is trusted as a cutoff without any further search
- Opponent-worsening term in reverse futility pruning: the margin shrinks when the evaluation swung
  further in our favor than the opponent's null-move expectation
- "Improving" also counts a node whose static eval already clears beta
- Shuffling guard: repetitive piece shuffling near the 50-move rule disables singular extensions,
  preventing search explosions (Stockfish #6447)
- RFP skipped when the TT move is quiet with strongly negative history
- Correction history updated on confirmed null-move fail-highs
- Far-from-root singular-extension margin damping
- Pre-qsearch TT-move extension at PV nodes, gated by TT depth, that never overrides a negative
  (singular) extension decision
- Check extension: a move giving direct check that would otherwise fall straight into qsearch is
  extended a full ply
- Singular-extension recursion cap: singular search is skipped beyond `ply < root_depth × 2`, on
  top of the existing single-level `!excludedMove` guard

**Quiescence search:**

- Checking quiets at the first ply: alongside the usual captures/promotions, quiet moves giving
  check are searched too, gated by an eval margin (`qs_checks_margin`) and capped by count
  (`qs_checks_max`) so it stays cheap. A TT-depth distinction (`TtDepth::QS_CHECKS` vs. `SOME`)
  ensures a plain captures-only cutoff can never be reused where a checks-considered result was
  required

**Search structure:**

- Internal Iterative Reductions restored in Stockfish's current form: PV and expected-cut nodes
  without a TT move are reduced by one ply from depth 6, exempting nodes on the previous
  iteration's principal variation
- Aspiration fail-low rebound: beta collapses to the failed window's floor before alpha drops,
  keeping re-searches narrow
- Correction values computed before the TT probe, overlapping the work with the prefetch

**Time management:**

- Two-horizon falling-eval scaling: the time manager's score-trend factor also compares against the
  best score from four iterations ago (Stockfish's `fallingEval`), extending time when the
  evaluation is sliding across recent iterations

### Speed

- **PEXT bitboards** — sliding-piece attacks indexed with the BMI2 `pext` instruction where
  supported (classical magic multiplication as fallback). Disabled automatically on AMD Zen 1/2,
  where `pext` is microcoded; override with `WRECKLESS_PEXT=0|1`
- **Windows large pages** — the transposition table and continuation-history tables use 2 MB pages
  via `VirtualAlloc(MEM_LARGE_PAGES)` when the "Lock pages in memory" privilege is held, falling
  back to regular pages otherwise (Linux already used `MADV_HUGEPAGE`)
- **Unchecked hot-path indexing** — the per-ply search stack and ply-indexed arrays, accessed many
  times at every node, skip the bounds check in release builds. The same `debug_assert` that
  guarded the safe indexing before still covers debug builds. Verified node-identical — a pure
  speed change with no behavior difference

### Protocol / usability

- **Pondering** — `go ponder` / `ponderhit` support and `bestmove ... ponder ...` output
- **`searchmoves`** — root move filtering on the `go` command
- **`UCI_ShowWDL`** — win/draw/loss estimates in `info` lines
- **`SyzygyProbeDepth` / `SyzygyProbeLimit`** — user-tunable tablebase engagement
- **SPSA tunables** — 98 search constants exposed as UCI options under the `spsa` cargo feature,
  for OpenBench SPSA tuning; identical compiled code in default (non-`spsa`) builds. A ready-to-use
  OpenBench SPSA input file is provided in [`spsa.config`](spsa.config)

### Removed, and why

Nothing below is present in the current source — this section exists so the reasoning isn't lost
and doesn't get re-litigated by mistake.

- **A large speculative stack** (killers, countermoves, one-reply extension, qsearch futility
  pruning, volatility-based pruning, entropy-based time scaling, history decay applied on every
  move, and others) measured **−69 Elo ± 39** under SPRT against the upstream baseline and was
  removed wholesale.
- **Killer moves and countermoves** were not reintroduced. Both duplicate what continuation history
  already does more precisely — continuation history is context-conditioned (keyed on the actual
  preceding move), while killers/countermoves are only ply- or square-indexed. Layering the weaker
  mechanism on top of the stronger one was the leading suspect behind the bisected regression above.
- **Classic Internal Iterative Deepening** was not added. It's superseded by Internal Iterative
  *Reductions*, already present: IIR gets the same TT-population benefit from a cheaper reduced
  search rather than a separate full extra search.
- **Qsearch checks, shared continuation history, the check extension, and a singular-extension
  recursion cap** were removed as a batch after plateauing around **−18 to −40 Elo** across many
  SPRT samples, even after fixing every identified bug in qsearch checks (an early-cutoff TT bypass
  and a late-move-pruning coverage gap) and reference-checking the other two against Stockfish's
  actual source. All four were later **restored** once a full-engine audit (see below) found the
  real regression source elsewhere: this batch was never the cause, so there was no reason to keep
  it out. They're documented under their normal sections above, not here.
- **A correction-history update on singular multicut** (feeding the gap between the singular
  search's value and the static eval into correction history, as described for PlentyChess) was
  implemented and removed after code review: the singular sub-search excludes the TT move and runs
  at reduced depth, so its result isn't statistically comparable to the genuine
  `(full search result − static eval)` samples correction history is built on elsewhere. Since
  correction history feeds every RFP/FP/LMR/NMP margin, this had unusually high leverage as a
  regression source.
- **History decay applied at the start of every search** (halving quiet/noisy/pawn history on every
  `go` command) was removed — it was stacking on top of an already-self-regulating gravity-decay
  mechanism built into every history table's update function, and fired far more often than
  intended (every move of every game, not occasionally).

**The actual lesson**: repeatedly bisecting and patching that batch never moved the Elo, because the
batch was never the problem. The real bug was the `corr_weight_div` normalization issue described
under [Correction history](#search-pending-sprt-verification) above — present since the very first
correction-history table beyond upstream's original three, and orthogonal to everything in this
section. It surfaced only once the audit stopped re-litigating the recently-changed code and started
checking older, previously-trusted code instead. The per-lag continuation-history reweighting fix
(lags 2/4/6 had been silently weakened to 43–79% of their original strength) was kept throughout,
since it's correct independent of anything else in this section.

## Testing and tuning

All "pending" items above are unverified until they pass game testing. If you're evaluating a
change, this is the expected workflow:

### SPRT (does this patch gain Elo?)

1. Build the candidate and a baseline binary to compare against (e.g. via
   `git worktree add ../wreckless-base <commit> && make` for the baseline).
2. Run a sequential probability ratio test with [fastchess](https://github.com/Disservin/fastchess)
   and a standard opening book such as `UHO_Lichess_4852_v1.epd`:

   ```bash
   fastchess -engine cmd=wreckless name=test -engine cmd=wreckless-base name=base \
     -each tc=10+0.1 option.Hash=16 option.Threads=1 proto=uci \
     -openings file=UHO_Lichess_4852_v1.epd format=epd order=random \
     -repeat -games 2 -rounds 30000 -concurrency 8 -recover \
     -sprt elo0=0 elo1=5 alpha=0.05 beta=0.05 -ratinginterval 200
   ```

3. The run stops itself: H1 accepted means the patch gains Elo, H0 accepted means it doesn't.

Test one patch per branch — bundling several changes into one SPRT run makes it impossible to tell
which one actually mattered if the result is negative. Always test with the default (non-`spsa`)
build; the `spsa` feature build reads parameters through extra indirection and is measurably slower.

### SPSA (what should the tunable constants be?)

Build with all constants exposed:

```bash
cargo rustc --release --bin wreckless --features spsa
```

Feed [`spsa.config`](spsa.config) to an [OpenBench](https://github.com/AndyGrant/OpenBench) SPSA
test (preferred — SPSA needs many games, and OpenBench distributes them across workers), or tune
one parameter group at a time locally with a cutechess-based SPSA driver if you don't have access
to distributed workers. Once you have new values, paste them into `src/parameters.rs` and run a
normal SPRT to confirm the tuned result is actually better before keeping it — SPSA on too few
games can converge to noise.

## Acknowledgements

- [Reckless](https://github.com/codedeliveryservice/Reckless) and its
  [contributors](https://github.com/codedeliveryservice/Reckless/graphs/contributors) — Wreckless
  is a fork and inherits virtually all of its strength from their work, including the NNUE networks
  from [RecklessNetworks](https://github.com/codedeliveryservice/RecklessNetworks)
- [OpenBench](https://github.com/AndyGrant/OpenBench), the primary testing framework, powered by
  [Cute Chess](https://github.com/cutechess/cutechess)
- [Bullet](https://github.com/jw1912/bullet), the NNUE trainer
- [Stockfish](https://github.com/official-stockfish/Stockfish),
  [PlentyChess](https://github.com/Yoshie2000/PlentyChess),
  [Ethereal](https://github.com/AndyGrant/Ethereal), [Berserk](https://github.com/jhonnold/berserk),
  and many other open source chess engines
- [Chess Programming Wiki](https://www.chessprogramming.org/Main_Page)

## License

This project is licensed under the [GNU Affero General Public License v3.0](LICENSE).
