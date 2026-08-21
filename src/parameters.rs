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
    // ZEROED -- the proportional form was measured and lost Elo.
    //
    // Converting this flat term to a depth-proportional one was the same fix
    // that was right for `threat_scaled` and for LMR's move-count term, and the
    // shape argument was sound. The magnitude was not. Anchoring the equivalence
    // at depth 8 left every shallow node with a far weaker improving correction
    // -- at depth 1 the term fell from 35 units to 4 -- which widens the RFP
    // margin and fires it less exactly where most nodes are.
    //
    // Measured: shipped alongside three other ports in 07bbebb7, that batch went
    // 42.2% -> 37.3% against Artemis on shared openings (-35.5 Elo), and
    // Wreckless's mean search depth fell 17.96 -> 16.56 while the opponent's own
    // depth fell only 0.44 on the same opening change. Of the four, this is the
    // only one that makes the tree BIGGER, and only at shallow depth.
    //
    // Set to a depth to re-enable; if retried, the anchor belongs near the depth
    // where the node count actually is (2-4), not at 8.
    // ENABLED at 6. Third instance of the same pattern: the branch this selects
    // is the one its own comment argues for, and it shipped switched off.
    //
    // The improving discount is SUBTRACTED from the RFP margin, so a bigger
    // discount means RFP fires more readily -- correct, because a fail-high is
    // more credible when the position is already trending our way. Stockfish
    // shapes it the same way. What the flat form gets wrong is the slope: a
    // constant discount is a meaningful fraction of an 11-unit margin at depth 1
    // and negligible against a quadratic margin at depth 24, so the term
    // effectively evaporates exactly where the base is largest.
    //
    // That is the flat-term-on-a-scaled-base shape this codebase has been burned
    // by twice before -- 87 Elo in LMR's move-count term, ~60 in `threat_density`
    // -- and the fix was written, gated, and defaulted off.
    //
    // 6 is the reference depth: at depth 6 the new form reproduces the old
    // magnitude exactly, so nothing moves there and only the slope across depth
    // changes. RFP has no explicit depth cap; its quadratic margin makes it rare
    // at high depth, so it lives at low-to-mid depths and 6 sits inside that band.
    // Doubles the discount by depth 12, halves it by depth 3.
    //
    // Direction: more RFP pruning at depth when improving -> fewer nodes -> more
    // depth. Same side as the IIR and futility activations.
    i32 rfp_improvement_ref: 6;
    // Shrinks the RFP margin on a TT miss, proportionally to depth. 0 disables.
    i32 rfp_tt_miss: 0;
    i32 rfp_improvement: 120;
    i32 rfp_depth_lin: 22;
    i32 rfp_corr: 669;
    // Restored: present in 0.1.2 (`4135b69`), silently dropped since with no
    // comment explaining the removal. Feeds the RFP `opponent_worsening`
    // term restored alongside it in search.rs.
    i32 rfp_worsening: 20;
    i32 rfp_no_threats: 54;
    // Dynamic contextual processing: scale the RFP margin by how many of our
    // own pieces are currently attacked. `rfp_no_threats` above is the same
    // idea collapsed to a single bit -- it fires only when the count is zero,
    // so the two terms are mutually exclusive and cannot fight each other.
    //
    // A position with one loose knight and one with the queen, both rooks and
    // a bishop hanging are both merely "not empty" to a boolean, and got the
    // same treatment. They are not the same position. The more of our material
    // is under attack, the more likely a static eval is about to be refuted by
    // a capture the search has not seen yet, so widen the margin (prune less).
    //
    // The count is free: `all_threats` is already computed by update_threats
    // and `colors(stm)` is a field read, so this is one AND and one popcount.
    // Capped because past ~6 attacked pieces the signal stops discriminating.
        //
    // Shipped at 14, in the PROPORTIONAL form (see `threat_scaled` in search.rs).
    //
    // An earlier ADDITIVE version at this same value cost real strength, for a
    // reason that has nothing to do with whether threat density is a useful
    // signal: it was added as a FLAT
    // term to a base that scales with depth. The RFP margin is 11 units at
    // depth 1, 60 at depth 2, 727 at depth 8. A term contributing up to 84
    // therefore inflates the depth-1 margin by up to 764% and the depth-8
    // margin by 6% -- so RFP effectively stopped firing near the leaves, which
    // is where most nodes are.
    //
    // This is the same flat-term-on-a-scaled-base failure already documented in
    // this file for other constants. If it is revisited, the term has to scale
    // with the base it modifies, and that scaling has to be measured rather
    // than assumed -- an earlier attempt in this codebase to apply "constant
    // fraction of base" as a principle made things worse because the data said
    // the opposite. The signal itself is still worth testing; this shape of it
    // is not.
    i32 rfp_threat_density: 0;
    i32 threat_density_cap: 6;
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
    // See the ProbCut block in search.rs. Divided by 8192, the gravity bound
    // on `probcut_history`, so this is the full swing of the threshold between
    // "verification always agrees" and "verification never agrees".
    // DEFAULT 1 -- restored. The measurement that set it to 0 was noise.
    //
    // 1 = ProbCut may only cut when a verification search actually ran. At
    // depth <= 4 `base_depth` is 0, so no verification runs and the cutoff
    // cannot fire; the draft behind it is a qsearch capped at two moves
    // (`qs_move_cap`), and those cutoffs were being written to the TT as real
    // `Bound::Lower` entries at `probcut_depth + 1`.
    //
    // History, because it matters: this shipped at 1, was set to 0 partway
    // through a debugging session on the strength of a 132-pair match, and an
    // A/A test between two provably identical binaries later scored -15.8 Elo on
    // that same harness. 132 pairs cannot distinguish 20 Elo from zero, so the
    // evidence for setting it to 0 was never evidence at all.
    //
    // Everything else points the other way. Removing the old `depth >= 5` gate
    // "bought" ~1.9 plies, which is precisely what an unsound cutoff buys: depth
    // that was never searched. This fork has been chasing a "same depth, thinner
    // search" regression for its whole history, and an independent review
    // flagged this flag as that signature without knowing it had been changed.
    //
    // Set back to 0 to reproduce the unverified behaviour; the two are directly
    // comparable in one SPRT.
    i32 probcut_require_verify: 1;

    // Most non-checking moves qsearch will SEARCH before breaking out.
    //
    // Held at 3, the value both 0.2.0 and every later build shipped.
    //
    // The counter this is tested against changed meaning: it used to increment at
    // the top of the loop, so it counted moves GENERATED and a node whose first
    // captures were delta-pruned could break having searched none. It now
    // increments at `make_move`, so it counts moves SEARCHED.
    //
    // That looked like it needed a numeric compensation, and it does not. The
    // change is not uniformly looser -- it is differently shaped. In a clean node
    // the new form searches more; in a pruning-heavy node the OLD form gave up
    // after three generated moves having searched almost nothing, while the new
    // one keeps hunting until it has really looked at three. Cutting the number
    // to compensate took the second case below what it takes to see a tactic, and
    // conversion is exactly the job that needs one: an extra ply of defence is
    // worth little if the engine cannot find the capture that finishes a won
    // position.
    //
    // The depth cost is real (~0.45 ply against 0.2.0, worst in the endgame) and
    // is the price of the soundness fix. Buy it back with the exemptions below
    // and with the ordering weights, not by starving the budget.
    //
    // Defaults to the shipped 3 (i.e. at most two non-checking moves), so this
    // is inert until deliberately raised. It is the most aggressive single
    // number in the engine: it is what makes the ProbCut draft "a qsearch capped
    // at two moves", and it bought ~1.9 plies -- but plies bought by declining
    // to search are the exact signature this file warns about elsewhere. Exposed
    // so the tradeoff can be measured instead of assumed.
    i32 qs_move_cap: 3;

    // Exempt a recapture on the square the opponent just captured on from the
    // qsearch move cap, the way direct checks already are.
    //
    // The cap is the engine's most aggressive constant, and it is a cap on move
    // NUMBER, so it falls on whatever the picker happens to order third -- which
    // in a busy position can be the recapture that resolves the exchange. Cutting
    // there returns a stand-pat bound for a position with material hanging, which
    // is the textbook horizon effect and precisely what qsearch exists to stop.
    //
    // Checks are already exempt on the same reasoning: a move that must be
    // answered cannot be skipped just because it sorted late. A recapture on the
    // contested square has the same property.
    //
    // DEFAULT 0 -- shipped on, then switched off on the depth cost.
    //
    // Enabling it closed the defence gap against 0.2.0 from -11.9pp to -4.1pp,
    // which is what it was designed to do. It also grew the depth deficit from
    // -0.45 to -0.69 ply at identical time per move, and conversion moved from
    // -2.0pp to -8.8pp over the same two runs.
    //
    // Neither of those conversion figures is significant on its own (1.05 sigma
    // at n=47), so this is not a verdict -- it is a decision to hold the depth
    // while the more useful measurement is taken. The clean test is this build
    // against 2bb0bb0f, which differ by this flag alone.
    //
    // Set to 1 to re-enable. The mechanism is sound; what is unproven is whether
    // the tactical accuracy is worth a quarter-ply.
    i32 qs_recapture_exempt: 0;

    // Good captures emitted before the SEE retest is short-circuited.
    //
    // Defaults to the shipped 2. Above this count, with a quiet TT move, every
    // remaining capture skips SEE and is filed as `bad_noisy` -- where BNFP's
    // `skip_bad_noisy` can abandon it on a margin tuned for LOSING captures. A
    // winning capture (QxQ) can therefore go unsearched. Raising this keeps more
    // captures on the verified path.
    i32 good_noisy_cap: 2;

    // Continuation-history malus slope and ceiling.
    //
    // Shipped as `(414 * depth).min(949)`, which saturates at depth 2.3 -- so it
    // is a flat 949 across the whole useful depth range. Every sibling saturates
    // far later: cont_bonus at 11.3, quiet_bonus at 9.5, noisy_malus at 7.2,
    // quiet_malus at 6.4. A slope 4x steeper than its own bonus paired with the
    // lowest ceiling in the set looks like a transposed digit, but "looks wrong"
    // is a hypothesis, so the defaults reproduce current behaviour exactly.
    i32 cont_malus_slope: 414;
    i32 cont_malus_cap: 949;

    // ---- Move-ordering weights (movepick.rs) ----
    //
    // The engine tuned ~150 pruning constants and zero ordering constants, yet
    // ordering decides which of them ever get to fire: every margin here is
    // compared against `good_quiet_threshold`, and a mis-weighted term moves
    // moves across that line at every node. Defaults reproduce the shipped
    // values exactly, so exposing them changes nothing until SPSA runs.
    //
    // NOT exposed, deliberately: the check-evasion term (`200000 - 20000 * pt`).
    // It is not a weight competing with the learned terms -- it is a hard
    // ordering that must dominate them, and letting a tuner shrink it toward the
    // ~60k the other terms reach would let history reorder check evasions.
    i32 mp_noisy_mvv: 14232;
    i32 mp_noisy_queen_promo: 4558;
    i32 mp_quiet_hist_w: 1763;
    i32 mp_pawn_hist_w: 1024;
    i32 mp_gives_check: 10723;
    i32 mp_moves_into_threat: 8875;
    i32 mp_attacks: 3446;
    i32 mp_breaks_wall: 4494;

    // ---- History bonus/malus shape (search.rs) ----
    //
    // Each pair is the slope and ceiling of a `(slope * depth).min(cap)` term.
    // These decide how fast history learns and how hard it saturates, and every
    // move-ordering weight above is applied to their output -- so they set the
    // scale that `good_quiet_threshold` is compared against. All were hardcoded.
    //
    // Exposing them also makes the `cont_malus` anomaly measurable rather than
    // arguable: with these tunable, SPSA can be asked directly whether a
    // saturation depth of 2.3 is right when its five siblings sit at 6.4-11.3.
    i32 hist_noisy_malus_slope: 175;
    i32 hist_noisy_malus_cap: 1252;
    i32 hist_quiet_bonus_slope: 184;
    i32 hist_quiet_bonus_cap: 1742;
    i32 hist_quiet_malus_slope: 171;
    i32 hist_quiet_malus_cap: 1099;
    i32 hist_cont_bonus_slope: 97;
    i32 hist_cont_bonus_cap: 1098;

    // ---- Remaining single-number levers ----
    //
    // `lmr_win_beta` is a FULL PLY of reduction (1024/1024) applied whenever beta
    // is a win score, and it was hardcoded next to a dozen tuned reduction terms.
    // Of everything left unexposed it has the largest per-node swing.
    //
    // `see_split_div`/`see_split_base` set the dynamic good/bad noisy boundary --
    // the line that decides which captures reach the verified path at all, and so
    // which ones `good_noisy_cap` above can rescue.
    //
    // `rfp_tt_hist_gate` is the history floor below which a quiet TT move no
    // longer licenses an RFP cutoff; `asp_widen_num` is the aspiration widening
    // rate (`delta += n * delta / 128`), which sets how fast a failing window
    // gives up and how many re-searches each iteration costs.
    // HALVED from 1024 to 512.
    //
    // 1024/1024 is a FULL PLY of extra reduction whenever `is_win(beta)`. Note
    // which side that describes: `beta` is the bound we are trying to prove the
    // opponent cannot exceed, so `is_win(beta)` means the opponent is winning --
    // this fires hardest in exactly the lines where we are DEFENDING and need to
    // establish that a save exists.
    //
    // Pooled over three runs (324 pairs) against 0.2.0, defence is the one
    // sub-metric that clears significance: 25.5% vs 35.3%, -9.8pp, p = 0.020.
    // Conversion does not (-5.8pp, p = 0.15). And the depth deficit is now only
    // -0.24 ply after the move-count reduction terms were enabled, so this is no
    // longer explainable as "we search shallower" -- HEAD is losing saveable
    // positions at near-equal depth, which points at selectivity.
    //
    // Of every reduction term in the engine this is the only one keyed directly
    // on "someone is winning here", it was never measured, and a full ply is a
    // large amount to spend on a single boolean. 512 keeps the idea and halves
    // the dose.
    i32 lmr_win_beta: 512;
    i32 see_split_div: 47;
    i32 see_split_base: 116;
    i32 rfp_tt_hist_gate: -2048;
    i32 asp_widen_num: 26;
    i32 bnfp_recapture: 96;
    i32 qs_noisy_bonus: 100;
    i32 hist_noisy_bonus_slope: 96;
    i32 hist_noisy_bonus_cap: 885;

    // ---- History decay, prior-move shaping, LMR re-search, hindsight ----
    //
    // `quiet_malus_decay` is the rate at which malus falls off across the up-to-32
    // quiets punished at a fail-high (`denom = 1024 + n * i`, squared). It decides
    // whether the malus is concentrated on the first few refuted moves or spread
    // thin across all of them -- a shape question no other constant controls.
    //
    // `lmr_research_up`/`_down` adjust the re-search depth by comparing the
    // reduced score against `best_score`; they sit inside the LMR re-search that
    // runs at a large fraction of all interior nodes.
    //
    // `hindsight_*` gate the two retroactive corrections at the top of the node,
    // which act on the PARENT's reduction decision after the fact.
    i32 quiet_malus_decay: 45;
    i32 prior_malus_slope: 93;
    i32 prior_malus_cap: 935;
    i32 research_bonus_slope: 233;
    i32 research_bonus_cap: 1550;
    i32 lmr_research_up: 57;
    i32 lmr_research_down: 9;
    // LOWERED from 2249 to 1024, to balance the two hindsight arms.
    //
    // The block has one arm that gives a ply back (parent was heavily reduced AND
    // the eval declined) and one that takes a ply away (parent was reduced at all
    // AND the eval improved). Their bars were wildly different: 2249 -- about 2.2
    // plies of prior reduction -- to regain, against `reduction > 0`, any
    // reduction whatsoever, to lose. Easy to lose a ply, hard to win one back.
    //
    // The give-back arm is also the one gated on `eval_delta < 0`, i.e. it only
    // fires when the position is DECLINING. That is the defensive case, and
    // defence is the one sub-metric significantly worse than 0.2.0 across three
    // pooled runs (25.5% vs 35.3%, p = 0.020) now that the depth deficit is
    // nearly closed.
    //
    // 1024 -- one full ply of prior reduction -- keeps the arm meaningful (it is
    // still not free) while bringing it into the same order as its opposite.
    // MODERATED to 1536 after 1024 proved too explosive.
    //
    // The two hindsight arms had wildly mismatched bars -- 2249 (2.2 plies of
    // prior reduction) to regain a ply, against any reduction at all to lose one.
    // Correcting that asymmetry fixed the defence deficit. Taking it all the way
    // to 1024 also meant the give-back arm fired at nearly every declining node
    // whose parent reduced by a single ply, which is a very large fraction of a
    // losing subtree -- and each firing extends, so the tree grew and depth fell
    // sharply.
    //
    // 1536 keeps the arm reachable (it was effectively dormant at 2249) without
    // making it near-unconditional in exactly the positions that already search
    // widest.
    i32 hindsight_reduction: 1536;
    i32 hindsight_eval_delta: 57;

    // ---- Prior-move credit (update_prior_move_histories) ----
    //
    // When a node fails low, the move that led INTO it gets credited or blamed.
    // `prior_f_*` are the three conditions that scale that credit -- the prior
    // move was the parent's TT move, the fail-low was decisive, the opponent's
    // eval worsened -- and they multiply a bonus applied one and two plies back.
    //
    // This is the only mechanism that propagates a result backwards past the
    // immediate parent, so its scale interacts with every conthist lag weight.
    i32 prior_f_tt_move: 110;
    i32 prior_f_fail_low: 144;
    i32 prior_f_worsening: 306;
    i32 prior_bonus_slope: 180;
    i32 prior_bonus_cap: 2414;
    i32 prior_lag2_slope: 152;
    i32 prior_lag2_cap: 1379;
    i32 prior_noisy_slope: 50;
    i32 prior_noisy_cap: 654;

    // ---- Escape bonuses, aspiration seed, FDS quantisation, shuffle guard ----
    //
    // `escape_*` reward moving a THREATENED piece of that type to safety. They
    // are the largest positional terms in quiet ordering (queen 20357 against a
    // ~60k total) and were the last hardcoded block in `QuietContext`. King is
    // absent by construction -- a king in check is handled by evasion ordering,
    // not by this table -- so only five entries exist.
    //
    // `asp_delta_*` seed the aspiration window from two stability counters that
    // ALSO drive the time multiplier: one signal, three consumers, coefficients
    // set independently. Exposing the seed lets that coupling be measured.
    //
    // `fds_reduction_t*` are the two thresholds at which FDS spends a whole ply,
    // while LMR divides the same 1024-scaled quantity continuously. Same units,
    // two different quantisations -- worth knowing whether the steps are placed
    // right.
    i32 shuffle_null_guard: 6;
    i32 asp_delta_base: 23;
    i32 asp_delta_stab_cap: 7;
    i32 asp_delta_floor: 10;
    i32 fds_reduction_t1: 2621;
    i32 fds_reduction_t2: 5579;
    // A threatened PAWN now earns something for stepping away.
    //
    // The escape table is [pawn, knight, bishop, rook, queen] and every entry but
    // this one is live: 8854 for a knight, 8170 for a bishop, 14051 for a rook,
    // 20357 for a queen. At 0 a pawn-saving quiet sorted below essentially every
    // other quiet in the position, which is not a tuning choice -- it is the one
    // hole in an otherwise complete table.
    //
    // 2400 keeps the table monotone in piece value (pawn < bishop < knight <
    // rook < queen) rather than guessing: it is roughly the same fraction of a
    // knight's bonus that a pawn is of a knight's value. The range is wide, so
    // SPSA can settle it now that it is not pinned at an endpoint.
    i32 escape_pawn: 2400;
    i32 escape_knight: 8854;
    i32 escape_bishop: 8170;
    i32 escape_rook: 14051;
    i32 escape_queen: 20357;

    // ---- LMR improvement clamp, IIR trigger ----
    //
    // `lmr_improvement_*` were the only clamp on `improvement` anywhere in the
    // engine -- RFP, NMP and LMP all consume it raw. Whoever wrote this knew the
    // range was dangerous enough to bound HERE, which makes the bound itself the
    // interesting number: it encodes how far the signal is trusted.
    //
    // `iir_tt_depth_slack` extends IIR to the case Stockfish also covers: a TT
    // entry whose depth is far below the current depth is nearly as uninformative
    // as no entry at all, and it is a much more common case than a missing move.
    // DEFAULT 0 = disabled, i.e. today's behaviour exactly (fires only on a null
    // TT move). Set it to e.g. 4 to also reduce when `tt_depth + 4 < depth`.
    i32 lmr_improvement_lo: -241;
    i32 lmr_improvement_hi: 1155;

    // FDS's own copy of the improvement clamp. Written inline as (-206, 1370)
    // while its LMR twin used tunables -- two forms of the same bound, one of
    // them unreachable by SPSA. Defaults are the literals it shipped with, NOT
    // the LMR values: the two were never the same number and making them so
    // would be a behaviour change wearing a refactor's clothes.
    i32 fds_improvement_lo: -206;
    i32 fds_improvement_hi: 1370;

    // Bound on the RAW improvement signal, applied where it is computed.
    //
    // `lmr_improvement_lo/hi` clamps the SCALED term in LMR and was the only
    // bound on this signal anywhere; RFP, NMP, LMP and FDS all consumed
    // `eval - stack[ply-2].eval` raw. That difference is unbounded, and a
    // tactical swing puts `lmp_improvement * improvement / 16` in the tens of
    // thousands against an `lmp_base` of 2818 -- which does not merely loosen
    // late move pruning, it switches it off at the node where the eval just
    // moved most.
    //
    // Set wide on purpose. +-2048 is about five pawns of eval swing between two
    // of our own moves; inside that range nothing changes, so this clips the
    // pathological tail without retuning the five consumers that were fitted
    // against the normal range. Narrowing it is a separate, tunable question.
    i32 improvement_lo: -2048;
    i32 improvement_hi: 2048;
    i32 iir_depth: 6;
    // ENABLED at 4 -- IIR was restricted to its rarest trigger.
    //
    // At 0 the condition collapses to `tt_move.is_null()`, so internal iterative
    // reduction only fired when the table had no move at all. The far more common
    // case -- an entry whose search was much shallower than this one, and so tells
    // us little more than nothing about which move to try first -- never fired.
    // Stockfish covers both.
    //
    // 4 means "the cached move came from a search at least 4 plies shallower".
    // That is a real mechanism activation rather than a tuning nudge: IIR is one
    // of the cheapest ways to buy depth, and this engine was running it on a
    // fraction of the nodes it was written for.
    //
    // Coupled change: `lmr_iir_comp`/`fds_iir_comp` were gated on
    // `tt_move.is_null() && iir_applied`. That was redundant while IIR implied a
    // null TT move and would have become wrong here -- the new firings reduce
    // `depth` identically and need the same compensation. Both now track
    // `iir_applied` alone.
    i32 iir_tt_depth_slack: 4;

    // ---- TT-cutoff credit, and the per-sibling decay rates ----
    //
    // `ttcut_*` reward the TT move when it produces a cutoff without any search
    // at this node. That is the cheapest fail-high in the engine and the most
    // frequent, so its credit sets the baseline every searched fail-high is
    // measured against -- and it was entirely hardcoded.
    //
    // `*_decay` subtract per already-tried sibling, so they control how the
    // penalty spreads across the refuted moves at a fail-high: a large value
    // concentrates blame on the first few, a small one spreads it evenly. The
    // `*_cut` terms discount the bonus at cut nodes, where the fail-high was
    // expected and therefore carries less information.
    i32 ttcut_quiet_slope: 190;
    i32 ttcut_quiet_cap: 1691;
    i32 ttcut_cont_slope: 96;
    i32 ttcut_cont_cap: 1206;
    i32 hist_noisy_bonus_cut: 87;
    i32 hist_noisy_malus_decay: 16;
    i32 hist_quiet_bonus_cut: 42;

    // The non-PV late-move bonus scale and its ceiling.
    //
    // `+ (18 * (move_count - 1)).min(180) * !PV` -- a move that proved best after
    // many others were tried is more informative than one that was tried first,
    // so at non-PV nodes the bonus scales with how much work preceded it. Sound
    // idea, taken from Stockfish, and the only term in this function that SPSA
    // could not reach: every one of its neighbours is a `p::` parameter and these
    // two were literals.
    i32 hist_quiet_late_scale: 18;
    i32 hist_quiet_late_cap: 180;
    i32 hist_quiet_malus_decay: 31;
    i32 hist_cont_bonus_cut: 48;
    i32 hist_cont_malus_decay: 17;
    i32 probcut_hist: 40;
    i32 probcut_hist_bonus: 190;
    i32 probcut_hist_malus: 130;
    i32 probcut_score_div: 319;
    i32 probcut_beta_step: 197;

    // Late Move Pruning
    // ZEROED -- measured and reverted alongside `fp_lmr_depth`; see there.
    //
    // Artemis divides its LMP threshold by `(2 - improving)`, halving it when
    // not improving. Porting that ratio ignored the baselines being different:
    // Wreckless's threshold at depth 8 is ~87 moves against Artemis's 33-67, so
    // the same halving is a far larger relative change here than there. That is
    // the same anchoring mistake as `rfp_improvement_ref` -- porting a ratio
    // without checking the quantity it is a ratio OF.
    //
    // 512 reproduces Artemis's halving; 0 disables and restores
    // `lmp_improvement` as the sole improving term.
    i32 lmp_improving_mult: 0;
    i32 lmp_base: 2818;
    // Restored to its tuned value: `lmp_improving_mult` is now 0, so this is
    // again the only improving term in the LMP threshold. If that is ever
    // re-enabled, this must go back to 0 -- both live would penalise `improving`
    // twice in one threshold.
    i32 lmp_improvement: 78;
    i32 lmp_quad: 1351;
    i32 lmp_history: 74;

    // Futility Pruning
    // ZEROED -- measured and reverted. 1 = futility uses the LMR-adjusted depth
    // (Artemis/Stockfish `lmrDepth`), 0 = raw depth.
    //
    // Shipped together with `lmp_improving_mult` in 3631a08a. That build fully
    // recovered the depth lost to `rfp_improvement_ref` (-1.32 -> -0.30 ply,
    // matching the pre-batch build) yet stayed at -108.7 Elo against -54.5
    // before the batch (z = -1.93, p = 0.053, corroborated by 07bbebb7 landing
    // at -109.5 over 59 pairs with the same two terms live).
    //
    // Depth and Elo therefore had DIFFERENT causes: `rfp_improvement_ref` cost
    // the depth, and one of these two cost the strength. Both prune more, which
    // is why neither shows up as lost depth.
    // ENABLED. The branch this switch selects IS the fix its own comment argues
    // for, and it shipped turned off.
    //
    // Futility measures how far a move is from raising alpha, and a move about to
    // be heavily reduced by LMR is effectively searched shallower than `depth`
    // says. Stockfish computes futility on `lmrDepth` for exactly this reason.
    // The comment at the branch explains that subtracting `r / 1024` as whole
    // plies rounds to zero at every depth futility runs at (`depth < 14`) -- so
    // the naive form is a no-op, and scaling by `(depth * 1024 - r)` is what keeps
    // the fraction. That corrected form is the `> 0` branch.
    //
    // Direction: smaller `fp_scaled_depth` -> smaller `futility_value` -> the
    // `<= alpha` test passes more often -> more pruning, fewer nodes, more depth.
    // Same side of the ledger as the IIR activation.
    //
    // NOTE this is a boolean wearing an i32. Only `> 0` vs `== 0` reaches the
    // arithmetic; the magnitude is never read. It should not be given a wide SPSA
    // range -- the tuner would spend a dimension discovering a single bit.
    i32 fp_lmr_depth: 1;
    i32 fp_depth: 79;
    i32 fp_history: 55;
    i32 fp_beta_bonus: 77;
    i32 fp_corr: 555;
    // Same contextual signal applied to futility pruning; see
    // `rfp_threat_density`. Futility prunes quiets at shallow depth on the
    // assumption that a quiet move cannot recover a large deficit -- an
    // assumption that is weakest exactly when our pieces are hanging, because
    // the "quiet" move may be the one that saves the piece.
    // Shipped at 20, proportional form; see `rfp_threat_density`.
    //
    // The additive version was worse here than there: at depth 1 the
    // `fp_depth * depth` term was -48 and this contributed up to +120, so it did
    // not merely inflate the margin, it dominated and flipped the expression's
    // sign. That specific hazard is gone -- the term is proportional now and
    // `threat_scaled` returns a non-positive base untouched -- but it is why the
    // additive form is not coming back.
    i32 fp_threat_density: 0;
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
    //
    // FIXED with d^2/64 scaling in search.rs. As a flat constant 48, this was
    // 69.6% of the base at depth 6, 16.4% at depth 8, and 0.9% at depth 24 --
    // the same flat-term-on-quadratic-base defect that cost movecount_ilog 87 Elo.
    // Volatility analysis shows excess eval at mover's depth 8-23 (ratio 1.15-1.19),
    // absent at 24+ (1.00). The d^2/64 scaling holds the term at constant ~8%
    // of its base throughout (effective quad coeff 12 -> 11.25).
    i32 see_q_cutoff: 48;
    i32 see_q_base: 27;
    i32 see_n_quad: 7;
    i32 see_n_lin: 36;
    i32 see_n_hist: 39;
    // Unchanged for the same reason as `see_q_cutoff` above.
    i32 see_n_cutoff: 37;
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
    // ENABLED at 192. At 0 the reduction had no move-count component at all --
    // move 3 and move 40 were reduced identically, which is the single most
    // standard component of late move reduction and the most conspicuous
    // absence in this engine relative to every reference implementation.
    //
    // The evidence for zeroing it was two positive results at 0 and one negative
    // at 192 across three different harnesses -- gathered on a setup whose A/A
    // test later scored two identical binaries at -15.8 Elo. That is not
    // evidence either way.
    //
    // Multiplicative form: `coeff * depth.ilog2() * move_count.ilog2() / 16`, so
    // ~0.23 ply at depth 16 / move 32 -- proportional to the reduction already
    // being applied, not a flat addition. It also REDUCES nodes, which is the
    // direction the measured -0.69 ply depth deficit needs.
    // RAISED to 256 to pay for the defensive extensions above.
    //
    // `lmr_win_beta` and `hindsight_reduction` both restore search effort in
    // declining positions, which fixed defence and cost depth. The nodes have to
    // come back from somewhere, and late moves are the right place: this term
    // scales with `move_count.ilog2()`, so it bites hardest on the 20th move at a
    // node and barely at all on the 3rd. A move ordered 20th is not where a
    // defensive resource hides -- the picker puts checks, captures and
    // history-favoured quiets first.
    //
    // Reducing late moves harder to fund extending critical ones is the trade
    // late move reduction exists to make; the engine simply was not making it at
    // all until this term was enabled.
    // RE-ENABLED at 24 -- INSIDE its declared range, which 192 and 256 were not.
    //
    // The SPSA range for this parameter is [0, 48]. It was set by whoever added
    // the parameter, with knowledge of the formula it feeds. 192 and 256 sit four
    // and five times above the top of it: the default was raised without ever
    // checking the bounds, and nothing in the codebase enforces that a default
    // lies inside its own range.
    //
    // In plies, at depth 16 / move 32, `coeff * ilog2(d) * ilog2(mc) / 16`:
    //     48  -> 0.06 ply
    //     256 -> 0.31 ply
    // So the shipped value was reducing late moves by five times the intended
    // maximum -- which is precisely the shape that loses the narrow resources
    // that hold inferior positions.
    //
    // 24 is the middle of the declared range: enabled, and at a magnitude the
    // parameter was designed for.
    //
    // These five were switched off on the argument that they layer new terms onto
    // a reduction formula tuned with them at zero -- sound reasoning, but it was
    // reasoning, not a measurement. The build that carried them ON measured
    // -5.9 +/- 41.0 against 1.0.0; the build with them OFF measured -25.7 +/- 42.3.
    // That difference is 0.66 sigma, so it proves nothing -- but it is the only
    // empirical signal either way, and it points at keeping them.
    //
    // The code corrections made in the same window (razoring/ProbCut gates, the
    // reduction publish, `write_move`'s CAS) are NOT reverted with them: those
    // rest on an argument that does not depend on Elo -- `!is_quiet()` is
    // literally Stockfish's `!(ttMove && !ttMove.isCapture())`, and a lost-update
    // on a TT entry is wrong however it measures.
    i32 lmr_movecount_ilog: 24;
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
    // Cap on double/triple singular extensions accumulated along one line.
    //
    // Neither this fork nor upstream tracked the cumulative total, so a
    // tactical line could keep taking the +2/+3 tiers with nothing but MAX_PLY
    // to stop it -- that bounds the recursion, not the tree. Stockfish and
    // Berserk both gate their upper tiers this way (`ss->doubleExtensions`,
    // `ss->de <= 6/7`).
    //
    // 8 is deliberately loose: a backstop against runaway lines, not a tuning
    // knob. Ordinary singular behaviour is unchanged because the base +1 does
    // not consume budget -- only the tiers above it do.
    // NOTE the sense of this one is INVERTED relative to every other gate in
    // this file. It is a ceiling on accumulated double/triple extensions along a
    // line, so 0 does NOT disable it -- `de < 0` is never true, which removes
    // double and triple extensions entirely, the maximum restriction rather than
    // none. To disable the gate, set it LARGE (9999).
    //
    // Worth being careful with: bench measures the gate at 12.73% of nodes
    // (2,708,280 with it, 3,053,066 without at depth 12), so both extremes move
    // the tree substantially. The SPSA range floor is 4 for this reason;
    // `set_parameter` enforces nothing, so a typed 0 would still land.
    i32 max_double_extensions: 8;
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
    // ENABLED at 192, matching its LMR twin above; same form, same reasoning.
    // RAISED to 256, matching its LMR twin above; same form, same reasoning.
    i32 fds_movecount_ilog: 18;
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
    // 1.0.0's value, restored after being wrongly reverted to 0.2.0's 2496.
    //
    // This and `lowply_weight`, `tm_trend_diff`, `tm_trend_recent` are the ONLY
    // four tuning values that differ between 0.2.0 and 1.0.0 (`corr_weight_div`
    // is the fifth and is a normalization fix, not tuning). They were reverted on
    // the theory that they were unexplained drift since 0.2.0 -- which had the
    // direction exactly backwards: 1.0.0 measures ~17-20 Elo ABOVE 0.2.0, so
    // these are its tuning gains, not drift away from a good baseline.
    //
    // The symptom that exposed it: reverting them brought HEAD to parity with
    // 0.2.0 while leaving it ~20 Elo below 1.0.0 -- exactly what removing 1.0.0's
    // tuning would do.
    //
    // 1.0.0 is the strongest build in this lineage and is the baseline worth
    // testing against.
    // 1.0.0's values -- the strongest baseline in this lineage.
    //
    // These four are the only tuning values separating 0.2.0 from 1.0.0
    // (`corr_weight_div` is the fifth and stays at 76 -- it is a normalization
    // fix, correct in both lineages, and reverting it would reintroduce a 19%
    // correction inflation).
    //
    //     param              0.2.0    1.0.0
    //     corr_bonus_min      2496     4678
    //     lowply_weight       3018     7052
    //     tm_trend_diff         56      480
    //     tm_trend_recent       27      230
    //
    // 1.0.0 measures ~17-20 Elo above 0.2.0, so these are 1.0.0's tuning gains.
    // The open question is whether they still transfer: HEAD's search differs
    // materially from 1.0.0's -- `lmr_win_beta` halved, hindsight rebalanced, and
    // four mechanisms activated that were dormant then -- and parameters do not
    // automatically survive a changed search.
    //
    // Flip all four to the 0.2.0 column to test the transfer question again.
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
    // 700. A halving to 350 was tried, on the argument that the write should
    // mirror `search()`'s 1/2 read weight. That argument does not survive
    // checking: the two read sites already disagree on this lag (search 0.50,
    // `score_quiet` 963/1024 = 0.94), so there is no single read weight to
    // match -- and write weight (how fast an entry fills) is a different
    // quantity from read weight (how much a filled entry counts). They are
    // independent scalings, not a correspondence. Reverted to the measured
    // value; the comment in search.rs claiming the two mirror each other is the
    // thing that was wrong, not this number.
    i32 conthist_lag5: 89;
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
    // anchor's arithmetic. Both cuts were later reverted; the shipped value is
    // 7052 and the paragraph below explains why.
    //
    // The arithmetic distinction that second correction turned on no longer
    // exists either: it separated upstream's 1614 from this fork's 1479, and
    // `CONTHIST_WEIGHTS[0]` is now 1614 again, since dropping the two dead lags
    // put the four survivors back on upstream's exact weights.
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
    // Deliberately NOT rescaled during the `lowply_weight` 7052 -> 3018 episode
    // (since reverted; see above),
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
    // Converts an eval-unit deficit into the material units SEE compares
    // against, in `(alpha - eval) / qs_see_div`.
    //
    // The true ratio is ~2.95: `PieceType::value()` calls a pawn 109 while the
    // search's own units put one at 321-382. Twenty lines above this consumer,
    // `qs_delta_piece_scale` (192/64 = 3.0) converts the SAME ratio in the other
    // direction, deriving 3.0 from exactly that argument -- so the two
    // conversions in one function disagreed by 2.7x.
    //
    // LEFT AT 8 pending measurement. The unit analysis says 3, and dividing by 8
    // shrinks the allowance 2.7x, making the qsearch SEE threshold far less
    // negative and pruning captures that would comfortably survive at the correct
    // scale -- in qsearch, which is most of the tree. But `qs_see_base` (74) was
    // tuned against the composite, so moving the divisor alone rescales a
    // threshold two other terms were fitted to. Change it WITH `qs_see_base` as
    // one SPRT, not on the strength of the derivation.
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
    // Evaluation blend; see `correct_eval`. The first eval-side parameters this
    // engine has ever exposed to tuning. The divisor is intentionally absent --
    // it sets the units every search margin is tuned in.
    i32 eval_material_base: 21032;
    i32 eval_optimism_base: 1548;
    // Fiftymove damping. Defaults reproduce the shipped `(200 - clock) / 200`;
    // Stockfish's equivalent is offset 200 with divisor 214.
    i32 eval_fifty_offset: 200;
    i32 eval_fifty_div: 200;

    // Master switch for Lazy SMP depth differentiation (see the iterative
    // deepening loop). 0 restores the previous behaviour, where every helper
    // thread searched every depth. Not a magnitude -- purely on/off, so that a
    // single SPRT can settle whether the schedule helps this engine.
    // ENABLED. At 0 the entire depth-differentiation schedule below was dead:
    // every helper walked thread 0's exact depth sequence, so the only thing
    // separating threads was LMR/FDS jitter -- which perturbs WHICH lines get
    // reduced, not when a thread arrives at a depth. Threads reached the same
    // iteration at the same time and re-derived the same result, which is
    // precisely the failure the schedule exists to prevent.
    //
    // Only observable at Threads > 1, so it is orthogonal to any single-threaded
    // SPRT and can be tested alongside other changes without confounding them.
    //
    // Prerequisite fixed first: `iter_values` was indexed by absolute depth,
    // which becomes an 8-12 ply lookback on a skipping helper. It now counts
    // completed iterations.
    i32 lazy_smp_skip: 1;

    // Stop the search once a forced mate this short is proven and has been
    // confirmed for `tm_mate_confirm` plies of extra depth. Set to 0 to retire.
    i32 tm_mate_moves: 5;
    i32 tm_mate_confirm: 2;
    // Extend when the root fails low; see the iterative deepening loop.
    // Applied as 1 + tm_fail_low/1000 * min(fail_lows, cap), so 250 means the
    // first fail-low buys 25% more time. Lower bound 0 retires it.
    i32 tm_fail_low: 125;
    i32 tm_fail_low_cap: 2;
    // Depth at which a forced single reply stops the search. 0 disables.
    i32 tm_single_move_depth: 8;
    i32 tm_trend_base: 7426;
    i32 tm_trend_diff: 480;
    i32 tm_trend_recent: 230;
    i32 tm_trend_min: 7214;
    i32 tm_trend_max: 14031;

}