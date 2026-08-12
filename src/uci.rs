use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;

use crate::{
    board::{Board, NullBoardObserver},
    search::Report,
    thread::{SharedContext, Status, ThreadData},
    threadpool::ThreadPool,
    time::{Limits, TimeManager},
    tools,
    transposition::DEFAULT_TT_SIZE,
    types::{Color, MAX_MOVES, MAX_PLY, Move, Piece, Score, Square, is_decisive, is_loss, is_win},
};

#[derive(Copy, Clone, PartialEq, Eq)]
enum Mode {
    Cli,
    Uci,
}

struct Settings {
    frc: bool,
    multi_pv: usize,
    move_overhead: u64,
    report: Report,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            frc: false,
            multi_pv: 1,
            move_overhead: 100,
            report: Report::Full,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn message_loop(mut buffer: VecDeque<String>) {
    let shared = Arc::new(SharedContext::default());
    let mut settings = Settings::default();
    let mut threads = ThreadPool::new(shared.clone());
    let mut board = Board::starting_position();

    let rx = spawn_listener(shared.clone());

    let mut mode = if buffer.is_empty() { Mode::Uci } else { Mode::Cli };

    loop {
        let message = if let Some(cmd) = buffer.pop_front() {
            cmd
        } else if mode == Mode::Uci {
            match rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => break,
            }
        } else {
            break;
        };

        let tokens = message.split_whitespace().collect::<Vec<_>>();
        match tokens.as_slice() {
            ["uci"] => {
                uci();
                mode = Mode::Uci;
            }

            ["isready"] => println!("readyok"),

            ["go", tokens @ ..] => go(&mut threads, &settings, &board, &shared, tokens),
            ["position", tokens @ ..] => position(&mut board, &settings, tokens),
            ["setoption", tokens @ ..] => set_option(&mut threads, &mut settings, &shared, tokens),
            ["ucinewgame"] => reset(&mut threads, &shared),

            ["stop"] => {
                shared.ponder.store(false, std::sync::atomic::Ordering::Release);
                shared.stop_pending.store(true, std::sync::atomic::Ordering::Release);
                shared.status.set(Status::STOPPED);
            }
            // Unreachable, and deliberately so. The listener thread consumes
            // `ponderhit` itself (it must, to stamp `ponderhit_time` while the
            // search is still running) and never forwards it, so this arm exists
            // only to stop the token falling through to `Unknown command`.
            ["ponderhit"] => (),
            ["quit"] => {
                drop(threads);
                break;
            }

            // Non-UCI commands
            ["compiler"] => compiler(),
            ["eval"] => eval(threads.main_thread(), &board),
            ["d"] => println!("{board}"),
            ["bench", args @ ..] => match mode {
                Mode::Uci => tools::bench::<true>(args),
                Mode::Cli => tools::bench::<false>(args),
            },
            ["speedtest", args @ ..] => tools::speedtest(args),
            ["perft", depth] => tools::perft(depth.parse().unwrap(), &mut board),
            ["perft"] => eprintln!("Usage: perft <depth>"),
            ["simpleperft", depth] => tools::simple_perft(depth.parse().unwrap(), &mut board),
            ["simpleperft"] => eprintln!("Usage: simpleperft <depth>"),
            ["islegalperft", depth] => tools::is_legal_perft(depth.parse().unwrap(), &mut board),
            ["islegalperft"] => eprintln!("Usage: islegalperft <depth>"),

            // Ignore empty lines
            [] => (),

            _ => eprintln!("Unknown command: '{}'", message.trim_end()),
        }

        // Auto-exit after last CLI command
        if matches!(mode, Mode::Cli) && buffer.is_empty() {
            drop(threads);
            break;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_listener(shared: Arc<SharedContext>) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        loop {
            let mut message = String::new();

            // A read error is treated exactly like EOF. `unwrap()` here would
            // panic the reader thread on a broken pipe or a non-UTF-8 byte,
            // and that panic skips the shutdown below -- leaving the search
            // running with no reader left to ever stop it, the same zombie the
            // EOF branch exists to prevent.
            if std::io::stdin().read_line(&mut message).unwrap_or(0) == 0 {
                // EOF: no further command can ever arrive, so stop any active
                // search (mirroring the "quit" arm below) and queue the quit.
                // Without clearing `ponder` here, a `go ponder` search in
                // flight when the pipe closes never sees ponder go false and
                // never sees STOPPED -- soft_limit/check_time both special-case
                // ponder to never fire, so the search runs to MAX_PLY and the
                // ponder-wait loop in go() then spins forever afterward,
                // leaving a 100%-CPU zombie that never dequeues this quit.
                shared.ponder.store(false, std::sync::atomic::Ordering::Release);
                shared.stop_pending.store(true, std::sync::atomic::Ordering::Release);
                shared.status.set(Status::STOPPED);
                let _ = tx.send("quit".to_string());
                break;
            }

            match message.trim_end() {
                "isready" => println!("readyok"),
                "stop" => {
                    shared.ponder.store(false, std::sync::atomic::Ordering::Release);
                    shared.stop_pending.store(true, std::sync::atomic::Ordering::Release);
                    shared.status.set(Status::STOPPED);
                }
                "ponderhit" => {
                    *shared.ponderhit_time.lock().unwrap() = Some(std::time::Instant::now());
                    shared.ponder.store(false, std::sync::atomic::Ordering::Release);
                }
                "quit" => {
                    shared.ponder.store(false, std::sync::atomic::Ordering::Release);
                    shared.stop_pending.store(true, std::sync::atomic::Ordering::Release);
                    shared.status.set(Status::STOPPED);
                    let _ = tx.send("quit".to_string());
                    break;
                }
                _ => {
                    // A pondering search never terminates on its own: both
                    // `soft_limit` and `check_time` return false outright while
                    // `ponder` is set, so it runs to MAX_PLY. It ends only when
                    // something clears the flag.
                    //
                    // The spec says the GUI sends `stop` before moving on, and
                    // when *we* are the one being waited for it does. But when
                    // the OPPONENT flags or crashes, the game is simply over
                    // from the harness's point of view -- it was not waiting on
                    // our move, so it has no reason to send `stop`, and it goes
                    // straight to setting up the next game.
                    //
                    // Those commands then hit the `RUNNING` test below and were
                    // dropped, `go()` stayed blocked in its ponder-wait loop,
                    // and every later command was dropped for the same reason.
                    // Because `isready` is answered from this thread, the GUI
                    // saw a live, responsive engine that never replied to
                    // `position`/`go` -- so it timed out and reported a
                    // disconnect rather than a hang.
                    //
                    // Any command that means the game moved on therefore ends a
                    // ponder search first. This is narrowly scoped to pondering:
                    // a real search has a result someone is waiting for, and the
                    // silent-ignore rule below still applies to it.
                    let command = message.split_whitespace().next().unwrap_or_default();

                    // Tests `ponder_search`, not `ponder`. `ponderhit` clears
                    // `ponder` while the search keeps running, and a game that
                    // ends in *that* window -- the opponent flags, or is mated,
                    // while we are converting a ponder into a real search --
                    // left the command dropped exactly as before.
                    //
                    // That window is why this only reproduces with both engines
                    // pondering: with one side pondering the engine is idle
                    // between its own moves, so a game ending lands in the idle
                    // gap and everything is forwarded normally. With both, there
                    // is no idle gap -- the engine is inside a search whenever
                    // the game can end.
                    if matches!(command, "ucinewgame" | "position" | "go")
                        && shared.ponder_search.load(std::sync::atomic::Ordering::Acquire)
                    {
                        // Mark it abandoned BEFORE releasing the search, so
                        // `go()` cannot reach its `bestmove` between the two
                        // stores and emit the very reply this prevents.
                        shared.ponder_abandoned.store(true, std::sync::atomic::Ordering::Release);
                        shared.ponder.store(false, std::sync::atomic::Ordering::Release);
                        shared.stop_pending.store(true, std::sync::atomic::Ordering::Release);
                        shared.status.set(Status::STOPPED);
                    }

                    // According to the UCI specs, commands that are unexpected
                    // in the current state should be ignored silently.
                    // (https://backscattering.de/chess/uci/#unexpected)
                    //
                    // "Silently" is the spec's word and it is kept, but note the
                    // consequence: a `setoption` sent during a search is dropped
                    // with no diagnostic, so a GUI that reconfigures Hash or
                    // Threads mid-search sees no error and no effect. Reporting
                    // it here would mean printing from the listener thread while
                    // the search is emitting `info` lines, so it is left alone.
                    if shared.status.get() != Status::RUNNING {
                        let _ = tx.send(message);
                    }
                }
            }
        }
    });

    rx
}

fn uci() {
    println!("id name Wreckless {}", env!("ENGINE_VERSION"));
    println!("id author Arseniy Surkov, Shahin M. Shahin, and Styx");
    println!("option name Hash type spin default {DEFAULT_TT_SIZE} min 1 max 262144");
    println!("option name Threads type spin default 1 min 1 max {}", ThreadPool::available_threads());
    println!("option name MoveOverhead type spin default 100 min 0 max 2000");
    println!("option name Minimal type check default false");
    println!("option name Clear Hash type button");
    println!("option name UCI_Chess960 type check default false");
    println!("option name UCI_ShowWDL type check default false");
    println!("option name Ponder type check default false");
    println!("option name MultiPV type spin default 1 min 1 max {MAX_MOVES}");

    #[cfg(feature = "syzygy")]
    println!("option name SyzygyPath type string default");
    #[cfg(feature = "syzygy")]
    println!("option name SyzygyProbeDepth type spin default 1 min 1 max 100");
    #[cfg(feature = "syzygy")]
    println!("option name SyzygyProbeLimit type spin default 7 min 0 max 7");

    #[cfg(feature = "spsa")]
    crate::parameters::print_options();

    println!("uciok");
}

fn compiler() {
    println!("Compiler Version: {}", env!("COMPILER_VERSION"));
    println!("Compiler Target: {}", env!("COMPILER_TARGET"));
    println!("Compiler Features: {}", env!("COMPILER_FEATURES"));
}

fn reset(threads: &mut ThreadPool, shared: &Arc<SharedContext>) {
    threads.clear();
    shared.tt.clear(threads.len());

    for corrhist in shared.history.all() {
        corrhist.pawn.clear();
        corrhist.non_pawn[Color::White].clear();
        corrhist.non_pawn[Color::Black].clear();
        corrhist.material.clear();
        corrhist.pawn_history.clear();
    }
}

fn go(threads: &mut ThreadPool, settings: &Settings, board: &Board, shared: &Arc<SharedContext>, tokens: &[&str]) {
    let mut ponder = false;
    let mut search_moves = Vec::new();
    let mut limit_tokens = Vec::new();

    let legal_moves = board.generate_all_moves();
    let find_move = |uci: &str| legal_moves.iter().map(|entry| entry.mv).find(|mv| mv.to_uci(board) == uci);

    let mut index = 0;
    while index < tokens.len() {
        match tokens[index] {
            "ponder" => ponder = true,
            "searchmoves" => {
                while index + 1 < tokens.len() {
                    match find_move(tokens[index + 1]) {
                        Some(mv) => {
                            search_moves.push(mv);
                            index += 1;
                        }
                        None => break,
                    }
                }
            }
            token => limit_tokens.push(token),
        }
        index += 1;
    }

    let limits = parse_limits(board.side_to_move(), &limit_tokens);
    let time_manager = TimeManager::new(limits, board.fullmove_number(), settings.move_overhead);

    // Discards a `stop` received while idle: it referred to no search and must
    // not kill this one. Anything arriving from here on is for this search and
    // survives the RUNNING store -- see `SharedContext::begin_search`.
    shared.arm_search();

    shared.ponder_abandoned.store(false, std::sync::atomic::Ordering::Release);
    shared.ponder_search.store(ponder, std::sync::atomic::Ordering::Release);
    shared.ponder.store(ponder, std::sync::atomic::Ordering::Release);

    // A `ponderhit` can land between `arm_search()` and the store above. The
    // listener thread handles it inline -- it does not wait for RUNNING -- so it
    // clears a `ponder` flag that is not set yet, and the store above then sets
    // it, losing the hit. The search would ponder forever on a position the GUI
    // has already committed to, and only the eventual `stop` would end it, at
    // which point `bestmove` arrives far too late.
    //
    // The window is small but reachable: the GUI sends `ponderhit` as soon as
    // the opponent plays the move we predicted, which can be immediately.
    //
    // The stamp is cleared at the END of the previous search, not here. Clearing
    // it at the top of `go()` looked equivalent and was not: the listener can
    // stamp before the main thread even reaches that line -- it forwards
    // `go ponder` and then reads `ponderhit` off the very next line, while the
    // main thread is still dequeuing -- and the clear would then erase a hit
    // belonging to THIS search, after which the store above re-set `ponder`.
    //
    // Clearing on the way out instead means the slot is already `None` when a
    // search begins, without `go()` having to win a race to make it so. Any
    // `Some` observed here therefore belongs to this search no matter which side
    // of the store it landed on.
    if ponder && shared.ponderhit_time.lock().unwrap().is_some() {
        shared.ponder.store(false, std::sync::atomic::Ordering::Release);
    }

    threads.execute_searches_filtered(time_manager, settings.report, settings.multi_pv, board, shared, &search_moves);

    // If the search ended while still pondering, the UCI protocol requires
    // waiting for `ponderhit` or `stop` before announcing the best move.
    while shared.ponder.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    // A ponder search torn down because the game moved on has no reply to give.
    // UCI requires a `bestmove` after `stop`, and only after `stop` -- the GUI
    // that sent `position`/`go` instead is not waiting for one, and is about to
    // read the next line we print as the answer to its `go`. That answer would
    // be a move for the *pondered* position: illegal in the new one about half
    // the time, and silently wrong the rest. Emitting it desynchronises the
    // stream by exactly one `bestmove` for the remainder of the game, which is
    // what an arbiter reports as an illegal move or a disconnect.
    let abandoned = shared.ponder_abandoned.swap(false, std::sync::atomic::Ordering::AcqRel);
    shared.ponder_search.store(false, std::sync::atomic::Ordering::Release);

    // Cleared here rather than at the top of the next `go()`; see the note at the
    // `ponder` store above. The search is over, so `TimeManager::search_elapsed`
    // has no further use for it, and leaving it set would make the next search
    // measure its own elapsed time from this search's `ponderhit`.
    *shared.ponderhit_time.lock().unwrap() = None;

    if abandoned {
        return;
    }

    // Every thread is indexed at `root_moves[0]` below, so the emptiness check
    // has to cover all of them, not just thread 0. Today they always agree --
    // `searchmoves` applies the same `retain` to each thread from one list --
    // but that is an invariant of the spawn path, not of this function, and an
    // out-of-bounds index here would take down a search that had already found
    // its move. Checking `any` instead of `threads[0]` makes the guard match
    // what the code actually relies on.
    if threads.iter().any(|t| t.root_moves.is_empty()) {
        println!("bestmove (none)");
        return;
    }

    let min_score = threads.iter().map(|v| v.root_moves[0].score).min().unwrap();
    let vote_value = |td: &ThreadData| (td.root_moves[0].score - min_score + 10) * td.completed_depth;

    let mut votes: HashMap<&Move, i32> = HashMap::new();
    for result in threads.iter() {
        *votes.entry(&result.root_moves[0].mv).or_default() += vote_value(result);
    }

    let mut best = 0;

    if !matches!(threads[best].time_manager.limits(), Limits::Depth(_)) && threads[0].multi_pv == 1 {
        for current in 1..threads.len() {
            let is_better_candidate = || -> bool {
                let best = &threads[best];
                let current = &threads[current];

                if is_win(best.root_moves[0].score) {
                    return current.root_moves[0].score > best.root_moves[0].score;
                }

                if current.root_moves[0].score != -Score::INFINITE
                    && best.root_moves[0].score != -Score::INFINITE
                    && is_loss(best.root_moves[0].score)
                {
                    return current.root_moves[0].score < best.root_moves[0].score;
                }

                if current.root_moves[0].score != -Score::INFINITE && is_decisive(current.root_moves[0].score) {
                    return true;
                }

                let best_vote = votes[&best.root_moves[0].mv];
                let current_vote = votes[&current.root_moves[0].mv];

                !is_loss(current.root_moves[0].score)
                    && (current_vote > best_vote
                        || (current_vote == best_vote && vote_value(current) > vote_value(best)))
            };

            if is_better_candidate() {
                best = current;
            }
        }
    }

    if best != 0 {
        let depth = threads[best].completed_depth;
        threads[best].print_uci_info(depth);
    }

    let best_root_move = &threads[best].root_moves[0];
    let mut line = format!("bestmove {}", best_root_move.mv.to_uci(board));

    if let Some(&ponder_move) = best_root_move.pv.line().first() {
        let mut after_best = board.clone();
        after_best.make_move(best_root_move.mv, &mut NullBoardObserver);
        line.push_str(&format!(" ponder {}", ponder_move.to_uci(&after_best)));
    }

    println!("{line}");
    crate::misc::dbg_print();
}

fn position(board: &mut Board, settings: &Settings, mut tokens: &[&str]) {
    while !tokens.is_empty() {
        match tokens {
            ["startpos", rest @ ..] => {
                *board = Board::starting_position();
                // Applied here too, not just in the `fen` branch: without it a
                // Chess960 game reaching the board via `position startpos`
                // emits castling moves in standard notation rather than the
                // king-takes-rook notation UCI_Chess960 requires.
                board.set_frc(settings.frc);
                tokens = rest;
            }
            ["fen", rest @ ..] => {
                match Board::from_fen(&rest.join(" ")) {
                    Ok(b) => *board = b,
                    // On stdout, for the same reason as `make_uci_move` above:
                    // a rejected FEN leaves the PREVIOUS position in place and
                    // the engine then searches it, so this needs to be visible
                    // where the GUI is actually reading.
                    Err(e) => println!("info string Invalid FEN ({e:?}) -- keeping previous position"),
                }
                board.set_frc(settings.frc);
                tokens = rest;
            }
            ["moves", rest @ ..] => {
                for uci_move in rest {
                    make_uci_move(board, uci_move);
                }
                break;
            }
            _ => tokens = &tokens[1..],
        }
    }
}

fn make_uci_move(board: &mut Board, uci_move: &str) {
    let moves = board.generate_all_moves();

    match moves.iter().map(|entry| entry.mv).find(|mv| mv.to_uci(board) == uci_move) {
        Some(mv) => board.make_move(mv, &mut NullBoardObserver),
        // Reported, not swallowed. A move the engine cannot match is either a
        // GUI/engine disagreement about notation (Chess960 castling is the usual
        // culprit) or a movegen bug -- and either way the board silently stops
        // matching the GUI's for the rest of the game, with every later move in
        // the list applied to the wrong position. `info string` rather than
        // stderr, because a GUI reading stdout never sees stderr.
        None => println!("info string Ignored unrecognised move '{uci_move}' -- position may be out of sync"),
    }
}

/// Parses a UCI `check` value, accepting the spellings GUIs actually send.
///
/// Returns `None` and reports on an unrecognised value rather than defaulting to
/// `false`, so a typo cannot look like a successful configuration change.
fn parse_check(name: &str, value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => {
            println!("info string Invalid value '{value}' for {name}, expected true or false");
            None
        }
    }
}

fn set_option(threads: &mut ThreadPool, settings: &mut Settings, shared: &Arc<SharedContext>, tokens: &[&str]) {
    match tokens {
        ["name", "Minimal", "value", v] => match *v {
            "true" => settings.report = Report::Minimal,
            "false" => settings.report = Report::Full,
            _ => eprintln!("Invalid value: '{v}'"),
        },
        ["name", "Clear", "Hash"] => {
            shared.tt.clear(threads.len());
            println!("info string Hash cleared");
        }
        ["name", "Hash", "value", v] => {
            // Must be clamped, not just parsed: a 0 MB table has zero clusters,
            // and every probe would index into a zero-length (on Windows, null)
            // allocation.
            if let Some(megabytes) = parse_spin("Hash", v, 1, 262_144) {
                shared.tt.resize(threads.len(), megabytes);
                println!("info string set Hash to {megabytes} MB");
            }
        }
        ["name", "Threads", "value", v] => {
            if let Some(count) = parse_spin("Threads", v, 1, ThreadPool::available_threads()) {
                threads.set_count(count);
                println!("info string set Threads to {}", threads.len());
            }
        }
        ["name", "MoveOverhead", "value", v] => {
            if let Some(overhead) = parse_spin("MoveOverhead", v, 0, 2000) {
                settings.move_overhead = overhead;
                println!("info string set MoveOverhead to {overhead} ms");
            }
        }
        #[cfg(feature = "syzygy")]
        // `rest @ ..`, not a single token. UCI option values may contain spaces
        // and tablebase paths routinely do -- `value C:\Program Files\syzygy`
        // is three tokens, so the single-token pattern did not match at all and
        // the command fell through to the silent unknown-option arm. Tablebases
        // then stayed disabled with no diagnostic, which on Windows is the
        // common case rather than an edge one.
        //
        // The failure branch also prints to stdout as an `info string` now: it
        // was on stderr, which a GUI reading the engine's stdout never shows, so
        // a mistyped path looked identical to a working one.
        ["name", "SyzygyPath", "value", rest @ ..] if !rest.is_empty() => {
            let path = rest.join(" ");
            match crate::tb::initialize(&path) {
                Some(size) => println!("info string Loaded Syzygy tablebases with {size} pieces"),
                None => println!("info string Failed to load Syzygy tablebases from '{path}'"),
            }
        }
        #[cfg(feature = "syzygy")]
        ["name", "SyzygyProbeDepth", "value", v] => {
            if let Some(depth) = parse_spin("SyzygyProbeDepth", v, 1, 100) {
                shared.syzygy_probe_depth.store(depth, std::sync::atomic::Ordering::Relaxed);
                println!("info string set SyzygyProbeDepth to {depth}");
            }
        }
        #[cfg(feature = "syzygy")]
        ["name", "SyzygyProbeLimit", "value", v] => {
            if let Some(limit) = parse_spin("SyzygyProbeLimit", v, 0, 7) {
                shared.syzygy_probe_limit.store(limit, std::sync::atomic::Ordering::Relaxed);
                println!("info string set SyzygyProbeLimit to {limit}");
            }
        }
        // Parsed, not `unwrap_or_default()`. That silently mapped any
        // unparseable value to `false` while printing "set ... to <garbage>", so
        // a GUI sending `value True` or `value 1` -- both seen in the wild --
        // was told the option had been set and got the opposite. `MultiPV`
        // below already got this treatment; these two were missed.
        //
        // Chess960 in particular is not a cosmetic setting: with it wrongly
        // false the engine emits castling in standard notation and the GUI
        // rejects the move.
        ["name", "UCI_Chess960", "value", v] => match parse_check("UCI_Chess960", v) {
            Some(value) => {
                settings.frc = value;
                println!("info string set UCI_Chess960 to {value}");
            }
            None => (),
        },
        ["name", "UCI_ShowWDL", "value", v] => match parse_check("UCI_ShowWDL", v) {
            Some(value) => {
                shared.show_wdl.store(value, std::sync::atomic::Ordering::Release);
                println!("info string set UCI_ShowWDL to {value}");
            }
            None => (),
        },
        ["name", "Ponder", "value", _] => {
            // The GUI only announces that it may send `go ponder`; nothing to configure.
        }
        ["name", "MultiPV", "value", v] => {
            // unwrap_or_default() here meant any unparseable value (or a literal
            // "0") silently set MultiPV to 0, which makes the root loop
            // `for index in 0..td.multi_pv` search nothing at all -- iterative
            // deepening would spin through every depth instantly and the engine
            // would answer with an unsearched, effectively random root move.
            if let Some(count) = parse_spin("MultiPV", v, 1, MAX_MOVES) {
                settings.multi_pv = count;
                println!("info string set MultiPV to {count}");
            }
        }
        #[cfg(feature = "spsa")]
        ["name", name, "value", v] => {
            crate::parameters::set_parameter(name, v);
            println!("info string set {name} to {v}");
        }
        _ => eprintln!("Unknown option: '{}'", tokens.join(" ").trim_end()),
    }
}

/// Parses a `spin` option value and clamps it to the range advertised in the
/// `uci` handshake, reporting (rather than panicking on) a malformed value.
/// A GUI sending a value the engine never offered should not be able to take
/// the process down in the middle of a game.
fn parse_spin<T>(name: &str, value: &str, min: T, max: T) -> Option<T>
where
    T: std::str::FromStr + Ord,
{
    match value.parse::<T>() {
        Ok(parsed) => Some(parsed.clamp(min, max)),
        Err(_) => {
            eprintln!("Invalid value for option '{name}': '{value}'");
            None
        }
    }
}

fn eval(td: &mut ThreadData, board: &Board) {
    td.nnue.full_refresh(board);
    td.nnue.evaluate(board);

    let side = board.side_to_move();

    println!("NNUE derived piece values");
    println!("+-------+-------+-------+-------+-------+-------+-------+-------+");
    for rank in (0..8).rev() {
        print!("|");
        for file in 0..8 {
            let sq = Square::from_rank_file(rank, file);
            let piece = board.piece_on(sq);
            let piece_str = if piece == Piece::None { " ".to_string() } else { piece.to_string() };
            print!("  {piece_str:^3}  |");
        }
        println!();

        print!("|");
        for file in 0..8 {
            let sq = Square::from_rank_file(rank, file);
            match td.nnue.piece_contribution(board, sq) {
                None => print!("       |"),
                Some(v) => {
                    let val = v as f32 / 100.0;
                    print!("{val:+6.2} |");
                }
            }
        }
        println!();
        println!("+-------+-------+-------+-------+-------+-------+-------+-------+");
    }

    let used_bucket = crate::nnue::OUTPUT_BUCKETS_LAYOUT[board.occupancies().popcount()];

    println!("\nNNUE output buckets (White side)");
    println!("+------------+------------+");
    println!("|   Bucket   |   Total    |");
    println!("+------------+------------+");

    for bucket in 0..8 {
        let raw_score = td.nnue.eval_with_bucket(board, bucket);
        let white_score = if side == Color::White { raw_score } else { -raw_score };
        let total = white_score as f32 / 100.0;

        if bucket == used_bucket {
            println!("|  {bucket:<2}        | {total:+7.2}    | <-- this bucket is used");
        } else {
            println!("|  {bucket:<2}        | {total:+7.2}    |");
        }
    }
    println!("+------------+------------+");

    let final_eval = td.nnue.evaluate(board);
    let final_total = (if side == Color::White { final_eval } else { -final_eval }) as f32 / 100.0;
    println!("\nNNUE evaluation        {final_total:+.2} (White side)");
}

fn parse_limits(color: Color, tokens: &[&str]) -> Limits {
    if let ["infinite"] = tokens {
        return Limits::Infinite;
    }

    let mut main = None;
    let mut inc = None;
    let mut moves = None;

    for chunk in tokens.chunks(2) {
        if let [name, value] = *chunk {
            let Ok(value) = value.parse::<u64>() else {
                continue;
            };

            match name {
                // Clamped before the cast. `value as i32` truncates: `go depth
                // 2147483648` lands on i32::MIN, and the iterative-deepening
                // loop's `depth > maximum` is then true on the first iteration,
                // so the search breaks before running and `bestmove` reports a
                // move that was never searched. `4294967296` gives 0 and does
                // the same. MAX_PLY is the deepest iteration the loop can reach
                // anyway, so clamping there loses nothing.
                "depth" if value > 0 => return Limits::Depth(value.min(MAX_PLY as u64) as i32),
                "movetime" if value > 0 => return Limits::Time(value),
                "nodes" if value > 0 => return Limits::Nodes(value),
                "mate" if value > 0 => return Limits::Mate(value),

                "wtime" if Color::White == color => main = Some(value),
                "btime" if Color::Black == color => main = Some(value),
                "winc" if Color::White == color => inc = Some(value),
                "binc" if Color::Black == color => inc = Some(value),
                // `movestogo 0` is sent by some GUIs to mean "sudden death".
                // Taken literally it divides the remaining clock by zero in
                // TimeManager (Limits::Cyclic), producing an infinite per-move
                // allocation that collapses to "spend the entire clock on this
                // move". Treat it as the absence of a move counter instead.
                "movestogo" if value > 0 => moves = Some(value),

                _ => continue,
            }
        }
    }

    if main.is_none() && inc.is_none() {
        return Limits::Infinite;
    }

    let main = main.unwrap_or_default();
    let inc = inc.unwrap_or_default();

    match moves {
        Some(moves) => Limits::Cyclic(main, inc, moves),
        None => Limits::Fischer(main, inc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_position_helper(tokens: &[&str]) -> Board {
        let settings = Settings::default();
        let mut board = Board::starting_position();

        position(&mut board, &settings, tokens);
        board.clone()
    }

    #[test]
    fn test_position_startpos() {
        let board = test_position_helper(&["startpos"]);
        assert_eq!(board.to_fen(), "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        let board = test_position_helper(&[]);
        assert_eq!(board.to_fen(), "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
    }

    #[test]
    fn test_position_startpos_multiple_moves() {
        let board = test_position_helper(&["moves", "e2e4", "e7e5", "g1f3"]);
        assert_eq!(board.side_to_move(), Color::Black);
        let fen = board.to_fen();
        let fen_position = fen.split_whitespace().next().unwrap();
        assert!(fen_position.contains("5N2"));
    }

    #[test]
    fn test_position_fen_with_moves() {
        let board = test_position_helper(&[
            "fen",
            "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR",
            "b",
            "KQkq",
            "e3",
            "0",
            "1",
            "moves",
            "e7e5",
        ]);
        assert_eq!(board.side_to_move(), Color::White);
    }

    #[test]
    fn test_position_empty_moves_list() {
        let board = test_position_helper(&["moves"]);
        assert_eq!(board.to_fen(), "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
    }

    #[test]
    fn test_position_invalid_move_ignored() {
        let board = test_position_helper(&["moves", "e2e4", "invalid", "e7e5"]);
        assert_eq!(board.side_to_move(), Color::White);
    }

    #[test]
    fn test_position_long_move_sequence() {
        let board = test_position_helper(&["moves", "e2e4", "e7e5", "g1f3", "b8c6", "f1b5", "a7a6"]);
        assert_eq!(board.side_to_move(), Color::White);
    }

    #[test]
    fn test_position_castling() {
        let board = test_position_helper(&[
            "fen",
            "r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R",
            "w",
            "KQkq",
            "-",
            "0",
            "1",
            "moves",
            "e1g1",
        ]);
        assert_eq!(board.side_to_move(), Color::Black);
    }

    #[test]
    fn test_position_en_passant() {
        let board = test_position_helper(&[
            "fen",
            "rnbqkbnr/ppp1p1pp/8/3pPp2/8/8/PPPP1PPP/RNBQKBNR",
            "w",
            "KQkq",
            "f6",
            "0",
            "1",
            "moves",
            "e5f6",
        ]);
        assert_eq!(board.side_to_move(), Color::Black);
    }

    #[test]
    fn test_position_promotion() {
        let board = test_position_helper(&["fen", "8/P7/8/8/8/8/8/4K2k", "w", "-", "-", "0", "1", "moves", "a7a8q"]);
        assert_eq!(board.side_to_move(), Color::Black);
    }

    #[test]
    fn test_make_uci_move_invalid() {
        let mut board = Board::starting_position();
        let fen_before = board.to_fen();
        make_uci_move(&mut board, "invalid_move");
        assert_eq!(board.to_fen(), fen_before);
    }

    #[test]
    fn test_position_moves_without_startpos_ignored() {
        let board = test_position_helper(&["moves", "e2e4", "e7e5"]);
        assert_eq!(board.to_fen(), "rnbqkbnr/pppp1ppp/8/4p3/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2");
    }
}