<div align="center">
  <h1>Wreckless Chess Engine</h1>

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
</div>

Wreckless is a UCI chess engine, a fork of [Reckless](https://github.com/codedeliveryservice/Reckless)
by Arseniy Surkov, Shahin M. Shahin, and Styx — an open source competitive engine that consistently
performs among the top engines in major tournaments including the
[Chess.com Computer Chess Championship (CCC)][ccc] and [Top Chess Engine Championship (TCEC)][tcec].

[ccc]: https://www.chess.com/computer-chess-championship
[tcec]: https://tcec-chess.com

## Changes relative to upstream Reckless

Search — Stockfish-verified techniques ported per a source-level gap analysis (each pending SPRT
verification):

- **Material-key correction history**: a shared correction-history table keyed by a
  piece-count-only Zobrist key, alongside the existing pawn/non-pawn/continuation tables
- **Minor-piece correction history**: a shared correction-history table keyed by the placement of
  knights, bishops, and kings (as in Stockfish)
- **Low-ply history**: a root-relative `[ply][from][to]` history for plies 0–4, weighted into quiet
  move ordering and carried over between searches (shifted down 2 plies per move)
- **TT-only ProbCut check**: a lower-bound TT entry from a near-full-depth search whose score
  comfortably exceeds beta is trusted as a cutoff without any search (Stockfish's "small ProbCut idea")
- **TT-move reliability history**: a gravity-updated statistic of how often the TT move turns out
  best; feeds back into the singular double-extension margin (Stockfish's `ttMoveHistory`)
- **Shuffling guard on singular extensions**: repetitive piece shuffling near the 50-move rule
  disables singular extensions to prevent search explosions (Stockfish #6447)
- **Improving-by-beta term**: a node also counts as "improving" when its static evaluation already
  clears beta
- **Opponent-worsening term in RFP**: the reverse futility margin shrinks when the evaluation swung
  further in our favor than the opponent's null-move expectation
- **Continuation-history consistency multiplier**: continuation-history updates are scaled up when
  multiple lags already agree (are positive) for the move

An earlier, larger set of speculative search additions (killers, countermoves, one-reply extension,
qsearch futility, volatility-based pruning, entropy time scaling, and others) was removed after SPRT
measured the combined stack at **−69 Elo ± 39** against the baseline.

Speed:

- **PEXT bitboards**: sliding-piece attacks are indexed with the BMI2 `pext` instruction when the
  target supports it (with the classical magic-multiplication path as fallback). Disabled
  automatically on AMD Zen 1/2, where `pext` is microcoded; override with `WRECKLESS_PEXT=0|1`
- **Windows large pages**: the transposition table is allocated with 2 MB pages via
  `VirtualAlloc(MEM_LARGE_PAGES)` when the "Lock pages in memory" privilege is held, reducing TLB
  misses on TT probes (falls back to regular pages otherwise; Linux already used `MADV_HUGEPAGE`)

Protocol / usability:

- **Pondering**: `go ponder` / `ponderhit` support and `bestmove ... ponder ...` output
- **`searchmoves`**: root move filtering on the `go` command
- **`UCI_ShowWDL`**: win/draw/loss estimates in `info` lines
- **`SyzygyProbeDepth` / `SyzygyProbeLimit`**: user-tunable tablebase engagement
- **SPSA tunables**: 92 search constants (LMR, LMP, FP, RFP, NMP, ProbCut, SEE, correction history)
  exposed as UCI options under the `spsa` cargo feature for OpenBench SPSA tuning; identical
  compiled code in default builds. A ready-to-use OpenBench SPSA input file is provided in
  [`spsa.config`](spsa.config)

## Getting started

### Building from source

To build Wreckless from source, make sure you have:

- `Rust 1.88.0` or a later version installed ([official Rust installation guide](https://www.rust-lang.org/tools/install))
- `Clang` installed (required for building the [Fathom](https://github.com/jdart1/Fathom) library used for Syzygy endgame tablebase support)

Once installed, you can build it with:

```bash
make
# ./wreckless
```

To build without Syzygy tablebase support and Clang dependency:

```bash
make no-syzygy
# ./wreckless
```

#### PGO builds

For profile-guided optimization (PGO) builds, you need to install additional tools:

```bash
rustup component add llvm-tools
cargo install cargo-pgo
```

Then, you can build the engine using `make`:

```bash
make pgo
# ./wreckless
```

Or run the steps manually:

```bash
cargo pgo instrument
cargo pgo run -- bench
cargo pgo optimize
# ./target/x86_64-unknown-linux-gnu/release/wreckless
# (the path may vary based on your system)
```

### Usage

Wreckless is not a standalone chess program but a chess engine designed for use with UCI-compatible GUIs,
such as [Cute Chess](https://github.com/cutechess/cutechess), [En Croissant](https://encroissant.org),
or [Nibbler](https://github.com/rooklift/nibbler).

### UCI options

Wreckless supports the following UCI options:

| Name         | Default | Description                                                           |
| ------------ | ------- | --------------------------------------------------------------------- |
| Hash         | 16      | Size of the transposition table in MB [1–262144]                      |
| Threads      | 1       | Number of search threads [1–512]                                      |
| MultiPV      | 1       | Number of principal variations to display [1–218]                     |
| Ponder       | false   | Allow the GUI to let the engine think on the opponent's time          |
| UCI_Chess960 | false   | Enable Chess960 (Fischer Random) support [false–true]                 |
| UCI_ShowWDL  | false   | Show win/draw/loss estimates in search output [false–true]            |
| Minimal      | false   | Enable minimal UCI output [false–true]                                |
| MoveOverhead | 100     | Time in milliseconds reserved for overhead during each move [0–2000]  |
| Clear Hash   | —       | Clear the transposition table                                         |
| SyzygyPath   | —       | Path to Syzygy endgame tablebases                                     |
| SyzygyProbeDepth | 1   | Minimum depth to probe tablebases at the piece-count boundary [1–100] |
| SyzygyProbeLimit | 7   | Maximum number of pieces for tablebase probes [0–7]                   |

### Custom commands

Along with the standard UCI commands, Wreckless supports additional commands for testing and debugging:

| Command                                | Description                                                                        |
| -------------------------------------- | ---------------------------------------------------------------------------------- |
| `perft <depth>`                        | Run a [perft][perft] test to count the number of leaf nodes at a given depth       |
| `bench`                                | Run a [benchmark][bench] on a set of positions to measure the engine's performance |
| `d`                                    | Print the current board position in a human-readable format together with FEN      |
| `eval`                                 | Print the network evaluation of the current position from white's perspective      |
| `compiler`                             | Print the compiler version, target and flags used to compile the engine            |
| `speedtest <Threads> <Hash> <Seconds>` | Runs a performance test across 50 positions                                        |

[perft]: https://www.chessprogramming.org/Perft
[bench]: /src/tools/bench.rs

## Testing

All search changes listed above are unverified until they pass game testing. The expected workflow:

### SPRT (patch verification)

Build the test and base binaries (e.g. via `git worktree add ../wreckless-base <commit> && make` in
each), then run a sequential probability ratio test with [fastchess](https://github.com/Disservin/fastchess)
and a standard opening book such as `UHO_Lichess_4852_v1.epd`:

```bash
fastchess -engine cmd=wreckless name=test -engine cmd=wreckless-base name=base \
  -each tc=10+0.1 option.Hash=16 option.Threads=1 proto=uci \
  -openings file=UHO_Lichess_4852_v1.epd format=epd order=random \
  -repeat -games 2 -rounds 30000 -concurrency 8 -recover \
  -sprt elo0=0 elo1=5 alpha=0.05 beta=0.05 -ratinginterval 200
```

The run stops on its own: H1 accepted means the patch gains Elo; H0 accepted means it does not.
Test one patch per branch. Always run games with the default build — the `spsa` feature build is
slightly slower and only meant for tuning.

### SPSA (parameter tuning)

Build with `cargo rustc --release --bin wreckless --features spsa` to expose all 92 tunables as UCI
options, then feed [`spsa.config`](spsa.config) to an [OpenBench](https://github.com/AndyGrant/OpenBench)
SPSA test (preferred, needs distributed workers), or tune one parameter group at a time locally with
a cutechess-based SPSA driver. Paste tuned values back into `src/parameters.rs` and SPRT the result
before keeping it.

## Acknowledgements

- [Reckless](https://github.com/codedeliveryservice/Reckless) and its
  [contributors](https://github.com/codedeliveryservice/Reckless/graphs/contributors) — Wreckless is a
  fork and inherits virtually all of its strength from their work, including the NNUE networks from
  [RecklessNetworks](https://github.com/codedeliveryservice/RecklessNetworks)
- [OpenBench](https://github.com/AndyGrant/OpenBench) is the primary testing framework powered by [Cute Chess](https://github.com/cutechess/cutechess)
- [Bullet](https://github.com/jw1912/bullet) is the NNUE trainer
- [Stockfish](https://github.com/official-stockfish/Stockfish), [PlentyChess](https://github.com/Yoshie2000/PlentyChess), [Ethereal](https://github.com/AndyGrant/Ethereal), [Berserk](https://github.com/jhonnold/berserk), and many other open source chess engines
- [Chess Programming Wiki](https://www.chessprogramming.org/Main_Page)

## License

This project is licensed under the [GNU Affero General Public License v3.0](LICENSE).
