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
    // Both of these are present in 0.1.2 (`4135b69`), the last build that
    // measured neutral, so they are part of the baseline rather than
    // speculative additions. They were removed alongside `razor_worsening`
    // when the razoring extras were pruned; only `razor_worsening` was
    // genuinely new, and only it stayed removed. `razor_cutoff` is 65 because
    // that is the literal 0.1.2 used, not the 270 a later parameterization
    // guessed at.
    i32 razor_corr: 300;
    i32 razor_cutoff: 65;

    // Reverse Futility Pruning
    i32 rfp_depth_quad: 980;
    i32 rfp_improvement: 120;
    i32 rfp_depth_lin: 22;
    i32 rfp_corr: 669;
    // Restored to 20, the value carried by 0.1.2 (`4135b69`). It had been
    // raised to 46 by "splitting evenly" toward parity with rfp_no_threats --
    // an admittedly speculative move with nothing measured behind it.
    //
    // 20 also happens to be what upstream works out to. Stockfish folds this
    // signal in as `335 * opponentWorsening * futilityMult / 1024`, and with
    // `futilityMult = min(45 + 4 * depth, 85)` that is ~21cp at mid depths and
    // ~28cp at high ones. So 20 sits right on upstream's effective value and 46
    // is roughly double it.
    i32 rfp_worsening: 20;
    i32 rfp_no_threats: 54;
    i32 rfp_base: 14;

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
    i32 probcut_tt_margin: 470;
    i32 probcut_base: 254;
    i32 probcut_improving: 85;
    i32 probcut_score_div: 319;
    i32 probcut_beta_step: 197;

    // Late Move Pruning
    i32 lmp_base: 3000;
    i32 lmp_improvement: 78;
    i32 lmp_quad: 1450;
    i32 lmp_history: 60;

    // Futility Pruning
    i32 fp_depth: 79;
    i32 fp_history: 55;
    i32 fp_beta_bonus: 60;
    i32 fp_corr: 555;
    i32 fp_base: 100;

    // Bad Noisy Futility Pruning
    i32 bnfp_depth: 80;
    i32 bnfp_history: 76;
    i32 bnfp_base: 12;

    // History Pruning
    i32 hp_margin: 720;
    // New: history pruning extended to bad-SEE noisy moves, previously
    // quiet-only. Scaled up from hp_margin by noisy_history's larger range
    // (MAX_HISTORY 12800 vs quiet's 8192) rather than reused as-is.
    i32 hp_noisy_margin: 1100;

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
    // meaningful fraction. Trimmed from 60 back toward the middle of that
    // range on review, to keep quiet SEE pruning a little less eager at nodes
    // whose children have been producing cutoffs; untested either way.
    i32 see_q_cutoff: 28;
    i32 see_q_base: 20;
    i32 see_n_quad: 7;
    i32 see_n_lin: 36;
    i32 see_n_hist: 39;
    i32 see_n_cutoff: 20;
    i32 see_n_base: 10;

    // Late Move Reductions
    i32 lmr_ilog: 269;
    // Missing entirely until now: Stockfish's reduction is fundamentally
    // reductions[depth] * reductions[moveNumber] (both log-scaled), but this
    // formula only ever used move_count as an on/off gate (depth >= 2 &&
    // move_count >= 2), never as a continuous scaling factor. Starting
    // magnitude mirrors lmr_ilog's own role as a log2-scaled base term;
    // genuinely untested and needs SPSA/SPRT before trusting the value.
    //
    // Lowered from 240. At that value this single term was doing essentially
    // all of the engine's over-pruning relative to 0.1.2: zeroing it alone
    // moved the bench tree from 2.20M to 2.74M nodes, while zeroing
    // hp_noisy_margin, qs_delta_margin and see_*_cutoff moved it by -19k,
    // +54k and +51k respectively. That left the engine searching a tree ~16%
    // *smaller* than the last build to measure neutral, at the same nominal
    // depth -- same depth, thinner search, which is consistent with the
    // engine matching base on depth in games while losing badly.
    //
    // It reduces every late move by `value * log2(move_count) / 1024`, so at
    // move 32 it was removing over a full ply on its own. 90 keeps the
    // mechanism doing real work while landing the tree back near 0.1.2's.
    // Still untested, and now actually reachable by SPSA -- this parameter
    // was missing from spsa.config entirely until recently.
    i32 lmr_movecount_ilog: 40;
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
    // Restored to 437, the value carried by 0.1.2 (`4135b69`).
    //
    // It had been cut to ~31-36 on the reasoning that a queen capture
    // contributing ~8483 "swamps" the learned noisy-history signal, since that
    // dwarfs lmr_noisy_base (1426). That reasoning was wrong: dominating the
    // capture statScore is exactly what the piece-value term is *for*
    // upstream. Stockfish computes
    //
    //     ss->statScore = 873 * PieceValue[captured] / 128 + captureHistory[..]
    //
    // i.e. 6.82 x the captured piece's value. This engine computes
    // `lmr_capture_stat * value / 64`, so 437 gives 6.83 x value -- upstream's
    // coefficient to within 0.2%. 36 gives 0.56 x, twelve times too small, and
    // the term stops doing its job. The consumer scaling matches too:
    // Stockfish applies `r -= statScore * 439 / 4096` (~0.107) against this
    // engine's `lmr_noisy_hist / 1024` (~0.127).
    //
    // In short, 437 was already correctly matched to upstream and the rescale
    // broke it.
    i32 lmr_capture_stat: 437;
    i32 lmr_noisy_base: 1426;
    i32 lmr_noisy_hist: 130;
    i32 lmr_pv_base: 519;
    i32 lmr_pv_delta: 437;
    i32 lmr_ttpv: 333;
    i32 lmr_ttpv_score: 611;
    i32 lmr_ttpv_depth: 685;
    i32 lmr_cutnode: 1550;
    i32 lmr_cutnode_null: 1850;
    i32 lmr_check: 955;
    i32 lmr_cutoff: 950;
    i32 lmr_cutoff_node: 400;
    i32 lmr_singular: 496;
    i32 lmr_singular_margin: 185;
    i32 lmr_singular_max: 2021;
    i32 lmr_prev_reduction: 118;

    // Full Depth Search reductions
    i32 fds_ilog: 207;
    // Same missing move-count scaling as lmr_movecount_ilog above, same
    // caveat: untested starting value, scaled down from lmr_movecount_ilog
    // by roughly the same ratio fds_ilog sits below lmr_ilog. Lowered from 185
    // alongside lmr_movecount_ilog and for the same reason, preserving that
    // ratio; likewise still untested and likewise only recently reachable by
    // SPSA.
    i32 fds_movecount_ilog: 34;
    i32 fds_improvement: 366;
    i32 fds_corr: 2255;
    i32 fds_quiet_base: 1468;
    i32 fds_quiet_hist: 118;
    i32 fds_noisy_base: 940;
    i32 fds_noisy_hist: 63;
    i32 fds_ttpv: 844;
    i32 fds_ttpv_depth: 1129;
    i32 fds_cutnode: 1180;
    i32 fds_cutnode_null: 2050;
    i32 fds_cutoff: 1180;
    i32 fds_cutoff_node: 258;
    i32 fds_singular: 351;
    i32 fds_singular_margin: 188;
    i32 fds_singular_max: 2167;
    i32 fds_ttmove: 3002;
    i32 fds_prev_reduction: 115;

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
    // NOTE: these two are coupled. `eval_correction()` sums 5 upstream terms
    // plus a minor/major/material group weighted by `corr_minor_major / 128`,
    // so the normalizing divisor that keeps the blend on upstream's scale is
    // `upstream_div * (5 + 3 * corr_minor_major / 128) / 5`. At
    // corr_minor_major = 128 that is 8 effective terms and gives 102.
    //
    // Keep them coupled. Setting the divisor below that figure divides the
    // blend by less than it sums, scaling every correction value up -- the
    // same direction as the original normalization bug documented in the
    // README, which silently inflated every RFP/FP/LMR/NMP margin that reads
    // `eval_correction()`, since those margins all scale with
    // `correction_value.abs()`. A 96 was tried here and reverted for exactly
    // that reason: it is indistinguishable from the bug the fork already paid
    // for once. If `corr_minor_major` changes, recompute this rather than
    // nudging it by hand.
    i32 corr_weight_div: 102;
    i32 corr_minor_major: 128;

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
