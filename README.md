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
  batch below. Fixed by folding material into the existing `corr_minor_major` weight
- Untuned since: `corr_minor_major` and `corr_weight_div` have never had a real SPSA run against
  this specific 8-term blend, so their current values (128 and 102) are a reasoned choice rather
  than a measured one. The minor/major/material group carries full weight
  (`corr_minor_major: 128`), which makes the blend 5 + 3 = 8 effective terms, so the divisor that
  keeps it on upstream's scale is `upstream_div * (5 + 3 * corr_minor_major / 128) / 5` = 102.
  **Keep the two coupled.** A divisor below that figure divides the blend by less than it sums and
  inflates every RFP/FP/LMR/NMP margin that reads `eval_correction()` — the same failure as the
  original normalization bug described above. A value of 96 was tried and reverted for precisely
  that reason; it is indistinguishable from a bug this fork has already paid for once. If
  `corr_minor_major` changes, recompute the divisor rather than nudging it
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
- History Pruning extended to already-bad-SEE noisy moves (`hp_noisy_margin`), not just quiets —
  distinct from Bad Noisy Futility Pruning, which tests eval+history combined against alpha rather
  than a raw history cutoff

**Pruning and extensions:**

- **Null-move zugzwang guard fixed**: `board.material()` sums every piece including pawns (see
  `board/parser.rs`), so a pawn-heavy, piece-empty endgame — the textbook zugzwang scenario this
  guard exists to catch — could pass a `material() > 491` threshold and get null-move pruned
  anyway. Now checks `non_pawn_material()` (a new `Board` accessor), the correct signal for "is
  this position bare enough that null-move's assumptions might not hold"
- SEE pruning now exempts moves that give check, in both the main search and quiescence search,
  matching the exemption LMP/FP/HP already had — a static-exchange estimate can't see a check's
  follow-up tactical value, so it shouldn't prune one for looking like a bad trade
- SEE pruning threshold now also responds to `cutoff_count` (`see_q_cutoff`/`see_n_cutoff`),
  extending the same signal already used by `lmr_cutoff`/`fds_cutoff`
- Qsearch delta pruning: a capture that can't plausibly reach alpha even crediting the full
  captured-piece value is skipped before the pricier SEE call (`qs_delta_margin`) — a standard
  technique, not previously present here
- Recapture extension: a capture landing on the square the opponent's last move captured on, that
  doesn't lose material itself, gets a full extra ply — compensates for the horizon effect at the
  end of a forced capture sequence. A different technique from the check extension tried and
  removed earlier (gated on square repetition and SEE, not on giving check)
- History Pruning now exempts quiet moves that give check, matching the exemption LMP and FP
  already had — HP was the one sibling pruning check that could discard a checking move on history
  alone
- TT-only ProbCut check: a lower-bound TT entry from a near-full-depth search, comfortably above
  beta, is trusted as a cutoff without any further search
- Opponent-worsening term in reverse futility pruning: the margin shrinks when the evaluation swung
  further in our favor than the opponent's null-move expectation
- "Improving" also counts a node whose static eval already clears beta
- Improving signal's ply-2/ply-4 fallback chain extended to ply-6, for long same-side-to-move gaps
  (e.g. extended check-evasion sequences) where neither ply-2 nor ply-4 is available
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

### Correctness fixes

Found via a systematic audit (transposition table, time management, threading, board state,
hashing), not through game testing — these are bugs in the fork's own code, independent of whether
any specific search/eval feature helps or hurts. **The node-limit time-check fix in particular means
any earlier SPRT run in this project's history that used `go nodes N` rather than a time control may
have been measured under an artificially slowed engine** — worth keeping in mind when weighing older
results against new ones.

- **Unthrottled node-limit time check** — `Limits::Nodes` in `check_time()` called
  `Counter::aggregate()` (summing every shard — at least 512, per `ThreadPool::available_threads()`)
  on *every node*, while every other limit type was already gated behind the same `& 2047 == 2047`
  periodic mask. A severe NPS penalty specific to node-limited search — exactly the mode typically
  used for deterministic SPRT testing. Now gated the same as the rest.
- **TT verification key race under multithreading** — `Cluster.keys` packs all 3 entries'
  verification keys into one `u64`, updated via a plain (non-atomic) read-modify-write from
  `write()`, which every lazy-SMP thread calls concurrently. Two threads updating *different* slots
  in the same cluster could race: one thread's update, computed from a stale read, could silently
  revert the other's — corrupting a sibling slot's key, not just the writer's own entry (worse than
  the "torn own entry" tradeoff this lock-free design otherwise deliberately accepts, the same way
  Stockfish's TT does). `keys` is now an `AtomicU64` updated via `fetch_update`.
- **Cyclic (`movestogo`) hard time bound could consume the entire remaining clock** — with a small
  `movestogo` (e.g. 2), `5x` the per-move time allocation already exceeds the remaining clock, so the
  hard safety bound collapsed to "everything left," even though another move is due before the
  control replenishes. Now reserves one more allocation's worth of time when a next move is still due.
- **Worker-thread panic caused a silent, permanent hang instead of a diagnosable crash** — if a
  search thread's work panicked, the completion signal update was skipped entirely, so the caller's
  `ReceiverHandle::join()` blocked forever with no output. The signal now fires regardless (via
  `catch_unwind`) before the panic is re-raised, so it's still visible rather than a silent freeze.
- **`TtDepth::NONE` was unreachable** — the depth encoding put both a real `SOME`-depth write and a
  genuinely empty (zero-initialized, never-written) slot at the same raw byte value, making them
  indistinguishable. The "found a truly free slot" fast path in the TT replacement logic could never
  trigger as a result. Re-encoded so a never-written slot is uniquely identifiable.
- **Qsearch delta pruning didn't credit promotion value** — for a non-capture promotion,
  `type_on(mv.to())` reads an empty square (value 0), not the ~1133cp actually gained by promoting,
  which could prune a winning promotion out of qsearch in a low-eval position. Now credits the
  promoted piece's value swing separately.
- **`ttMoveHistory`'s gravity update was missing its bonus clamp** — every other gravity-style
  history update in the codebase clamps its bonus first; this one didn't, letting the multicut caller
  push the tracked value briefly past its documented ±8192 bound at high depth.
- Several lower-severity hygiene fixes from the same audit: `CorrectionHistory::update` now clamps
  its bonus internally (previously safe only because its one caller already did); the LMR
  `reduced_depth` PV bonus is applied before its clamp rather than after (previously could exceed its
  own ceiling by up to 2 plies); Chess960 castling no longer leaves a stale rook in `state.captured`
  (inert today — the one consumer already gates on move kind — but a real invariant violation); the
  `Zobrist` table's `mem::transmute` from a flat array is now backed by `#[repr(C)]` rather than
  relying on Rust's unguaranteed default field ordering.

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

### Protocol / usability

- **Pondering** — `go ponder` / `ponderhit` support and `bestmove ... ponder ...` output
- **`searchmoves`** — root move filtering on the `go` command
- **`UCI_ShowWDL`** — win/draw/loss estimates in `info` lines
- **`SyzygyProbeDepth` / `SyzygyProbeLimit`** — user-tunable tablebase engagement
- **SPSA tunables** — 122 search constants exposed as UCI options under the `spsa` cargo feature,
  for OpenBench SPSA tuning; identical compiled code in default (non-`spsa`) builds. A ready-to-use
  OpenBench SPSA input file is provided in [`spsa.config`](spsa.config). It is derived
  mechanically from the defaults in `src/parameters.rs` (min/max at ±50%, step at 5% of the
  magnitude, learning rate 0.002) and must be regenerated whenever a default changes or a parameter
  is added or removed — it had previously drifted, leaving two parameters absent from it entirely
  and one default outside the range the file offered for it

### Removed, and why

Nothing below is present in the current source — this section exists so the reasoning isn't lost
and doesn't get re-litigated by mistake.

- **Classical (hand-crafted, not learned) evaluation terms** (pawn structure, bishop pair, rook
  files, outposts, safe mobility, king safety) added on top of the NNUE output, gated behind
  phase-scaled weights to limit double-counting risk with signal the network already learned from
  real games. Tested negative in SPRT **twice** — once unscaled, once gated (25%/63% weights) — even
  after fixing a real, concrete bug (`outpost_score`'s attackable-zone direction was backwards,
  awarding the outpost bonus almost unconditionally) and adding a standard technique (safe mobility,
  excluding enemy-pawn-attacked squares). Two negative results in a row on the same broad approach,
  surviving real bug fixes, is a meaningful signal that a hand-crafted eval on top of this
  NNUE doesn't pay off — not just an unlucky sample. Removed entirely (`classical_eval.rs` and its
  22 SPSA parameters) rather than continuing to lower the weight indefinitely.
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
- **The three fork-original razoring margin terms** (`razor_corr`, `razor_cutoff`,
  `razor_worsening`) were removed, restoring razoring to the `base + quad * depth^2` shape upstream
  uses. None was ported or derived; each was introduced as an admitted guess, described in
  `parameters.rs` as, respectively, a "rougher estimate ... needs SPSA/SPRT more than most values
  here", "guessed, not just exposed", and "genuinely ambiguous how to scale". `razor_worsening` was
  additionally suspect on its own terms: it made the engine razor *more* readily when the evaluation
  had swung further in our favour than expected, inverting how the same opponent-worsening signal is
  used in RFP, where good news supports trusting a fail-high rather than giving up on the node
  earlier. Razoring is the cheapest and most aggressive early exit in the search — it abandons a
  node to qsearch outright — so speculative widening of its margin has more downside than the same
  guess would elsewhere.
- **Null-move reduction responding to `ttMoveHistory`** (`nmp_r_tt_history`) was removed. It was the
  one item this section's own notes ranked as "lower confidence than the fixes above": a plausible
  correlation between a well-trusted TT move and a settled, less volatile position, never a derived
  relationship. `ttMoveHistory` still feeds the singular double-extension margin, where there is an
  actual mechanism behind it.
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
