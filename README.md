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

### Evaluation (pending SPRT verification)

**Classical (hand-crafted, not learned) evaluation terms** — added on top of the NNUE output in
plain centipawn space, in [`classical_eval.rs`](src/classical_eval.rs):

- Pawn structure: doubled, isolated, backward, phalanx/connected, and defended (chain) pawns; passed
  pawns with a per-relative-rank bonus
- Bishop pair; rook on an open or semi-open file; knight/bishop outposts (pawn-defended, unreachable
  by an enemy pawn, in enemy territory)
- Simple mobility (attacked-square count, weighted per piece type) for knights, bishops, rooks, queens
- King safety: pawn-shield gaps directly in front of the king; king on an open file

Every constant above is SPSA-tunable (`doubled_penalty`, `isolated_penalty`, `backward_penalty`,
`phalanx_bonus`, `chain_bonus`, `passed_r1`–`passed_r6`, `bishop_pair_bonus`,
`rook_open_file_bonus`, `rook_semi_open_file_bonus`, `knight_outpost_bonus`, `bishop_outpost_bonus`,
`knight_mobility`, `bishop_mobility`, `rook_mobility`, `queen_mobility`, `king_shield_penalty`,
`king_open_file_penalty`), but the starting values are reasoned guesses from classical (pre-NNUE)
engine evaluation, not SPSA/SPRT output — unlike the rest of this file's constants, they aren't even
normalized against an internal `/1024`-style scale, since they're a direct centipawn addition.

This intentionally lives outside the network's own feature transformer rather than as a new NNUE
input: a brand-new *learned* feature has no meaningful weights without training data (zero-init is a
no-op, random-init is noise once it flows through the already-trained layers), so this gets the same
non-zero, non-random signal classical engines got before NNUE, at a magnitude that's transparent and
safe to tune directly.

**Known risk, and how it's mitigated:** a hand-crafted eval added on top of an already-trained NNUE
risks double-counting signal the network already learned from real games, then distorting a
calibration that was tuned assuming that signal didn't exist twice. A first version added all these
terms directly, unscaled, and tested clearly negative in an SPRT. It's now gated behind two weights
(`classical_eval_weight` for the middlegame, `classical_eval_endgame_weight` for material-light
endgames, out of 128 = full strength) and phase-interpolated by `board.material()` — defaulting low
(25%) where redundancy risk with the network is highest, higher (63%) where classical rules are
well-established and network training data is typically sparser. King safety is deliberately
excluded from that ramp and given its own flat weight (`king_safety_weight`): an exposed king is a
liability with material on the board but an asset once it's bare, so scaling shield/open-file
penalties *up* toward the endgame — as the rest of the terms do — would fight against the piece it's
evaluating.

**A real bug found and fixed during audit:** `outpost_score`'s "can no enemy pawn ever attack this
square" check used `ahead_mask(rank_index, !color)` — ranks *behind* the piece from its own color's
perspective. Since enemy pawns essentially never sit behind an established piece in normal play, that
check was almost always trivially true, awarding the outpost bonus almost unconditionally whenever
the rank/defense conditions were met, regardless of whether an advancing enemy pawn could actually
still kick the piece off the square. Fixed to `ahead_mask(rank_index, color)` — ranks *ahead* of the
piece, the direction an enemy pawn still has to advance through to ever threaten it.

**Speed**: pawn placement is the priciest term here (nested per-file/per-pawn loops with neighbor
lookups) and identical between the two `classical_score` calls a node makes (side to move and its
opponent) whenever they share a pawn structure — true for most of the tree, since most moves don't
touch a pawn. It's cached by `pawn_key()` (a dedicated per-thread `PawnCache`, 65536 entries) instead
of recomputed from scratch every node. Occupancy (`board.occupancies()`) is likewise
color-independent and computed once per node rather than twice.

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
  batch below. Fixed by folding material into the existing `corr_minor_major` weight
- Untuned since: `corr_minor_major` and `corr_weight_div` have never had a real SPSA run against
  this specific 8-term blend, so their current values (80 and 88) are a reasoned guess — the
  minor/major/material group is treated as an unproven addition and damped to ~63% weight
  (`corr_minor_major: 80`) rather than trusted equally with the terms upstream actually tuned, and
  the divisor is rescaled by that group's *effective* contribution (~6.9 effective terms) rather
  than its raw table count
- `corr_bonus_min`/`corr_bonus_max` (the update clamp shared by every correction table) were
  asymmetric (4678 / 2496) despite every other history table in the codebase clamping
  symmetrically — letting negative corrections swing ~2x larger than positive ones, a systematic
  pessimism bias with no documented rationale. Both now match the smaller, already-shipped bound
  (2496)

**Move ordering:**

- Low-ply history: root-relative `[ply][from][to]` table for plies 0–4, carried over between searches
- Continuation history: all six lags updated with per-lag weights and a positive-consistency
  multiplier (as in Stockfish), near lags limited when in check, overall scale SPSA-tunable
  (`conthist_div`); per-thread, matching upstream (an attempt to share it across threads, the way
  Stockfish shares its own `sharedHistory.continuationHistory`, is covered in
  [Removed](#removed-and-why) below)
- Good/bad quiet split: quiets with strongly negative history are deferred until after bad captures
  (Stockfish's `GOOD_QUIET`/`BAD_QUIET` ordering); the threshold is SPSA-tunable (`good_quiet_threshold`)
- Depth-indexed history divisors for late-move and futility pruning, replacing a flat divisor
- TT-move reliability statistic (`ttMoveHistory`): a gravity-updated track record of how often the
  TT move turns out best, feeding the singular double-extension margin
- Best-move history bonus scaled by how many other moves were searched first, at non-PV nodes
- Captured-piece value credited in noisy-move reductions (Stockfish's capture `statScore`),
  strength SPSA-tunable (`lmr_capture_stat`)

**Pruning and extensions:**

- Razoring margin widens with correction-history magnitude (`razor_corr`), matching the same
  uncertainty-scaled-margin pattern already used by RFP, FP, and LMR/FDS — razoring was the one
  early-pruning decision that read `correction_value` nowhere
- History Pruning now exempts quiet moves that give check, matching the exemption LMP and FP
  already had — HP was the one sibling pruning check that could discard a checking move on history
  alone
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
- **Search stack reuse** — the per-ply search stack (`Stack`) was reallocated from scratch on every
  aspiration-window retry and every iterative-deepening depth (`Stack::new()`, a fresh `Box` alloc
  plus a `MAX_PLY+16`-entry init loop, called inside the hottest retry loop in the engine). It's now
  reset in place (`Stack::reset()`), reusing the one allocation made at thread startup. Verified
  node-identical (bench and perft unaffected) — a pure speed change
- **Per-thread pawn-structure cache** — the classical pawn-structure term (above) is pure-computed
  from pawn placement, is evaluated for both the side to move and the opponent at every node, and is
  identical across any two positions sharing a pawn structure (most of the tree, since most moves
  don't touch a pawn). It's now cached per thread, keyed by `pawn_key()` (65536-entry direct-mapped
  table), so it's computed once per distinct pawn structure instead of recomputed from scratch on
  every call. Deterministic and lossless by construction (a cache hit returns the exact value a fresh
  computation would), but introduced in the same session as the classical-eval feature it caches and
  not yet independently bench/perft-verified against a clean build — confirm before relying on it

### Protocol / usability

- **Pondering** — `go ponder` / `ponderhit` support and `bestmove ... ponder ...` output
- **`searchmoves`** — root move filtering on the `go` command
- **`UCI_ShowWDL`** — win/draw/loss estimates in `info` lines
- **`SyzygyProbeDepth` / `SyzygyProbeLimit`** — user-tunable tablebase engagement
- **SPSA tunables** — 138 search and evaluation constants exposed as UCI options under the `spsa` cargo feature,
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
  actual source. All four were then **restored** once a full-engine audit found `corr_weight_div`
  (below) — a real, independent bug — as a plausible explanation for the persistent negative
  results. That restored candidate subsequently tested at **≈−18 Elo at n≈394** (wide error bars,
  not yet SPRT-resolved) — better than before, but still negative rather than clearly positive, so
  the fix alone hasn't been confirmed to fully explain the earlier results. All four were **removed
  again** to let `corr_weight_div` be tested in isolation, cleanly separated from this batch's own
  (still unproven) effect on Elo. Restore them again only once that isolated test has a result.
  - As a side effect of this second removal, the continuation-history table also reverted from
    shared/atomic back to per-thread, non-atomic storage (see [Move ordering](#search-pending-sprt-verification)
    above) — the sharing itself was never implicated in anything, it just travels with qsearch
    checks as part of the same historical batch.
- **A second check extension** (full-search-depth only, gated by shallow remaining depth and
  non-losing SEE) was tried again, without first cross-checking the history above, and removed. A
  different implementation of the same broad technique that already plateaued at −18 to −40 Elo
  unisolated once — treated as higher-risk than a typical "pending" item for that reason, and
  removed on request rather than risk-tested.
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

**The lesson so far**: repeatedly bisecting and patching that batch never moved the Elo, which is
what led to auditing older, previously-trusted code instead of the recently-changed batch — and
that's how the `corr_weight_div` normalization bug described under
[Correction history](#search-pending-sprt-verification) above was found. It's a genuine, independent
defect, present since the very first correction-history table beyond upstream's original three. What
isn't yet established is whether it's the *whole* explanation: the restored batch plus that fix still
tested negative (though less so) rather than clearly positive, so causality here is still open — it
could be that the fix is real but this batch is separately, mildly net-negative on its own. Isolated
testing of the fix alone (batch removed) is the next step to resolve that. The per-lag
continuation-history reweighting fix (lags 2/4/6 had been silently weakened to 43–79% of their
original strength) was kept throughout, since it's correct independent of anything else in this
section.

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
