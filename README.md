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

Search (each pending SPRT verification — treat as experimental until tested):

- **Material-key correction history**: a fourth shared correction-history table keyed by a
  piece-count-only Zobrist key, alongside the existing pawn/non-pawn/continuation tables
- **Low-ply history**: a root-relative `[ply][from][to]` history for plies 0–4, weighted into quiet
  move ordering and carried over between searches (shifted down 2 plies per move)
- **Killer moves**: two killer-move slots per ply; quiet moves that cause a beta cutoff are stored
  and scored highly in move ordering (+31 000 / +22 000) at the same ply in future iterations
- **Countermove heuristic**: a `[piece][to]` table records the quiet refutation of the opponent's
  last move; matched moves receive a +13 000 ordering bonus
- **Internal Iterative Reductions (IIR)**: when a node at depth ≥ 3 has no TT move to anchor
  ordering, depth is reduced by 1 to quickly populate the TT before the full re-search
- **Check extension**: a move that delivers a direct check at the leaf (new\_depth == 0) is
  extended to depth 1 rather than falling immediately into qsearch, preventing horizon-effect
  oversights in forced tactical sequences
- **History decay**: quiet, noisy, and pawn history tables are halved at the start of each new
  search so stale ordering data from previous positions does not bias the current search
- **Aspiration window floor**: the initial aspiration delta is clamped to a minimum of 10 cp,
  preventing hairline windows in very stable positions that cause excessive re-searches

Speed:

- **PEXT bitboards**: sliding-piece attacks are indexed with the BMI2 `pext` instruction when the
  target supports it (with the classical magic-multiplication path as fallback). Disabled
  automatically on AMD Zen 1/2, where `pext` is microcoded; override with `WRECKLESS_PEXT=0|1`

Protocol / usability:

- **Pondering**: `go ponder` / `ponderhit` support and `bestmove ... ponder ...` output
- **`searchmoves`**: root move filtering on the `go` command
- **`UCI_ShowWDL`**: win/draw/loss estimates in `info` lines
- **SPSA tunables**: ~90 search constants (LMR, LMP, FP, RFP, NMP, ProbCut, SEE, correction history)
  exposed as UCI options under the `spsa` cargo feature for OpenBench SPSA tuning; identical
  compiled code in default builds

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
