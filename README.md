<div align="center">
  <img src="public/wreckless_logo.png" alt="Wreckless" width="220">

  <h1>Wreckless</h1>

  <p><em>A UCI chess engine, and a laboratory for search ideas.</em></p>

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![Rust](https://img.shields.io/badge/rust-2024_edition-orange.svg)](https://www.rust-lang.org)

</div>

---

Wreckless is a fork of [Reckless](https://github.com/codedeliveryservice/Reckless) by Lihan van der
Westhuizen — an open-source engine that competes in the
[Chess.com Computer Chess Championship][ccc] and the [Top Chess Engine Championship][tcec].

**Wreckless inherits effectively all of its playing strength from Reckless, including its NNUE
networks.** It exists to try search ideas against that baseline and to find out, honestly, whether
they work.

[ccc]: https://www.chess.com/computer-chess-championship
[tcec]: https://tcec-chess.com

## A note on how this README is written

Chess engine documentation has a habit of describing every change as an improvement. This one does
not. Sections are graded by what is actually known:

| grade | meaning |
|---|---|
| **Measured** | tested at sufficient sample size, with the confidence interval stated |
| **Proven correct** | a defect fixed, where correctness is demonstrable by reading |
| **Implemented** | written and reasoned about, but never measured |

If a claim does not carry a number, treat it as the third kind.

---

## Contents

- [Status](#status)
- [Quick start](#quick-start)
- [Building](#building)
- [UCI options](#uci-options)
- [Custom commands](#custom-commands)
- [Architecture notes](#architecture-notes)
- [Differences from Reckless](#differences-from-reckless)
- [Testing methodology](#testing-methodology)
- [Tuning](#tuning)
- [Acknowledgements](#acknowledgements)
- [License](#license)

---

## Status

**Measured.** Against tag `0.2.0`, at 10+0.1, one thread, 128 MB hash, UHO book:

```
Wreckless 1.1.3   171.5/345   49.7%   -2.0 Elo  +/- 24.7   +77 -79 =189
Wreckless 0.2.0   173.5/345   50.3%      --         --     +79 -77 =189
```

That is **statistical parity** — the interval spans zero comfortably. Earlier builds in this lineage
measured a real deficit (−17.2 Elo, CI [−30.6, −3.8], p = 0.012, pooled over 324 pairs), which has
since been closed by a series of correctness fixes and reduction-shape corrections.

Two things are worth saying plainly:

1. **Parity is the honest claim.** Not superiority. The added machinery has yet to demonstrate that
   it pays for itself.
2. **The engine grew ~52% more search code than 0.2.0 for that parity** — roughly fifteen additional
   selectivity mechanisms, most of which have never been tuned. That is where any remaining strength
   is most likely to be found; see [Tuning](#tuning).

---

## Quick start

Wreckless speaks [UCI](https://backscattering.de/chess/uci/) and needs a GUI such as
[Cute Chess](https://github.com/cutechess/cutechess), [En Croissant](https://encroissant.org) or
[Nibbler](https://github.com/rooklift/nibbler).

```sh
cargo build --release
# binary at target/release/wreckless (wreckless.exe on Windows)
```

Point your GUI at that binary. The NNUE network is embedded — there is no separate file to install.

---

## Building

**Requirements**

- Rust (2024 edition)
- Clang — only for Syzygy tablebase support, via the bundled [Fathom](https://github.com/jdart1/Fathom)

```sh
cargo build --release                          # standard
cargo build --release --no-default-features    # without Syzygy (no Clang needed)
```

### Profile-guided optimization

PGO is worth roughly 10-20% NPS and is how release binaries should be built. The Makefile drives a
**three-layer profile** — depths 8, 12 and 14 — so the profile covers shallow, middling and deep
search rather than a single operating point.

```sh
make pgo     # instrument -> profile at 3 depths -> optimize -> install
make bolt    # PGO, then BOLT block/function layout on top (Linux/ELF only)
```

<details>
<summary><strong>If your project path contains a space</strong> (common on Windows)</summary>

`cargo-pgo` passes `-Cprofile-generate=<path>` unquoted, so a space in the path makes `rustc` see two
input filenames and fail. Redirect profiles somewhere without spaces:

```sh
export RUSTFLAGS="-C target-cpu=native"
P="/tmp/pgo-wreckless"; B="target/$(rustc --print host-tuple)/release/wreckless"
rm -rf "$P" && mkdir -p "$P"
cargo pgo instrument --profiles-dir "$P"
for a in "128 1 8" "128 1 12" "256 1 14"; do
  LLVM_PROFILE_FILE="$P/wreckless_%m_%p.profraw" ./$B bench $a
done
cargo pgo optimize --profiles-dir "$P"
```

Keep `%m` and `%p` single-`%`. Doubling them is batch-file escaping; under `cmd /c` they stay literal
and all three layers overwrite one file, silently collapsing the profile to a single depth.

</details>

> **BOLT is ELF-only.** `llvm-bolt` cannot process a PE binary, so `make bolt` does nothing useful on
> Windows regardless of whether the tool is installed.

---

## UCI options

| Option | Type | Default | Range |
|---|---|---|---|
| `Hash` | spin | 16 | 1 – 262144 (MB) |
| `Threads` | spin | 1 | 1 – 512 |
| `MoveOverhead` | spin | 100 | 0 – 2000 (ms) |
| `MultiPV` | spin | 1 | 1 – 256 |
| `Ponder` | check | false | |
| `Minimal` | check | false | reduced `info` output |
| `UCI_Chess960` | check | false | |
| `UCI_ShowWDL` | check | false | |
| `Clear Hash` | button | | |
| `SyzygyPath` | string | | supports paths containing spaces |
| `SyzygyProbeDepth` | spin | 1 | 1 – 100 |
| `SyzygyProbeLimit` | spin | 7 | 0 – 7 |

---

## Custom commands

```sh
wreckless bench [hash] [threads] [depth]     # fixed-depth node/NPS benchmark
wreckless speedtest [threads] [hash] [secs]  # sustained NPS measurement
```

`bench` prints a node count that is **deterministic for a given binary and arguments**, which makes
it the fastest way to tell whether a change altered search behaviour at all:

- **identical node count** — behaviour-neutral (refactor, exposed constant, pure speed work)
- **changed node count** — the tree moved; the change needs an SPRT

---

## Architecture notes

### The transposition table

`Cluster` is exactly 32 bytes — three atomic entries plus a packed key word — asserted at compile
time. Two clusters therefore share a 64-byte cache line. Index and verification key are derived from
opposite ends of the hash (multiply-shift high bits for the index, low 16 bits for the key) so they
are independent.

Entry and key are separate atomics, published entry-first with release/acquire ordering. A concurrent
probe can still match a key against a newer payload — the standard benign TT race, and the reason
every TT move is re-validated with `is_legal` before it is played.

### Large pages, where they are real

The transposition table and history tables are allocated on 2 MB pages when the OS grants them:
`MADV_HUGEPAGE` on Linux, `MEM_LARGE_PAGES` on Windows — which requires the `SeLockMemoryPrivilege`
right, **not** granted by default.

The NNUE weights are handled differently, and deliberately: they are copied into large pages **only
if large pages are actually obtained**, otherwise the embedded `&'static` weights are used directly.
The entire value of that copy is the page size — the weights already live in read-only pages of the
mapped executable, which are just as good as regular allocated pages. Copying ~700 KB to obtain the
same page size costs startup time, resident memory and a pointer indirection per evaluation, for
nothing.

---

## Differences from Reckless

### Proven correct

Defects fixed where the correctness argument is demonstrable by reading the code:

- **Null move never restored the ply's own state.** NMP nulled `stack[ply].mv`, `.piece` and both
  continuation pointers and never put them back, so every path after NMP at that node saw a null
  previous move — disabling one singular arm entirely, and leaving continuation history reading from
  and writing to a shared scratch table for the remainder of the node.
- **The extension budget was seeded from a sibling.** `double_extensions` was written only inside the
  move loop, so null-move and both ProbCut recursions handed the child whatever an unrelated subtree
  had left in that slot.
- **Alpha-raise TT writes hardcoded `tt_pv = true`**, marking ordinary cut nodes as PV-line nodes and
  disabling RFP at a growing share of nodes as the table filled.
- **The qsearch move cap counted moves it never searched**, so a node whose captures were all
  delta-pruned could break out having searched nothing and still return a bound.
- **Previous-search TT entries were discarded regardless of depth**, because the depth-preference
  guard required a matching age and `increment_age()` runs at the top of every search.
- **En passant read the wrong square** in the NMP capture guard (`to()` rather than `capture_sq()`).
- **Promotions were classified by the pawn**, so every promotion scored as "not a direct check" and
  took the quiet treatment in LMP, futility and BNFP.
- Plus unchecked allocation failures on two platforms, an unchecked `IndexMut<Square>` on the write
  path, a reachable index underflow in repetition detection, and a lost-`ponderhit` race.

### Measured

Reduction-shape corrections that closed a −17.2 Elo deficit against `0.2.0` to parity. The
significant sub-metric was **defence** — saving inferior positions — at 25.5% versus 35.3%
(p = 0.020, pooled). Two mechanisms fired hardest exactly there:

- a full ply of extra reduction whenever the opponent was winning, and
- a hindsight give-back arm whose bar (2.2 plies of prior reduction) was an order of magnitude
  stricter than the take-away arm's (any reduction at all).

### Implemented, not measured

Everything else. Notably, several mechanisms were **written, gated behind a parameter, and shipped
with that parameter at zero** — where the code comment argues for the branch the default switches
off. Internal iterative reduction on shallow TT entries, futility measured on LMR-scaled depth, and a
depth-scaled RFP improving discount were all in this state.

That pattern is worth watching for in any engine: a condition that is harmlessly redundant under
today's parameter values, and silently wrong under tomorrow's.

---

## Testing methodology

**This is the most useful section in this repository.** If you take one idea from it, take this one.

### Calibrate the harness before trusting it

Run two **identical** binaries against each other and look at the reported Elo. On the setup used
here, over 132 paired games, that A/A test returned:

```
-15.8 Elo, CI [-35.7, +4.0]
```

Two binaries that cannot differ by a single move. That is the noise floor, and every result at that
sample size sits inside it.

### What that implies

| to resolve | pairs | games |
|---|---|---|
| ±30 Elo | ~150 | 300 |
| ±20 Elo | ~340 | 680 |
| ±10 Elo | ~1,350 | 2,700 |
| ±5 Elo | ~5,400 | 10,800 |

Most genuine search improvements are worth 2–8 Elo. **A 100-game match cannot see them.** Three
separate "regressions" during this fork's development dissolved under larger samples, and the one
real regression became visible only after pooling 324 pairs.

### Practical rules

1. **Report the interval, not the point estimate.** "−31 Elo" is not a result; "−31 ± 49" is, and it
   says you do not know.
2. **Use pentanomial (paired-opening) statistics.** Same games, roughly 30% narrower interval, free.
3. **Prefer SPRT** with `elo0=0, elo1=5` — it stops when the evidence is decisive rather than at an
   arbitrary game count.
4. **Change one thing.** A batch that comes back flat tells you nothing about any member of it.
5. **Save every binary under its commit hash before rebuilding.** PGO *moves* rather than copies the
   binary; without a saved reference, the A/B you want tomorrow is impossible.

```sh
cp wreckless.exe "wreckless_$(git rev-parse --short=8 HEAD).exe"
```

---

## Tuning

`spsa.config` exposes **235 parameters** in OpenBench format:

```
name, int, default, min, max, step, learning_rate
```

Roughly a third were hardcoded constants promoted to tunables — move-ordering weights, history
bonus/malus slopes and ceilings, per-sibling decay rates, prior-move credit factors, the escape table
and the aspiration seed. **Ordering and history were previously untuned in their entirety**, while
~150 pruning constants had been tuned repeatedly — and ordering decides which of those pruning
constants ever gets to fire.

Feed the file to an [OpenBench](https://github.com/AndyGrant/OpenBench) SPSA test. Build the tuning
binary with the `spsa` feature so parameters become runtime-settable UCI options:

```sh
cargo build --release --features spsa
```

> In the default build every parameter is a `const fn` that folds at compile time — zero cost, and
> **not settable over UCI**. The tuning binary reads them as runtime statics and is measurably
> slower. Tune with one, play with the other.

**Deliberately not exposed:** the `1024` unit divisors — tuning them is scale drift, which has cost
this fork Elo more than once — and the check-evasion ordering term, which must dominate the learned
terms rather than compete with them.

---

## Acknowledgements

- [Reckless](https://github.com/codedeliveryservice/Reckless) and Lihan van der Westhuizen, for the
  engine this is built on and for its NNUE networks.
- [Fathom](https://github.com/jdart1/Fathom) for Syzygy tablebase probing.
- [OpenBench](https://github.com/AndyGrant/OpenBench) for SPRT and SPSA infrastructure.
- The wider open-source engine community — Stockfish, Viridithas, Stormphrax, PlentyChess — whose
  published reasoning informed several mechanisms here.

## License

[GNU Affero General Public License v3.0](LICENSE). Wreckless is a derivative of Reckless and is
distributed under the same terms.
