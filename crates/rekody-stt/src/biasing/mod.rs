//! Engine-level dictionary term biasing for Nemotron streaming (issue #91).
//!
//! Spec: `docs/design/nemotron-term-biasing-spec.md`, section 5 steps 2 and 3.
//!
//! Shallow fusion on the greedy decode path: [`sp`] encodes each dictionary
//! term into SentencePiece token sequences, [`TermBias`] holds a prefix trie
//! over those sequences and implements the decode-loop hook. At every inner
//! decode step the processor adds `boost` to the logits of tokens that start
//! or continue a term, but only when the token is already acoustically
//! competitive (within `margin` of the step's best logit). Emitted tokens
//! advance the trie state; a non-matching emit simply empties it, with no
//! penalties and no retraction needed. Model weights are never touched, and
//! nothing here runs unless the (default off) `term_biasing` config flag
//! installs a processor at engine setup.

pub mod sp;

use std::collections::{HashMap, HashSet};

/// The decode-loop hook trait from the pinned parakeet-rs fork (spec
/// section 5, step 1), re-exported so engine wiring can name it from one
/// place: `process` runs before the argmax, `on_emit` after a non-blank
/// token commits, `on_reset` on `Nemotron::reset`. [`TermBias`] implements
/// it and installs via `Nemotron::set_logits_processor`.
pub use parakeet_rs::LogitsProcessor;

/// Hard cap on concurrent partial-match states (spec guard 5). When more
/// states survive an emit, the freshest shallow state is dropped first.
pub const MAX_ACTIVE_STATES: usize = 16;

/// Tunables for decode-time term biasing. Defaults are the spec's starting
/// points, chosen from the shallow-fusion literature; they must be re-tuned
/// on the shipped int8 artifact with the section 6 harness before the
/// feature flag may default on.
#[derive(Debug, Clone)]
pub struct BiasSettings {
    /// Dictionary terms to bias toward, in file order.
    pub terms: Vec<String>,
    /// Logits added per matched token at depth 1. Default 3.0.
    pub boost: f32,
    /// A candidate is only boosted when its logit is within this distance
    /// of the step's best logit. Default 6.0.
    pub margin: f32,
    /// Boost multiplier for tokens at trie depth >= 2, so a started phrase
    /// is carried to completion instead of dying halfway. Default 1.5.
    pub depth_factor: f32,
    /// Hard cap on the number of dictionary terms used. Default 200.
    pub max_terms: usize,
}

impl Default for BiasSettings {
    fn default() -> Self {
        Self {
            terms: Vec::new(),
            boost: 3.0,
            margin: 6.0,
            depth_factor: 1.5,
            max_terms: 200,
        }
    }
}

/// One trie node. Node 0 is the root; children are `(token id, node index)`.
#[derive(Debug)]
struct Node {
    children: Vec<(usize, u32)>,
    terminal: bool,
    depth: u16,
}

/// Prefix trie over the token sequences of the dictionary terms, plus the
/// per-utterance matching state. Implements [`LogitsProcessor`].
pub struct TermBias {
    /// Node 0 is the root.
    nodes: Vec<Node>,
    /// Current partial-match states (root implicit, always active).
    active: Vec<u32>,
    cfg: BiasSettings,
    /// Completed term matches this utterance, for logging.
    hits: u64,
    /// Token ids never boosted (language tags on the multilingual variant).
    excluded: HashSet<usize>,
}

impl TermBias {
    /// Build from settings plus a parsed tokenizer: truncate the term list
    /// to `max_terms` (with a warning), encode up to three casing variants
    /// per term (spec step 2.3), and exclude language-tag ids from ever
    /// being boosted. Returns `None` when nothing is encodable so the
    /// caller installs no processor at all: disabled means the decode loop
    /// pays nothing beyond an `Option` check.
    pub fn build(sp_model: &sp::SpModel, cfg: BiasSettings) -> Option<Self> {
        if cfg.terms.is_empty() {
            return None;
        }
        let keep = cfg.terms.len().min(cfg.max_terms);
        if keep < cfg.terms.len() {
            tracing::warn!(
                terms = cfg.terms.len(),
                max_terms = cfg.max_terms,
                "dictionary exceeds the term biasing cap; extra terms ignored"
            );
        }
        let mut paths: Vec<Vec<usize>> = Vec::new();
        for term in &cfg.terms[..keep] {
            paths.extend(sp_model.encode_term_variants(term));
        }
        Self::from_paths(cfg, paths, sp_model.lang_tag_ids())
    }

    /// Lower-level constructor over already-encoded token paths. Used by
    /// [`TermBias::build`] and by tests; also the seam where n-best
    /// segmentations per term would plug in (spec risk 3), since the trie
    /// absorbs extra paths at trivial cost. Paths containing an excluded id
    /// are dropped; empty paths are ignored. `None` when no usable path
    /// remains.
    pub fn from_paths(
        cfg: BiasSettings,
        paths: Vec<Vec<usize>>,
        excluded: Vec<usize>,
    ) -> Option<Self> {
        let excluded: HashSet<usize> = excluded.into_iter().collect();
        let mut nodes = vec![Node {
            children: Vec::new(),
            terminal: false,
            depth: 0,
        }];
        let mut inserted = false;
        for path in &paths {
            if path.is_empty() || path.iter().any(|token| excluded.contains(token)) {
                continue;
            }
            insert_path(&mut nodes, path);
            inserted = true;
        }
        inserted.then(|| Self {
            nodes,
            active: Vec::new(),
            cfg,
            hits: 0,
            excluded,
        })
    }

    /// Completed term matches in the current utterance (`bias_hits`).
    pub fn hits(&self) -> u64 {
        self.hits
    }

    fn child(&self, node: u32, token: usize) -> Option<u32> {
        self.nodes[node as usize]
            .children
            .iter()
            .find_map(|&(t, c)| (t == token).then_some(c))
    }
}

fn insert_path(nodes: &mut Vec<Node>, path: &[usize]) {
    let mut cur = 0u32;
    for &token in path {
        let existing = nodes[cur as usize]
            .children
            .iter()
            .find_map(|&(t, c)| (t == token).then_some(c));
        cur = match existing {
            Some(next) => next,
            None => {
                let depth = nodes[cur as usize].depth.saturating_add(1);
                nodes.push(Node {
                    children: Vec::new(),
                    terminal: false,
                    depth,
                });
                let next = (nodes.len() - 1) as u32;
                nodes[cur as usize].children.push((token, next));
                next
            }
        };
    }
    nodes[cur as usize].terminal = true;
}

impl LogitsProcessor for TermBias {
    fn process(&mut self, logits: &mut [f32], blank_id: usize) {
        if logits.is_empty() {
            return;
        }

        // One pass for the step's best logit (blank included: the margin is
        // measured against whatever currently wins the step).
        let best = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let floor = best - self.cfg.margin;

        // Candidate set: children of the root (term starts) plus children of
        // every active state (continuations). When several states propose
        // the same token, keep the single largest boost, never a sum.
        let mut proposals: HashMap<usize, f32> = HashMap::new();
        for state in std::iter::once(0u32).chain(self.active.iter().copied()) {
            for &(token, child) in &self.nodes[state as usize].children {
                let depth = self.nodes[child as usize].depth;
                // TurboBias-style depth weighting: on a greedy decode,
                // deeper transitions need more push so a started phrase can
                // be carried through (the factor is flat, not compounding).
                let factor = if depth >= 2 {
                    self.cfg.depth_factor
                } else {
                    1.0
                };
                let boost = self.cfg.boost * factor;
                let slot = proposals.entry(token).or_insert(f32::NEG_INFINITY);
                if boost > *slot {
                    *slot = boost;
                }
            }
        }

        for (token, boost) in proposals {
            // Guard list (spec section 5): the blank logit is never touched,
            // ids at or past blank are outside the real vocabulary, and
            // language-tag ids are excluded outright.
            if token >= blank_id || token >= logits.len() || self.excluded.contains(&token) {
                continue;
            }
            // Margin gate on the unmodified value: a token acoustically out
            // of the running is never boosted, so terms cannot materialize
            // out of nothing.
            if logits[token] < floor {
                continue;
            }
            logits[token] += boost;
        }
    }

    fn on_emit(&mut self, token: usize) {
        // Advance every active partial match, then consider a fresh start
        // from the root. At the state cap the root start is dropped first,
        // keeping the deeper partial matches alive. A terminal child counts
        // a hit; it stays active only if it has children of its own (a term
        // that is a prefix of another term keeps matching). If the emitted
        // token continues nothing, the set naturally empties.
        let mut next: Vec<u32> = Vec::with_capacity(self.active.len() + 1);
        let mut new_hits = 0u64;
        for state in self.active.iter().copied().chain(std::iter::once(0u32)) {
            let Some(child) = self.child(state, token) else {
                continue;
            };
            if self.nodes[child as usize].terminal {
                new_hits += 1;
            }
            if !self.nodes[child as usize].children.is_empty() && !next.contains(&child) {
                next.push(child);
            }
        }
        next.truncate(MAX_ACTIVE_STATES);
        self.hits += new_hits;
        self.active = next;
    }

    fn on_reset(&mut self) {
        tracing::debug!(bias_hits = self.hits, "term biasing utterance reset");
        self.active.clear();
        self.hits = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::sp::testutil as tu;
    use super::*;

    /// Logits length 11: token ids 0..=9 plus blank at index 10, matching
    /// the engine's `blank_id = vocab_size` layout.
    const BLANK: usize = 10;

    fn bias_from(paths: &[&[usize]]) -> TermBias {
        TermBias::from_paths(
            BiasSettings::default(),
            paths.iter().map(|p| p.to_vec()).collect(),
            vec![],
        )
        .expect("paths must build a trie")
    }

    fn logits_of(len: usize, value: f32) -> Vec<f32> {
        vec![value; len]
    }

    #[test]
    fn default_settings_match_spec() {
        let cfg = BiasSettings::default();
        assert!(cfg.terms.is_empty());
        assert_eq!(cfg.boost, 3.0);
        assert_eq!(cfg.margin, 6.0);
        assert_eq!(cfg.depth_factor, 1.5);
        assert_eq!(cfg.max_terms, 200);
    }

    // ----- trie construction -----

    #[test]
    fn trie_shares_prefixes() {
        let bias = bias_from(&[&[1, 2, 3], &[1, 2, 4]]);
        // root + shared [1], [1,2] + two leaves = 5 nodes.
        assert_eq!(bias.nodes.len(), 5);
        assert_eq!(bias.nodes[0].children.len(), 1);
        let d3: Vec<u16> = bias.nodes.iter().map(|n| n.depth).collect();
        assert_eq!(d3, vec![0, 1, 2, 3, 3]);
    }

    #[test]
    fn empty_or_unusable_paths_yield_none() {
        assert!(TermBias::from_paths(BiasSettings::default(), vec![], vec![]).is_none());
        assert!(TermBias::from_paths(BiasSettings::default(), vec![vec![]], vec![]).is_none());
        // Every path contains an excluded id.
        assert!(TermBias::from_paths(BiasSettings::default(), vec![vec![4]], vec![4]).is_none());
    }

    #[test]
    fn paths_with_excluded_ids_are_dropped() {
        let bias =
            TermBias::from_paths(BiasSettings::default(), vec![vec![4], vec![8]], vec![4]).unwrap();
        let roots: Vec<usize> = bias.nodes[0].children.iter().map(|&(t, _)| t).collect();
        assert_eq!(roots, vec![8]);
    }

    // ----- process: boost, margin, guards -----

    #[test]
    fn boost_applies_only_within_margin() {
        let mut bias = bias_from(&[&[3]]);
        // Out of the margin: best 5.0, floor -1.0, candidate at -1.5.
        let mut logits = logits_of(11, 0.0);
        logits[1] = 5.0;
        logits[3] = -1.5;
        bias.process(&mut logits, BLANK);
        assert_eq!(logits[3], -1.5);

        // Within the margin: candidate at -0.5 gets exactly one boost.
        let mut logits = logits_of(11, 0.0);
        logits[1] = 5.0;
        logits[3] = -0.5;
        bias.process(&mut logits, BLANK);
        assert_eq!(logits[3], -0.5 + 3.0);
    }

    #[test]
    fn near_tie_flips_argmax_only_with_boost() {
        let mut bias = bias_from(&[&[3]]);
        let mut logits = logits_of(11, 0.0);
        logits[1] = 5.0;
        logits[3] = 4.2;
        bias.process(&mut logits, BLANK);
        assert_eq!(logits[3], 7.2);
        let argmax = logits
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(argmax, 3, "boosted term token must win the near-tie");

        // Control: a trie over an unrelated token leaves the near-tie alone.
        let mut other = bias_from(&[&[7]]);
        let mut logits = logits_of(11, 0.0);
        logits[1] = 5.0;
        logits[3] = 4.2;
        other.process(&mut logits, BLANK);
        assert_eq!(logits[3], 4.2);
        assert_eq!(logits[1], 5.0);
    }

    #[test]
    fn blank_untouched_bit_for_bit() {
        // A hostile path proposing the blank id itself must be ignored, and
        // the blank slot must not change by a single bit even while other
        // boosts fire.
        let mut bias = bias_from(&[&[9], &[BLANK]]);
        let mut logits = logits_of(11, 0.0);
        logits[BLANK] = 9.25;
        logits[9] = 8.0;
        let blank_bits = logits[BLANK].to_bits();
        bias.process(&mut logits, BLANK);
        assert_eq!(logits[BLANK].to_bits(), blank_bits);
        assert_eq!(logits[9], 11.0, "non-blank candidate still boosts");
    }

    #[test]
    fn untouched_logits_are_bit_identical() {
        let mut bias = bias_from(&[&[3]]);
        let mut logits: Vec<f32> = (0..11).map(|i| i as f32 * 0.1).collect();
        let before: Vec<u32> = logits.iter().map(|v| v.to_bits()).collect();
        bias.process(&mut logits, BLANK);
        for (i, (bits, value)) in before.iter().zip(&logits).enumerate() {
            if i == 3 {
                continue;
            }
            assert_eq!(value.to_bits(), *bits, "logit {i} must be untouched");
        }
        assert_eq!(logits[3], 0.3 + 3.0);
    }

    #[test]
    fn max_not_sum_when_two_states_propose_same_token() {
        // Terms [1,2] and [2]: after emitting 1, the active state proposes
        // token 2 at depth 2 (4.5) and the root proposes it at depth 1
        // (3.0). The applied boost is the max, never the sum.
        let mut bias = bias_from(&[&[1, 2], &[2]]);
        bias.on_emit(1);
        let mut logits = logits_of(11, 0.0);
        bias.process(&mut logits, BLANK);
        assert_eq!(logits[2], 3.0 * 1.5);
    }

    #[test]
    fn depth_factor_applies_from_depth_two_without_compounding() {
        let mut bias = bias_from(&[&[1, 2, 3]]);

        // Depth 1 (root child): plain boost.
        let mut logits = logits_of(11, 0.0);
        bias.process(&mut logits, BLANK);
        assert_eq!(logits[1], 3.0);
        assert_eq!(logits[2], 0.0);

        // Depth 2: boost * depth_factor.
        bias.on_emit(1);
        let mut logits = logits_of(11, 0.0);
        bias.process(&mut logits, BLANK);
        assert_eq!(logits[2], 4.5);

        // Depth 3: still boost * depth_factor, not * depth_factor^2.
        bias.on_emit(2);
        let mut logits = logits_of(11, 0.0);
        bias.process(&mut logits, BLANK);
        assert_eq!(logits[3], 4.5);
    }

    #[test]
    fn out_of_range_token_is_ignored() {
        let mut bias = bias_from(&[&[42]]);
        let mut logits = logits_of(11, 0.0);
        bias.process(&mut logits, BLANK);
        assert!(logits.iter().all(|&v| v == 0.0));
        // Emitting the out-of-range token still advances/dies safely.
        bias.on_emit(42);
        assert!(bias.active.is_empty());
    }

    #[test]
    fn excluded_ids_are_never_boosted() {
        // Defense in depth: even a path that slipped past the build-time
        // drop cannot boost an excluded id at process time.
        let mut bias = TermBias::from_paths(
            BiasSettings::default(),
            vec![vec![4], vec![8]],
            vec![8, 999],
        )
        .unwrap();
        bias.excluded.insert(4);
        let mut logits = logits_of(11, 0.0);
        bias.process(&mut logits, BLANK);
        assert!(logits.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn empty_logits_are_a_no_op() {
        let mut bias = bias_from(&[&[3]]);
        let mut logits: Vec<f32> = Vec::new();
        bias.process(&mut logits, 0);
    }

    // ----- on_emit: advance, overlap, death, caps -----

    #[test]
    fn nonmatching_emit_empties_active_with_no_penalty() {
        let mut bias = bias_from(&[&[1, 2]]);
        bias.on_emit(1);
        assert_eq!(bias.active.len(), 1);

        bias.on_emit(7); // continues nothing
        assert!(bias.active.is_empty());

        // Afterwards only the term-start token is a candidate again, and no
        // logit is ever pushed down (no retraction, the boost simply stops).
        let mut logits = logits_of(11, 0.0);
        bias.process(&mut logits, BLANK);
        assert_eq!(logits[1], 3.0);
        assert_eq!(logits[2], 0.0);
        assert!(logits.iter().all(|&v| v >= 0.0));
    }

    #[test]
    fn terminal_with_children_keeps_matching_and_counts_hits() {
        // "5" is a term and a prefix of the term "5 6".
        let mut bias = bias_from(&[&[5], &[5, 6]]);
        bias.on_emit(5);
        assert_eq!(bias.hits(), 1, "short term completed");
        assert_eq!(bias.active.len(), 1, "prefix keeps matching");
        bias.on_emit(6);
        assert_eq!(bias.hits(), 2, "long term completed");
        assert!(bias.active.is_empty(), "terminal leaf leaves no state");
    }

    #[test]
    fn overlapping_terms_can_both_hit_on_one_emit() {
        // Terms [1,2] and [2]: emitting 1 then 2 completes both at once.
        let mut bias = bias_from(&[&[1, 2], &[2]]);
        bias.on_emit(1);
        bias.on_emit(2);
        assert_eq!(bias.hits(), 2);
    }

    #[test]
    fn active_state_cap_holds() {
        // A single self-repeating path grows one new partial match per
        // emit; without the cap the active set would reach 17.
        let path: Vec<usize> = vec![9; MAX_ACTIVE_STATES + 2];
        let mut bias = TermBias::from_paths(BiasSettings::default(), vec![path], vec![]).unwrap();
        for step in 0..MAX_ACTIVE_STATES + 4 {
            bias.on_emit(9);
            assert!(
                bias.active.len() <= MAX_ACTIVE_STATES,
                "cap exceeded at step {step}: {}",
                bias.active.len()
            );
        }
        assert_eq!(bias.active.len(), MAX_ACTIVE_STATES);
    }

    #[test]
    fn on_reset_clears_state_and_hits() {
        let mut bias = bias_from(&[&[5], &[1, 2]]);
        bias.on_emit(5);
        bias.on_emit(1);
        assert_eq!(bias.hits(), 1);
        assert!(!bias.active.is_empty());
        bias.on_reset();
        assert!(bias.active.is_empty());
        assert_eq!(bias.hits(), 0);
        // A fresh utterance starts matching from the root again.
        bias.on_emit(5);
        assert_eq!(bias.hits(), 1);
    }

    // ----- build(): terms, casing variants, caps -----

    #[test]
    fn build_encodes_casing_variants_through_the_tokenizer() {
        let sp_model = tu::tiny_model();
        let cfg = BiasSettings {
            terms: vec!["Rekody".into()],
            ..BiasSettings::default()
        };
        let bias = TermBias::build(&sp_model, cfg).expect("lowercase variant must encode");
        // The fixture vocabulary is lowercase-only, so exactly one path
        // (the lowercase form, one piece) lands in the trie.
        assert_eq!(bias.nodes.len(), 2);
        assert_eq!(bias.nodes[0].children, vec![(tu::ID_REKODY, 1)]);
        assert!(bias.nodes[1].terminal);
        assert_eq!(bias.nodes[1].depth, 1);
    }

    #[test]
    fn build_caps_terms_at_max_terms() {
        let sp_model = tu::tiny_model();
        let cfg = BiasSettings {
            terms: vec!["ab".into(), "cd".into(), "x".into()],
            max_terms: 2,
            ..BiasSettings::default()
        };
        let bias = TermBias::build(&sp_model, cfg).unwrap();
        let mut roots: Vec<usize> = bias.nodes[0].children.iter().map(|&(t, _)| t).collect();
        roots.sort_unstable();
        assert_eq!(roots, vec![tu::ID_AB, tu::ID_CD]);
        assert!(
            !bias.nodes[0].children.iter().any(|&(t, _)| t == tu::ID_X),
            "terms past the cap must not enter the trie"
        );
    }

    #[test]
    fn build_returns_none_without_usable_terms() {
        let sp_model = tu::tiny_model();
        assert!(TermBias::build(&sp_model, BiasSettings::default()).is_none());
        let cfg = BiasSettings {
            terms: vec!["qqq".into()],
            ..BiasSettings::default()
        };
        assert!(
            TermBias::build(&sp_model, cfg).is_none(),
            "unencodable-only dictionaries install nothing"
        );
    }

    #[test]
    fn build_wires_a_multiword_phrase_end_to_end() {
        let sp_model = tu::tiny_model();
        let cfg = BiasSettings {
            terms: vec!["core ml".into()],
            ..BiasSettings::default()
        };
        let mut bias = TermBias::build(&sp_model, cfg).unwrap();

        // Start of the phrase: depth 1 boost on the first token.
        let mut logits = logits_of(20, 0.0);
        bias.process(&mut logits, 19);
        assert_eq!(logits[tu::ID_CORE], 3.0);

        // Continuation across the word boundary: depth 2 boost, then a hit.
        bias.on_emit(tu::ID_CORE);
        let mut logits = logits_of(20, 0.0);
        bias.process(&mut logits, 19);
        assert_eq!(logits[tu::ID_ML], 4.5);
        bias.on_emit(tu::ID_ML);
        assert_eq!(bias.hits(), 1);
    }
}
