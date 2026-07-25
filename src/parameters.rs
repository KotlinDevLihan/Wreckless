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
        pub fn set_parameter(name: &str, value: &str) {
            match name {
                $(stringify!($name) => unsafe { parameters::$name = value.parse().unwrap() },)*
                _ => panic!("Unknown tunable parameter: {name}"),
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
    // Rougher estimate than lmr_capture_stat's fix below: razoring's own
    // terms (razor_base, razor_quad*depth^2) have no /1024 normalization to
    // compare against directly, unlike RFP/FP's margins. Pulled back from an
    // earlier 900 guess (too large a swing from the original 300) toward a
    // more conservative increase -- needs SPSA/SPRT more than most values
    // here.
    i32 razor_corr: 612;
    // Guessed, not just exposed: the same cutoff-count signal is already
    // SPSA-tunable everywhere else it's used (lmr_cutoff: 1151, fds_cutoff:
    // 1394). Re-checked against the established lmr_cutoff/lmr_noisy_base
    // and fds_cutoff/fds_noisy_base ratios (~0.81x and ~1.48x) applied to
    // razor_base (237), which suggests ~190-350 -- raised from an earlier,
    // too-conservative 100 toward the low end of that range.
    i32 razor_cutoff: 270;
    // Extends opponent_worsening (already used in RFP's rfp_worsening) to
    // razoring. Genuinely ambiguous how to scale between the two formulas'
    // very different base magnitudes (razor_base is ~12x rfp_base) -- shifted
    // toward the better-justified ratio-scaled anchor (~398) rather than
    // splitting evenly with the weaker direct-copy anchor.
    i32 razor_worsening: 320;

    // Reverse Futility Pruning
    i32 rfp_depth_quad: 1140;
    i32 rfp_improvement: 120;
    i32 rfp_depth_lin: 22;
    i32 rfp_corr: 669;
    // Speculative, low-confidence: both rfp_worsening and rfp_no_threats are
    // flat boolean-gated subtractions in the same margin, so they're directly
    // comparable (unlike rfp_corr, which is continuously scaled). Opponent-
    // worsening is arguably the more information-rich signal of the two.
    // Split evenly between the original 20 and full parity with
    // rfp_no_threats (54), rather than favoring the (equally unproven)
    // pullback toward the original. rfp_no_threats reverted to its original
    // value -- no real evidence it needed changing.
    i32 rfp_worsening: 46;
    i32 rfp_no_threats: 54;
    i32 rfp_base: 19;

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
    // Speculative: extends ttMoveHistory (an existing, already-tracked
    // signal) into the null-move reduction depth, on the theory that a
    // well-trusted TT move correlates with a more settled position -- a
    // plausible connection, not a derived one.
    i32 nmp_r_tt_history: 250;

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
    i32 hp_margin: 948;
    // New: history pruning extended to bad-SEE noisy moves, previously
    // quiet-only. Scaled up from hp_margin by noisy_history's larger range
    // (MAX_HISTORY 12800 vs quiet's 8192) rather than reused as-is.
    i32 hp_noisy_margin: 1481;

    // SEE Pruning
    i32 see_q_quad: 12;
    i32 see_q_lin: 56;
    i32 see_q_hist: 27;
    // Extends the cutoff_count signal (already used in lmr_cutoff/fds_cutoff/
    // razor_cutoff) to SEE pruning as well -- not previously used here.
    // Re-checked: at depth 5 the surrounding terms sum to roughly 600
    // (quad+lin+base), and an initial guess of 15 was only ~2.5% of that --
    // far weaker proportionally than lmr_cutoff/fds_cutoff are relative to
    // their own base terms (~80%). Raised to a still-modest but more
    // meaningful fraction.
    i32 see_q_cutoff: 60;
    i32 see_q_base: 27;
    i32 see_n_quad: 7;
    i32 see_n_lin: 36;
    i32 see_n_hist: 39;
    i32 see_n_cutoff: 37;
    i32 see_n_base: 14;

    // Late Move Reductions
    i32 lmr_ilog: 269;
    // Missing entirely until now: Stockfish's reduction is fundamentally
    // reductions[depth] * reductions[moveNumber] (both log-scaled), but this
    // formula only ever used move_count as an on/off gate (depth >= 2 &&
    // move_count >= 2), never as a continuous scaling factor. Starting
    // magnitude mirrors lmr_ilog's own role as a log2-scaled base term;
    // genuinely untested and needs SPSA/SPRT before trusting the value.
    i32 lmr_movecount_ilog: 220;
    i32 lmr_improvement: 425;
    i32 lmr_corr: 3417;
    i32 lmr_exact: 1412;
    i32 lmr_tt_alpha: 464;
    i32 lmr_tt_depth: 326;
    i32 lmr_quiet_base: 2171;
    i32 lmr_quiet_hist: 179;
    i32 lmr_quiet_alpha: 418;
    // At 437, a queen capture (value 1242) alone contributed ~8483 to the
    // noisy `history` term feeding lmr_noisy_hist/1024 -- larger than
    // lmr_noisy_base (1426) and comparable to NoisyHistory::MAX_HISTORY
    // (12800), the clamp on the entire learned capture-history signal.
    // Rescaled so a queen capture contributes ~600 (comparable to
    // lmr_noisy_base), a supplementary nudge rather than a term that
    // swamps the actual learned noisy-history signal it's meant to add to.
    i32 lmr_capture_stat: 31;
    i32 lmr_noisy_base: 1426;
    i32 lmr_noisy_hist: 130;
    i32 lmr_pv_base: 519;
    i32 lmr_pv_delta: 437;
    i32 lmr_ttpv: 333;
    i32 lmr_ttpv_score: 611;
    i32 lmr_ttpv_depth: 685;
    i32 lmr_cutnode: 1852;
    i32 lmr_cutnode_null: 2204;
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
    // by roughly the same ratio fds_ilog sits below lmr_ilog.
    i32 fds_movecount_ilog: 170;
    i32 fds_improvement: 366;
    i32 fds_corr: 2255;
    i32 fds_quiet_base: 1468;
    i32 fds_quiet_hist: 118;
    i32 fds_noisy_base: 940;
    i32 fds_noisy_hist: 63;
    i32 fds_ttpv: 844;
    i32 fds_ttpv_depth: 1129;
    i32 fds_cutnode: 1260;
    i32 fds_cutnode_null: 2168;
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

    // Correction history updates
    i32 corr_bonus_scale: 148;
    // Was asymmetric (min 4678 / max 2496) despite every other history table
    // in the codebase clamping symmetrically; no evidence that letting
    // negative corrections swing ~2x larger than positive ones was
    // intentional, so both bounds now match the smaller, already-shipped
    // value rather than the untested larger one.
    i32 corr_bonus_min: 2496;
    i32 corr_bonus_max: 2496;
    // Upstream tuned this divisor for a 5-term correction blend (pawn,
    // non-pawn x2, continuation x2). Material/minor/major are folded in as a
    // single group damped by corr_minor_major rather than counted as 3 more
    // full-strength terms, so the divisor is rescaled by the group's
    // *effective* contribution (5 + 3 * corr_minor_major/128 ≈ 6.9 effective
    // terms) rather than its raw table count.
    i32 corr_weight_div: 88;
    // Damped below 100% (128): this group is an unproven addition relative
    // to the pawn/non-pawn/continuation terms upstream actually tuned, so it
    // defaults to a conservative ~63% weight rather than being trusted
    // equally until real tuning data says otherwise.
    i32 corr_minor_major: 80;

    // Continuation history
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
    i32 conthist_lag6: 700;
    // Positive-consistency multipliers, indexed by how many of the (up to 6)
    // continuation entries checked so far were already positive. Same
    // reasoning as the lag weights above: exposed, not re-guessed.
    i32 conthist_mult0: 94;
    i32 conthist_mult1: 103;
    i32 conthist_mult2: 110;
    i32 conthist_mult3: 106;
    i32 conthist_mult4: 119;
    i32 conthist_mult5: 126;
    i32 conthist_mult6: 121;

    // Move ordering
    i32 good_quiet_threshold: -14000;

    // Qsearch SEE pruning threshold -- previously hardcoded consts with no
    // SPSA exposure at all. Defaults unchanged; exposed so SPSA can actually
    // re-check qs_see_corr_cap now that corr_weight_div/corr_minor_major
    // have shifted what typical correction_value magnitudes look like since
    // this cap was last (informally) calibrated.
    i32 qs_see_div: 8;
    i32 qs_see_corr_cap: 75;
    i32 qs_see_base: 70;

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

}
