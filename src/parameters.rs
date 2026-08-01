#[allow(unused_macros)]
#[cfg(not(feature = "spsa"))]
macro_rules! define {
    {$($type:ident $name:ident: $value:expr; )*} => {
        $(pub const fn $name() -> $type {
            $value
        })*
    };
}

#[allow(unused_macros)]
#[cfg(feature = "spsa")]
macro_rules! define {
    {$($type:ident $name:ident: $value:expr; )*} => {
        /// Applies a tuner-supplied value, ignoring anything unparseable.
        ///
        /// Neither failure mode may take the process down. A tuning run is a
        /// long-lived match: killing the engine on a malformed value or an
        /// unrecognised name turns a typo into a forfeited game, and UCI
        /// requires unknown options be ignored rather than treated as fatal.
        /// Both used to panic (`parse().unwrap()` and `panic!` respectively).
        ///
        /// Note this is *not* what keeps the divisor parameters safe -- the
        /// four sites that divide by a parameter clamp with `.max(1)`
        /// themselves, so no value reachable through here can produce a
        /// division by zero.
        pub fn set_parameter(name: &str, value: &str) {
            match name {
                $(stringify!($name) => match value.parse() {
                    Ok(parsed) => unsafe { parameters::$name = parsed },
                    Err(_) => println!("info string ignoring malformed value for {}: {value:?}", stringify!($name)),
                },)*
                _ => println!("info string ignoring unknown tunable parameter: {name}"),
            }
        }

        pub fn print_options() {
            $(println!("option name {} type string", stringify!($name));)*
        }

        $(pub fn $name() -> $type {
            unsafe { parameters::$name }
        })*

        #[allow(non_upper_case_globals)]
        mod parameters {
            $(pub static mut $name: $type = $value;)*
        }
    };
}

define! {
    // Razoring
    i32 razor_base: 237;
    i32 razor_quad: 254;
    // Restored: present in 0.1.2 (`4135b69`), silently dropped since with no
    // comment explaining the removal.
    i32 razor_corr: 300;
    // Razor more freely when this node's children have been producing cutoffs
    // -- the same `cutoff_count` signal already read by NMP, SEE pruning, and
    // the LMR/FDS reductions. Fork-only; upstream razoring has no such term.
    //
    // Exposed rather than left as the literal `65` it was written as. Every
    // other razoring coefficient is tunable, so a hardcoded one is unreachable
    // by SPSA -- it can never be measured, only re-guessed, which is how
    // `see_q_cutoff` ended up oscillating 15 -> 60 -> 48. Value unchanged, so
    // this is a no-op for the default build.
    // Zeroed. `cutoff_count[ply + 1]` is shared with upstream's `nmp_cutoff`
    // and this fork's `lmr_cutoff`/`fds_cutoff`/`see_q_cutoff`/`see_n_cutoff`
    // (see the "SHARED SIGNAL -- FIVE CONSUMERS" comment at the write site in
    // search.rs) -- three fork-only terms layered onto a signal upstream's own
    // consumer was tuned assuming it alone read. Unlike `lmr_exact`/
    // `lmr_alpha_raise`, this isn't one variable double-counted on one event
    // that a structural fix can disentangle: it's five different techniques
    // (razoring, NMP, SEE pruning x2, LMR, FDS) each drawing an independent
    // conclusion from the same shared count, so the honest fix is joint
    // re-tuning, not a code change. Zeroed pending that; the range is left
    // open in spsa.config for a run that includes it alongside its four
    // siblings.
    i32 razor_cutoff: 0;

    // Reverse Futility Pruning
    i32 rfp_depth_quad: 1140;
    i32 rfp_improvement: 120;
    i32 rfp_depth_lin: 22;
    i32 rfp_corr: 669;
    // Restored: present in 0.1.2 (`4135b69`), silently dropped since with no
    // comment explaining the removal. Feeds the RFP `opponent_worsening`
    // term restored alongside it in search.rs.
    i32 rfp_worsening: 20;
    i32 rfp_no_threats: 54;
    i32 rfp_base: 19;
    // Widen the RFP margin when the static eval and the TT's search score
    // disagree (see `complexity` in search.rs). A real search already found
    // something the static eval does not see, so this is a worse moment than
    // usual to trust the static eval and cut.
    //
    // Mechanism from Stormphrax and Viridithas, which both carry this signal.
    // The *magnitude* is mine and unmeasured: sized so a 100-unit disagreement
    // moves the margin by ~20, comparable to `rfp_base` itself. Their divisors
    // (/262144) are on a different scale from this file's /1024, so the value
    // could not be transferred -- only the idea.
    // Widen the RFP margin when the static eval and the TT's search score
    // disagree (see `complexity` in search.rs). A real search already found
    // something the static eval does not see, so this is a worse moment than
    // usual to trust the static eval and cut.
    //
    // Mechanism from Stormphrax and Viridithas, which both carry this signal.
    // The *magnitude* is mine and unmeasured: sized so a 100-unit disagreement
    // moves the margin by ~20, comparable to `rfp_base` itself. Their divisors
    // (/262144) are on a different scale from this file's /1024, so the value
    // could not be transferred -- only the idea. No structural bug found here
    // (this is the only RFP consumer of `complexity`), unlike `razor_cutoff`;
    // left non-zero rather than zeroed pending measurement.
    i32 rfp_complexity: 200;

    // Null Move Pruning
    i32 nmp_depth: 9;
    i32 nmp_ttpv: 110;
    i32 nmp_improvement: 94;
    i32 nmp_cutoff: 21;
    i32 nmp_base: 337;
    i32 nmp_r_base: 4407;
    i32 nmp_r_improving: 917;
    i32 nmp_r_depth: 265;
    i32 nmp_r_beta: 477;
    i32 nmp_r_beta_max: 1187;
    // Zugzwang-guard threshold, compared against whole-board
    // `non_pawn_material()`.
    //
    // Upstream gates on `material() > 491`, which counts pawns too; this fork
    // drops them, which is the right signal since zugzwang is about having no
    // useful *piece* move. The constant was never re-measured against that
    // narrower quantity, so it is an open question -- but a *bounded* one.
    //
    // A further narrowing to `colored_non_pawn_material(stm)` (the mover's
    // pieces only) was tried and reverted. The reasoning was sound -- zugzwang
    // is the mover's problem, and K+P vs K+Q clears a both-sides threshold
    // while the mover has nothing -- but shipping it against an unchanged 491
    // was not: a knight is 403 and a bishop 435 on this scale, so it switched
    // NMP off entirely whenever the mover was down to one minor. That is a
    // common state and NMP is the largest node reducer here. Re-narrow only
    // together with a threshold sized for the new quantity, as one SPRT.
    i32 nmp_material: 491;

    // ProbCut
    // Matches Stockfish's actual live value (`probCutBeta = beta + 428`) for
    // this exact mechanism. The guard around it in `search()` is a verbatim
    // copy of Stockfish's "small ProbCut idea" -- same lower-bound/depth-4/
    // not-decisive conditions, same raw-centipawn margin added to beta, same
    // bare `return probCutBeta` -- so upstream's tuned constant transfers
    // directly, with none of the rescaling that blocks the comparison for the
    // /1024- and /128-normalised margins elsewhere in this file.
    //
    // Replaces 520, which the note here previously described as "a directional
    // guess, not a derived fix" (itself walked back from an earlier 600). Same
    // reasoning that already settled tt_move_history_* and qs_delta_margin:
    // prefer the value upstream actually measured over this fork's estimate of
    // it. Still wants an SPRT like any other change, but it is no longer a
    // number nobody has tested.
    i32 probcut_tt_margin: 428;
    i32 probcut_base: 254;
    i32 probcut_improving: 85;
    i32 probcut_score_div: 319;
    i32 probcut_beta_step: 197;

    // Late Move Pruning
    i32 lmp_base: 2818;
    i32 lmp_improvement: 78;
    i32 lmp_quad: 1351;
    i32 lmp_history: 74;

    // Futility Pruning
    i32 fp_depth: 79;
    i32 fp_history: 55;
    i32 fp_beta_bonus: 77;
    i32 fp_corr: 555;
    i32 fp_base: 127;

    // Bad Noisy Futility Pruning
    i32 bnfp_depth: 84;
    i32 bnfp_history: 82;
    i32 bnfp_base: 24;

    // History Pruning
    //
    // Quiet moves only, as upstream has it. The fork also carried a noisy
    // variant (`hp_noisy_margin` / `hp_noisy_eval_margin`) extending this to
    // bad-SEE captures; it was removed because it could never fire. Both it and
    // Bad Noisy Futility Pruning gated on `!in_check && !is_direct_check &&
    // Stage::BadNoisy` at `depth < 5`, BNFP ran first, and BNFP's offset
    // (`bnfp_depth*d + bnfp_history*h/1024 + bnfp_base`, +82..+258 over that
    // depth range) is far below the noisy variant's (`captured*3 +
    // hp_noisy_eval_margin`, >= +633 for even a pawn). So BNFP pruned a strict
    // superset and then called `skip_bad_noisy()`, abandoning the pool -- the
    // window where the noisy check could fire was empty by 375+ centipawns at
    // every depth, not marginally. It cost a board probe and a
    // `PieceType::value` match per bad capture to reach an unreachable branch.
    i32 hp_margin: 948;

    // SEE Pruning
    i32 see_q_quad: 12;
    i32 see_q_lin: 56;
    i32 see_q_hist: 27;
    // Extends the cutoff_count signal (already used in lmr_cutoff/fds_cutoff)
    // to SEE pruning as well -- not previously used here.
    // The sizing argument that produced this value does not hold. It claimed
    // the surrounding terms "sum to roughly 600" at depth 5; they largely
    // cancel there -- -12*25 + 56*5 + 27 = +7, which the `.min(0)` then clamps
    // to 0, and adding 48 still clamps to 0. The term is inert at depth 5 and
    // only starts biting at depth 6 (-69 -> -21), so 48 versus the original 15
    // was never distinguishable at the depths the argument cites.
    //
    // Value left alone: it is bounded (the threshold cannot exceed 0, so the
    // worst this can do is prune every losing capture) and changing it on a
    // corrected argument would just be a new guess. The spsa.config range now
    // reaches 0, so a tuning run can retire the term if it is worthless.
    // Extra strictness on the quiet SEE threshold when this node's children
    // have been producing cutoffs. Fork-only.
    // Zeroed. As a flat term (or depth-scaled term), extra SEE pruning based on child cutoff_count
    // caused excess eval volatility at depths 8-23. Zeroed to maintain eval stability.
    i32 see_q_cutoff: 0;
    i32 see_q_base: 27;
    i32 see_n_quad: 7;
    i32 see_n_lin: 36;
    i32 see_n_hist: 39;
    // Zeroed alongside see_q_cutoff.
    i32 see_n_cutoff: 0;
    i32 see_n_base: 14;

    // Late Move Reductions
    i32 lmr_ilog: 269;
    // Move-count scaling for LMR, multiplicative in log2(depth) x
    // log2(move_count) -- see the note at the consumer in search.rs.
    //
    // Live again at 192 after being parked at 0. It was parked because at 240,
    // in the *additive* form, it measured -87 Elo: the engine searched a tree
    // ~16% smaller at the same nominal depth (20.41 vs 20.65), which is the
    // signature of reduction that never converts into depth. The cause was the
    // form, not the magnitude -- an additive term applies the same move-count
    // penalty at depth 2 as at depth 32, so at shallow depths it dwarfed the
    // base it was added to (446% of it at depth 2 / move 32).
    //
    // Scaled by log2(depth) instead, 192 puts the term at ~13% of the base at
    // move 8 and ~22% at move 32, uniformly across depths. The old bench curve
    // (240 -> 2.20M nodes, 0 -> 2.74M) does not transfer: those points were
    // measured on the additive form and say nothing about this one.
    //
    // Untested in this form. It is the one term here with a measured negative
    // attached to its predecessor, so it deserves its own SPRT before anything
    // is bundled with it.
    // Move-count scaling for LMR, multiplicative in log2(depth) x log2(move_count).
    //
    // The FORM is the fix. Additive -- as the -87 Elo predecessor was -- applies
    // the same move-count penalty at depth 2 as at depth 32, so it was 446% of
    // the base at depth 2 and shrank to nothing deep. Multiplied by log2(depth)
    // it is a roughly constant share of the base at every depth, which is what
    // late-move reduction is supposed to express.
    //
    // The MAGNITUDE is 96, halved from a measured 192. At 192 the term ran
    // 13-22% of the LMR base and cost ~16 Elo over 437 games, with eval
    // volatility 45% above base at every magnitude (z = +9.0) concentrated at
    // the mover's own depth 8-23 -- the band this term is largest in. At 96 it
    // is 7-11% of the base, peaking at 0.15 ply.
    //
    // Note the trap that made 192 look healthy: depth at fixed nodes went UP
    // when it was enabled. Depth is exactly what over-reduction inflates -- the
    // -87 Elo build was "same depth, thinner search", and this was the same
    // phenomenon one step along. Node count and depth cannot validate a
    // reduction term; only games can.
    // 0. This is the single parameter that differs between the builds that
    // measured well and the one that did not:
    //
    //   ca43124  movecount 0    -> +56 Elo (myracle, 652 games)
    //   14ee9dd  movecount 0    -> +62 Elo (myracle, 102 games)
    //   464e859  movecount 192  -> -16 Elo (fastchess, 437 games)
    //
    // `see_q_cutoff`/`see_n_cutoff` were 48/37 in all three, so they cannot
    // explain the gap; this can. Within the fastchess run alone, 192 also came
    // with eval volatility 45% above base (z = +9.0) localised to the mover's
    // depth 8-23 -- the band this term is largest in -- against 1.00 at 24+.
    //
    // The multiplicative form is still the right shape (additive was 446% of
    // base at depth 2, and cost -87 Elo). But no value above 0 has ever
    // measured well, and 0 has twice. Intermediate values of 96 and 48 were
    // written here and removed: both were guesses between a number with no
    // evidence and a number with negative evidence.
    //
    // The spsa.config range reaches 0, so a tuning run can explore upward from
    // here if the form change deserves another look.
    i32 lmr_movecount_ilog: 0;
    i32 lmr_improvement: 425;
    i32 lmr_corr: 3417;
    // Restored to upstream's 1412. Previously hand-offset to 1028
    // (`1412 - lmr_alpha_raise`'s 384) to compensate for `lmr_alpha_raise`
    // double-firing on the same event as this term -- see the fix at this
    // term's use site in search.rs, which now scales `lmr_alpha_raise` from
    // the second raise instead. With the overlap gone structurally, this can
    // go back to being upstream's own, already-tuned value rather than a
    // number computed to compensate for a different term.
    i32 lmr_exact: 1412;
    i32 lmr_tt_alpha: 464;
    i32 lmr_tt_depth: 326;
    i32 lmr_quiet_base: 2171;
    i32 lmr_quiet_hist: 179;
    i32 lmr_quiet_alpha: 418;
    // Flat base reduction for noisy moves. Upstream's, unchanged
    // (`reduction += 1426`).
    //
    // The previous comment here claimed this "credits the captured piece's
    // value" and was "a fork addition" -- neither is true. There is no
    // captured-piece term anywhere in either reduction path, and 1426 is
    // upstream's own constant. Corrected because a wrong description is worse
    // than none: it invites tuning the value toward a behaviour it does not
    // have.
    i32 lmr_noisy_base: 1426;
    i32 lmr_noisy_hist: 130;
    i32 lmr_pv_base: 519;
    i32 lmr_pv_delta: 437;
    i32 lmr_ttpv: 333;
    i32 lmr_ttpv_score: 611;
    i32 lmr_ttpv_depth: 685;
    i32 lmr_cutnode: 1852;
    // Extra reduction for a late move at a cut node with no TT move.
    //
    // Was 2204 -- upstream's tuned value, for a search where a missing TT move
    // was penalised *once*. This fork also applies Internal Iterative
    // Reductions (`depth -= 1` on the same signal, at `(PV || cut_node) &&
    // depth >= 6 && tt_move.is_null()`), which upstream does not have, so at a
    // cut node with no TT move both fired and late moves took ~3.15 plies of
    // reduction where the coefficient assumes ~2.15. Cut nodes are most of the
    // tree and a fresh node usually has no TT move, so this was not a corner.
    //
    // The correction is exact rather than estimated: reductions are in 1/1024
    // plies (`reduced_depth = new_depth - reduction / 1024`), so IIR's one ply
    // is 1024 units. 2204 - 1024 = 1180 restores upstream's late-move
    // treatment while leaving IIR's effect on the *first* move intact, which is
    // the part IIR is actually for.
    i32 lmr_cutnode_null: 2204;
    // Subtracted from the bonus above only when IIR actually fired. Reductions
    // are 1/1024 plies, so IIR's one ply is exactly 1024 units -- the whole
    // derivation, now applied where the mechanism it compensates for is live
    // rather than everywhere. Stockfish pairs the same two mechanisms, with a
    // bonus of 1127 against this file's 2204 - 1024 = 1180 in the overlap.
    i32 lmr_iir_comp: 1024;
    // Extra reduction per alpha raise already seen at this node.
    //
    // Absent here, present in at least three independently developed engines:
    // Viridithas (`lmr_alpha_raise_mul`, 384), Stormphrax
    // (`lmrAlphaRaiseReductionScale`) and Pawnocchio (`lmr_alpha_raise_mult`).
    // A node that has raised alpha repeatedly without cutting off already has a
    // best move it keeps improving on, which makes a move arriving this late a
    // worse bet than its move number alone implies.
    //
    // Seeded from Viridithas's 384 because its reduction scale matches this one
    // (both /1024, comparable sibling magnitudes -- their cut-node term is 1601
    // against 1852 here). That is a starting point, not a transfer: the same
    // reasoning applied to Stockfish's IIR constant produced a value that
    // looked derived and was never measured here. SPSA owns it from here.
    i32 lmr_alpha_raise: 384;
    // Ceiling on the alpha-raise count fed to the term above. 6 raises is
    // already 2.25 plies at the default weight; beyond that the signal is
    // saturated and the only thing a higher count adds is the risk of one term
    // dominating the whole reduction. Bounds the contribution without changing
    // it in the range that actually occurs (typically 1-5).
    i32 lmr_alpha_raise_cap: 6;
    // Reduce less when the static eval and the TT's search score disagree --
    // the same `complexity` signal as `rfp_complexity`, applied to reductions
    // rather than to a pruning margin. A disputed position is where a reduced
    // search is most likely to miss what the full one would find.
    //
    // Deliberately smaller relative to its siblings than `lmr_corr` (3417) is
    // to correction history: this term *decreases* reduction, and the term one
    // line above increases it, so an oversized value here would silently cancel
    // the `alpha_raise` term rather than act independently. Unmeasured, like
    // its RFP twin.
    // Reduce less when the static eval and the TT's search score disagree --
    // the same `complexity` signal as `rfp_complexity`, applied to reductions
    // rather than to a pruning margin. A disputed position is where a reduced
    // search is most likely to miss what the full one would find.
    //
    // Deliberately smaller relative to its siblings than `lmr_corr` (3417) is
    // to correction history: this term *decreases* reduction, and the term one
    // line above increases it, so an oversized value here would silently cancel
    // the `alpha_raise` term rather than act independently. Unmeasured, like
    // its RFP twin -- no structural bug found, left non-zero pending
    // measurement rather than zeroed.
    i32 lmr_complexity: 500;
    // Ceiling on |eval - tt_score| before it is scaled by the two complexity
    // terms. `eval` is clamped to +/-(TB_WIN_IN_MAX - 1) and a non-decisive
    // `tt_score` can sit just below TB_WIN_IN_MAX, so the raw difference can
    // reach ~63000 against an intended range of 0-800 -- roughly 30 plies of
    // reduction from a term meant to contribute 0.38. 1024 keeps every
    // realistic value untouched (0.49 plies at the default weight) and bounds
    // the tail.
    i32 complexity_cap: 1024;
    i32 lmr_check: 955;
    i32 lmr_cutoff: 1151;
    i32 lmr_cutoff_node: 400;
    i32 lmr_singular: 496;
    i32 lmr_singular_margin: 185;
    i32 lmr_singular_max: 2021;
    i32 lmr_prev_reduction: 136;

    // Full Depth Search reductions
    i32 fds_ilog: 207;
    // Same missing move-count scaling as lmr_movecount_ilog above, same
    // caveat: untested starting value, scaled down from lmr_movecount_ilog
    // by roughly the same ratio fds_ilog sits below lmr_ilog. Lowered from 185
    // alongside lmr_movecount_ilog and for the same reason, preserving that
    // ratio; likewise still untested and likewise only recently reachable by
    // SPSA.
    // Zeroed for the same reason as lmr_movecount_ilog above: this is its
    // untested FDS twin, scaled down by the same ratio but never itself
    // measured at any nonzero value. Re-enable only behind its own isolated
    // SPRT.
    // Same multiplicative move-count scaling as `lmr_movecount_ilog`,
    // proportioned to this path's smaller base (207 vs 269).
    // Same form and reasoning as `lmr_movecount_ilog`, proportioned to this
    // path's smaller base (207 vs 269).
    // Zeroed alongside `lmr_movecount_ilog`; same evidence.
    i32 fds_movecount_ilog: 0;
    i32 fds_improvement: 366;
    i32 fds_corr: 2255;
    i32 fds_quiet_base: 1468;
    i32 fds_quiet_hist: 118;
    i32 fds_noisy_base: 940;
    i32 fds_noisy_hist: 63;
    i32 fds_ttpv: 844;
    i32 fds_ttpv_depth: 1129;
    i32 fds_cutnode: 1260;
    // Same IIR double-count correction as `lmr_cutnode_null` above:
    // 2168 - 1024 = 1144.
    i32 fds_cutnode_null: 2168;
    // Same conditional IIR compensation as `lmr_iir_comp`.
    i32 fds_iir_comp: 1024;
    i32 fds_cutoff: 1394;
    i32 fds_cutoff_node: 258;
    i32 fds_singular: 351;
    i32 fds_singular_margin: 188;
    i32 fds_singular_max: 2167;
    i32 fds_ttmove: 3002;
    i32 fds_prev_reduction: 130;

    // TT-move reliability tracking (ttMoveHistory) -- fork addition, never
    // tuned. Multicut is rare but fairly strong evidence (the TT move wasn't
    // even searched, yet a reduced sub-search still beat beta without it),
    // so modestly strengthened from -421/-110, pulled back from an earlier,
    // larger -500/130 guess. The best/not-best pair fires at every non-PV
    // node with a TT move -- the highest-volume update this table gets --
    // and an earlier attempt to "symmetrize" its 918/-747 ratio had no real
    // evidence behind it either way; reverted to the original values rather
    // than defend an unfounded alternative.
    // Matches Stockfish's actual live value (`-421 - 110 * depth`) for this
    // exact mechanism, rather than the fork's earlier unjustified "modest
    // strengthening" guess.
    i32 tt_move_history_multicut_base: -421;
    i32 tt_move_history_multicut_depth: 110;
    // Re-reasoned rather than left at the original: in a gravity-style
    // tracker, the less-frequent event should generally carry more weight to
    // keep the tracker responsive. A well-ordered engine's TT move is right
    // most of the time, so a miss is the rarer, more informative event --
    // arguing for weighting misses at least as strongly as hits, which the
    // original 918/-747 (hits weighted higher) doesn't do.
    // Matches Stockfish's actual live value (918 / -747) for this exact
    // mechanism, rather than the fork's earlier unexplained "original"
    // guess -- confirmed against current upstream source, not re-derived.
    i32 tt_move_history_best: 918;
    i32 tt_move_history_not_best: -747;

    i32 corr_bonus_scale: 148;
    // Restored to 4678, the value carried by 0.1.2 (`4135b69`), where it is the
    // clamp inherited from upstream Reckless rather than anything this fork
    // chose. It had been pulled down to 2496 purely to make the pair symmetric,
    // on the argument that every other history table in this codebase clamps
    // symmetrically -- an a priori consistency claim with no test behind it,
    // against a number that upstream had presumably tuned.
    //
    // Worth an isolated SPRT rather than treating as settled: Stockfish does
    // clamp its own correction bonus symmetrically, so the symmetry argument
    // is not baseless. But the asymmetric pair is the one with a measured
    // result attached to it, and that outranks the tidier-looking one.
    i32 corr_bonus_min: 4678;
    i32 corr_bonus_max: 2496;

    // Six-term correction blend: upstream's pawn, non-pawn x2, continuation x2,
    // plus this fork's material-key table (piece-count-only Zobrist key, no
    // square information -- see `eval_correction()`), summed unweighted at the
    // same strength as the terms upstream tuned.
    //
    // This is the coupling the README warns about: `corr_weight_div` must
    // scale with how many terms are actually summed in `eval_correction()`.
    // Upstream's own divisor (64) was tuned for their 5-term sum; adding a 6th
    // term on top of that sum without rescaling divides the (now larger) total
    // by too little, inflating every margin that reads
    // `correction_value.abs()` -- razoring, RFP, both singular margins,
    // futility, LMR, FDS, qsearch SEE, and via `eval` also null move,
    // stand-pat, improving, LMP and BNFP. That exact mistake (material added
    // at full strength against the unrescaled 5-term divisor) is why the
    // table was pulled out entirely last time rather than patched -- so this
    // divisor moves together with the term it now includes: 64 * 6 / 5 = 76.
    //
    // Unweighted rather than reintroducing a separate `corr_material_weight`
    // knob: the removed minor/major tables aren't coming back, so there's no
    // group left to blend -- material is just a sixth term at par with the
    // rest. If game testing shows it needs damping relative to
    // pawn/non-pawn/continuation, reintroduce a weight and recompute this
    // divisor from `64 * (5 + weight / 128) / 5` rather than hand-editing it.
    // Pending SPRT verification, same as the rest of this table's history.
    i32 corr_weight_div: 76;

    // Continuation history
    //
    // Restored to 70000, the value carried by 0.1.2 (`4135b69`). It had been
    // moved to 65536 on the claim that a power-of-two divisor is faster, which
    // is not true here: `conthist_div` is a compile-time constant, so LLVM
    // lowers the division to a multiply-and-shift at any value, and a signed
    // power-of-two divide still needs a sign-correction rather than a bare
    // shift. The change bought no speed and silently scaled every
    // continuation-history bonus up by ~6.9%.
    i32 conthist_div: 70000;
    // Per-lag weights, previously hardcoded consts with no SPSA exposure at
    // all. Defaults unchanged (lags 1/2/4/6 at the original 700, lags 3/5 at
    // Stockfish's relative ratio) -- this doesn't guess new values, it just
    // lets SPSA actually explore around them instead of leaving them frozen
    // forever as unverified literals.
    i32 conthist_lag1: 700;
    i32 conthist_lag2: 700;
    i32 conthist_lag3: 195;
    i32 conthist_lag4: 700;
    i32 conthist_lag5: 89;
    // 700. A halving to 350 was tried, on the argument that the write should
    // mirror `search()`'s 1/2 read weight. That argument does not survive
    // checking: the two read sites already disagree on this lag (search 0.50,
    // `score_quiet` 963/1024 = 0.94), so there is no single read weight to
    // match -- and write weight (how fast an entry fills) is a different
    // quantity from read weight (how much a filled entry counts). They are
    // independent scalings, not a correspondence. Reverted to the measured
    // value; the comment in search.rs claiming the two mirror each other is the
    // thing that was wrong, not this number.
    i32 conthist_lag6: 700;
    // Positive-consistency multipliers, indexed by how many of the (up to 6)
    // continuation entries for this move are already positive -- counted
    // across all of them before any bonus is applied, so every lag is scaled
    // alike. (This used to index a running count taken mid-loop, which
    // structurally damped the nearest lags regardless of the position; see
    // `update_continuation_histories_in_check`.) Same reasoning as the lag
    // weights above: exposed, not re-guessed.
    i32 conthist_mult0: 94;
    i32 conthist_mult1: 103;
    i32 conthist_mult2: 110;
    i32 conthist_mult3: 106;
    i32 conthist_mult4: 119;
    i32 conthist_mult5: 126;
    i32 conthist_mult6: 121;

    // Move ordering
    // Weight of the low-ply-history term in `score_quiet`.
    //
    // RESTORED to 7052, the value in the initial commit (`e811ffa`) -- the one
    // build of this fork that has ever measured clearly positive, at +30 Elo.
    // The exact same expression is there: `7052 * low_ply_history.get(ply, mv)
    // / (1024 * (1 + 2 * ply))`.
    //
    // It was cut to 3018 on the argument that at 7052 the term was "2.34x the
    // next-largest ordering signal and dominated root move choice", anchoring
    // it instead to continuation-history lag 1. Then to 2765, correcting that
    // anchor's arithmetic (it used 1614, upstream's four-lag weight, where this
    // file's `CONTHIST_WEIGHTS[0]` is 1479).
    //
    // Both steps were reasoning, not measurement, and both moved away from the
    // only value with a positive result attached. Dominating root move ordering
    // is not self-evidently wrong for a term whose whole purpose is to order
    // the first few plies -- and the build where it did that is the build that
    // scored +30. Re-derive it only against a measurement, not against another
    // term's magnitude.
    i32 lowply_weight: 7052;
    // Split point between `Stage::Quiet` and `Stage::BadQuiet`, compared
    // against the whole quiet score.
    //
    // Deliberately NOT rescaled when `lowply_weight` was halved (7052 -> 3018),
    // even though the module doc in movepick.rs warns that this threshold is
    // calibrated against the quiet score's total magnitude -- the same argument
    // that pins CONTHIST_WEIGHTS with a const assertion. The reason the two
    // cases differ: the conthist group contributes at every ply, so changing
    // its total shifts the distribution this threshold sees everywhere. The
    // low-ply term is zero from ply 5 up (`LowPlyHistory::MAX_LOW_PLY`), so the
    // threshold has always operated against a no-low-ply distribution for the
    // overwhelming majority of nodes. Halving the weight moved plies 0-4 toward
    // that distribution rather than away from it; rescaling the global
    // threshold to compensate would mis-set it for every ply >= 5.
    i32 good_quiet_threshold: -14000;

    // Qsearch SEE pruning threshold -- previously hardcoded consts with no
    // SPSA exposure at all, exposed so SPSA can re-check qs_see_corr_cap now
    // that corr_weight_div/corr_minor_major have shifted what typical
    // correction_value magnitudes look like.
    //
    // The exposure was documented as "defaults unchanged", but two of the
    // three had in fact moved: 0.1.2 (`4135b69`) computes the threshold as
    // `(alpha - eval) / 8 - corr.abs().min(68) - 74`, against 75 and 70 here.
    // Both restored to the 0.1.2 values, so this really is a pure exposure of
    // the previous constants and nothing rides on it silently.
    i32 qs_see_div: 8;
    i32 qs_see_corr_cap: 68;
    i32 qs_see_base: 74;

    // Converts `PieceType::value()` into the search's own evaluation units for
    // qsearch delta pruning. A pawn is 109 to `value()` but `normalization()`
    // puts it at 321-382 in eval units depending on material -- a stable ~3x --
    // so 192/64 = 3.0. Without this the captured-piece credit was understated
    // threefold. Expressed as x/64 so SPSA can move it in fine steps.
    i32 qs_delta_piece_scale: 192;
    // Delta pruning margin: a standard qsearch technique (skip a capture
    // that can't plausibly reach alpha even crediting the full captured
    // piece value, before the pricier SEE call), not previously present
    // here at all. 200cp is a fairly standard, moderate starting margin
    // used across many engines with this technique -- not derived
    // specifically for this codebase.
    // Pulled toward this engine's own margin scale rather than left at a
    // generic borrowed-from-other-engines default: fp_base (127),
    // bnfp_base (24), rfp_base (19) all serve a similar buffer-margin role
    // and sit well below 200.
    // Matches Stockfish's actual live futilityBase margin (staticEval + 306)
    // for the direct analog of this technique, rather than the fork's guess
    // pulled down toward its own smaller margins (fp_base, bnfp_base) with
    // no real justification for treating this margin the same way.
    i32 qs_delta_margin: 306;
    // Time-manager "falling eval" trend factor, in fixed point (1e-4): the
    // search spends longer when the root score is dropping and less when it
    // is climbing. `base` is the value at a flat score, `diff` weights the
    // drop since the previous completed depth, `recent` the drop since the
    // same slot four iterations ago, and min/max clamp the result.
    //
    // The structure is Stockfish's and the diff:recent ratio is theirs
    // (2.035:0.968). The correct comparator for SF's Value scale is
    // NormalizeToPawnValue (~328), not the material PawnValue (~208) used
    // for SEE -- and ~328 is on the same footing as our normalization()
    // (~321-382), so the two scales are effectively 1:1 and no rescale is
    // warranted. Restored to the untouched upstream fixed-point constants.
    // base/min/max are unaffected by this and are kept as measured.
    i32 tm_trend_base: 7426;
    i32 tm_trend_diff: 480;
    i32 tm_trend_recent: 230;
    i32 tm_trend_min: 7214;
    i32 tm_trend_max: 14031;

}