# Engine-level dictionary term biasing for Rekody Streaming (Nemotron)

Implementation spec for GitHub issue #91 (part of the Dictionary v2 arc, issues #88 to #91).

Status: spec, awaiting review. No code in this document has been written yet.
Scope: the Nemotron streaming engine only (`stt_engine = "nemotron"`). Whisper and cloud engines are out of scope.

The short version: today, when the streaming engine mishears a personal term badly, we try to repair the text afterwards and often cannot. This spec moves the fix into the decoder itself: while the model is choosing each token, we nudge the scores of tokens that continue one of the user's dictionary terms. The model weights are never touched, everything happens in our Rust decode loop, and the whole feature sits behind a config flag that defaults to off.

---

## 1. Background

### 1.1 The engine as it exists today

Rekody's streaming dictation path is three layers deep. Read them in this order if you are new to the code:

1. `crates/rekody-core/src/streaming.rs`
   The bridge between the async pipeline and the engine. `spawn(model_dir)` starts a dedicated OS thread that owns the engine, receives raw 16 kHz mono samples via `StreamMsg::Samples`, and emits `StreamEvent::Partial` / `StreamEvent::Final` back to the pipeline. The pipeline side lives in `crates/rekody-core/src/lib.rs` (`run_streaming`, around line 736; the backend is selected at lines 512 to 531 where `stt_engine = "nemotron"` maps to the model directory `~/.local/share/rekody/models/nemotron-en-int8`).

2. `crates/rekody-stt/src/nemotron.rs`
   `NemotronStreamingEngine`, a thin wrapper (about 90 lines). `feed()` buffers samples and calls the decoder once per full 560 ms chunk (`CHUNK_SAMPLES = 8960`). `finish()` pad-flushes the tail, returns the utterance transcript, and calls `model.reset()` so no state bleeds into the next dictation.

3. `parakeet-rs 0.3.6` (crates.io dependency, feature `nemotron`)
   The actual model runtime. Source on this machine: `~/.cargo/registry/src/index.crates.io-*/parakeet-rs-0.3.6/src/`. Two files matter:
   - `nemotron.rs`: mel extraction, chunk assembly, and the decode loop.
   - `model_nemotron.rs`: the two ONNX Runtime sessions, `encoder.onnx` and `decoder_joint.onnx`.

The model itself is NVIDIA's Nemotron streaming transducer (cache-aware FastConformer, 0.6B), shipped by Rekody as an int8 ONNX conversion plus its SentencePiece `tokenizer.model`, all in the model directory. English-only variant today (vocab 1024 pieces); the multilingual variant is auto-detected by parakeet-rs but not shipped yet.

### 1.2 How a chunk becomes text (the decode loop)

This is the part issue #91 is about, so here it is in detail. A transducer emits tokens frame by frame:

1. Each 560 ms chunk becomes 56 mel frames; the encoder subsamples 8x, so one chunk yields about 7 encoder frames plus updated streaming caches.
2. For each encoder frame, `Nemotron::decode_chunk` (parakeet-rs `src/nemotron.rs`, lines 723 to 770) runs an inner loop of up to `max_symbols_per_step = 10` steps.
3. Each step calls `NemotronModel::run_decoder` (`src/model_nemotron.rs`, lines 232 to 287). That runs the `decoder_joint.onnx` session (prediction network LSTM plus joint network) and returns three things: a plain `Array1<f32>` of logits over the vocabulary plus one extra slot, and the two new LSTM states.
4. Back in Rust, the loop takes a pure greedy argmax over those logits (lines 749 to 756). If the winner is the blank token (`blank_id`, the last index), the loop breaks and moves to the next frame. Otherwise the token is emitted: pushed to the output, fed back as `last_token`, LSTM states committed.
5. Emitted token ids are turned into text by `SentencePieceVocab::decode_single`, which just looks up the piece string and maps the SentencePiece word-start marker to a space.

Two facts fall out of reading that code, and they are the foundation of this whole spec:

- The logits arrive in Rust as ordinary `f32` values BEFORE any decision is made. The int8 quantization is internal to the ONNX graphs; the joint output tensor is f32. Anything we add between "logits extracted" and "argmax taken" changes decoding behavior without touching a single weight.
- The decision is a bare argmax. There is no beam, no language model, no hook. One point per step where the future of the transcript is decided, currently in about eight lines of Rust.

### 1.3 What Dictionary v1 does, and why it cannot fix far mishears

The personal dictionary (`~/.config/rekody/dictionary.toml`, managed by `rekody dictionary add/remove`, code in `crates/rekody-core/src/dictionary.rs`) is applied AFTER the engine has produced text:

1. `dictionary::correct_text` runs a deterministic pass over the final transcript: exact case-insensitive matches are recased, and fuzzy near-misses are rewritten into the canonical term.
2. `dictionary::inject_vocabulary_prompt` lists the terms in the LLM cleanup prompt so cleanup preserves them.

`correct_text` is deliberately conservative. A fuzzy rewrite only fires when ALL of these guards pass (see `fuzzy_word_matches` and `match_at`):

- first letter matches,
- length within plus or minus 2 characters,
- weighted edit distance within tolerance (one edit for short terms, two for long ones, sound-alike substitutions counted as half),
- the transcript word is not one of the top 10,000 everyday English words,
- exactly one dictionary term is in range (ambiguity means no correction).

Those guards are correct and should not be loosened. Loosening them corrupts normal prose: the unit tests in `dictionary.rs` exist precisely because "change" is two edits from "Chamgei" and must never be rewritten.

The consequence is a class of errors nothing downstream can repair, the "far mishears":

- "Chamgei" heard as "jam gay" or "chum gay": wrong first letters, wrong word boundaries, two words instead of one. Every guard fails.
- "Rekody" heard as "recording": length guard fails, and "recording" is an everyday word.
- "Kipkemboi" heard as "keep them boy": three words, nothing to anchor on.

By the time text exists, the information needed to fix these is gone. But at the moment the decoder took its argmax, the correct token was often sitting a few logits below the winner. The model heard something Rekody-shaped; greedy decoding just committed to the more common word. Post-hoc correction works on the output of that decision. Engine-level biasing works on the decision itself. That is the entire point of this feature.

### 1.4 Glossary (for readers new to transducers)

- blank: the special "emit nothing, advance time" token. In this model it is the last logit index (`blank_id = vocab_size`).
- joint network: the small network that combines one encoder frame with the prediction network state to produce per-token logits.
- shallow fusion: adding an external score onto the model's logits/log-probs at decode time, without training. Our biasing is shallow fusion with a term list instead of a language model.
- SentencePiece piece: a subword unit. Word starts carry a marker character (U+2581) that decodes to a space. "Rekody" might be pieces like `▁Re`, `k`, `ody`.

---

## 2. Options analysis

Three families of approaches exist in the literature and in shipping systems. All three were researched against our exact constraint set: shipped int8 ONNX, greedy decode in Rust via parakeet-rs, push-to-talk latency budget (about 81 ms compute per 560 ms chunk today), per-user dynamic term list.

### Option A: decode-time trie boosting on the greedy path (shallow fusion)

Build a prefix trie over the SentencePiece token sequences of the dictionary terms. At every inner decode step, before the argmax, add a boost to the logits of tokens that start or continue a term, but only when that token is already acoustically competitive (within a margin of the step's best logit). Advance the trie state on every emitted token.

- No training. No model file changes. Works on the shipped int8 artifact as-is.
- This is exactly what NVIDIA itself ships for this problem class: NeMo's GPU-PB / TurboBias ("GPU-accelerated Phrase-Boosting", ASRU 2025, arXiv 2508.07014) applies a token-level Aho-Corasick boosting tree in shallow fusion mode for CTC, RNNT/TDT and AED models, explicitly supporting GREEDY decoding, with no training. Their greedy variant increases boost with trie depth so a single-hypothesis decode can carry a phrase through to completion. NeMo's older CTC-WS context biasing (Interspeech 2024, arXiv 2406.07096) is likewise decode-time and training-free, but needs a hybrid CTC head we do not have.
- Known weakness: greedy commits. If a boost flips a step wrongly there is no retraction, unlike beam search where a dead partial match can have its boost cancelled. Managed with the margin gate, moderate boost, and evaluation (section 6).

### Option B: modified beam search plus context graph

What sherpa-onnx ships as "hotwords": keep k hypotheses, boost per token through an Aho-Corasick context graph with failure arcs, and cancel the accumulated boost when a partial match dies. Strictly better biasing quality in principle.

- Also decode-time and training-free.
- Cost: every decode step multiplies into k `decoder_joint.onnx` calls plus per-hypothesis LSTM state copies. Our decoder call is the per-step unit of work; k = 4 roughly quadruples decoder-side compute on the hot path we currently brag about (final flush about 55 ms).
- Much larger fork surface in parakeet-rs: hypothesis management, state cloning, merge rules, instead of a three-line hook.
- Field report worth taking seriously: sherpa-onnx's own tracker has modified beam search on NeMo TDT models hallucinating or returning empty text about 20% of the time while greedy works (k2-fsa/sherpa-onnx issue #3267), and hotwords for their Nemotron streaming integration are still an open feature request (issue #3572, which also documents the motivating gap: proper-noun substitutions). Beam on this model family is not free quality.

### Option C: fine-tuned context biasing

Train the capability in: contextual adapters (Amazon, ICASSP 2022 and the gated follow-up), CLAS-style context encoders, or TCPGen pointer-generators; or simply fine-tune the model on term-rich personal data. Highest ceiling, especially for terms whose sounds the base model barely knows.

- Requires the fp checkpoint, GPU training, a new ONNX export, and re-running our int8 quantization and evaluation. Architecture-level variants (adapters, TCPGen) also change the ONNX graph contract (a biasing-list input), which cascades into parakeet-rs changes.
- Cannot ship quickly, and per the standing project rule this path is APPROVAL-GATED: no fine-tuning work starts until Tony approves. Scoped in Appendix A.

### Comparison

| | A: greedy trie boosting | B: modified beam + context graph | C: fine-tuned biasing |
|---|---|---|---|
| Training needed | none | none | yes (APPROVAL-GATED) |
| Works on shipped int8 ONNX unchanged | yes | yes | no: retrain, re-export, re-quantize |
| Where it lives | Rust decode loop (small parakeet-rs fork hook + rekody-stt module) | large parakeet-rs fork (hypothesis engine) | training pipeline + model release |
| Added latency | negligible (trie walk per step) | roughly k times decoder compute | none at decode |
| Dynamic per-user terms | yes, rebuild trie in microseconds | yes | only with adapter-style architectures |
| Quality ceiling | good when the term is acoustically competitive | higher (alternatives stay alive, boosts retractable) | highest, covers hard OOV sounds |
| Main risk | over-boost hallucination (guarded, measurable) | instability reports on NeMo-family models; latency | training regressions; large scope |
| Precedent | NVIDIA TurboBias/GPU-PB greedy mode; NeMo CTC-WS | sherpa-onnx hotwords; Google biasing FSTs (Interspeech 2019) | Amazon contextual adapters; CLAS; TCPGen |

### Explicit answer to the feasibility question

Can we bias the shipped quantized int8 ONNX purely at decode time, weights untouched? Yes.

Evidence, all verifiable in the code on this machine:

- `NemotronModel::run_decoder` (parakeet-rs 0.3.6, `src/model_nemotron.rs` lines 232 to 287) extracts the joint output as an f32 tensor into an `ndarray::Array1<f32>` and returns it to safe Rust. Quantization never reaches the interface.
- `Nemotron::decode_chunk` (`src/nemotron.rs` lines 723 to 770) takes the argmax in plain Rust. Inserting a logit adjustment between those two points is a pure Rust change with zero model-file impact.
- Precedent that this exact mechanism works on this exact model family without training: TurboBias/GPU-PB (greedy and beam, CTC/RNNT/TDT), and sherpa-onnx hotwords on streaming transducers.

The only obstacle is API surface: `decode_chunk` is private and takes no hook, so we patch parakeet-rs (details in step 1 below). The license is MIT OR Apache-2.0, so a pinned fork is clean, and the patch is small enough to offer upstream.

---

## 3. Recommendation

Build Option A: trie-based shallow-fusion boosting inside the greedy decode loop, delivered through a minimal, pinned fork of parakeet-rs that adds a logits-processor hook, with all biasing logic living in `rekody-stt`. Keep Dictionary v1's post-hoc pass exactly as it is; the two are complementary (biasing fixes the decode, `correct_text` still recases and cleans near-misses).

Reasoning, in order of weight:

1. It is the only option that ships on the current artifact with no training, no new model files, and no meaningful latency cost.
2. NVIDIA validated the greedy variant of this exact idea on this model family (TurboBias). We are not inventing a decoding scheme, we are porting a known-good one to Rust at dictation scale (tens of terms, not 20k phrases).
3. The risk profile is measurable and gateable: over-boosting is the one failure mode, and section 6 defines the evaluation that must pass before the flag defaults on.
4. Option B remains available as an escalation with the same trie and tokenizer work reused, if evaluation shows greedy boosting cannot hit the recall target. Option C remains the ceiling, gated on approval.

---

## 4. Design overview

Data flow, end to end:

```
~/.config/rekody/dictionary.toml            (user terms, source of truth)
        |
        v
rekody-core lib.rs run_streaming()          reads Dictionary + RekodyConfig.term_biasing
        |                                    builds BiasSettings { terms, boost, margin, ... }
        v
rekody-core streaming.rs spawn(model_dir, bias)
        |                                    engine thread owns everything below
        v
rekody-stt nemotron.rs NemotronStreamingEngine::new_with_bias()
        |    parses tokenizer.model (pieces + scores)
        |    encodes each term with a SentencePiece Viterbi encoder
        |    builds the token trie
        |    installs the processor:  model.set_logits_processor(...)
        v
parakeet-rs (fork) Nemotron::decode_chunk()
        per inner step:
          logits = run_decoder(...)          f32, length vocab_size + 1
          processor.process(&mut logits)     <- boost trie continuations, margin-gated
          argmax, blank check                unchanged
          on emit: processor.on_emit(tok)    <- advance trie state
        on Nemotron::reset():
          processor.on_reset()               <- clear state between utterances
```

Everything above the fork line is new Rekody code. The fork itself is three call sites and one trait.

---

## 5. Step-by-step implementation plan

Work top to bottom; each step compiles and is testable on its own. Estimated sizes are for orientation, not commitments.

### Step 1: fork parakeet-rs and add the hook (about 40 lines of diff)

1. Fork `parakeet-rs` at the 0.3.6 release into a Rekody-controlled repo, branch `rekody/logits-processor`. License is MIT OR Apache-2.0; keep upstream attribution intact.
2. In the fork's `src/nemotron.rs`, add:

```rust
/// Hook into the greedy decode loop. Called with the model lock held;
/// implementations must be fast and must never touch the blank logit.
pub trait LogitsProcessor: Send {
    /// Adjust `logits` in place before the argmax. `blank_id` is the last index.
    fn process(&mut self, logits: &mut [f32], blank_id: usize);
    /// A non-blank token was emitted and committed.
    fn on_emit(&mut self, token: usize);
    /// Utterance state was reset (Nemotron::reset).
    fn on_reset(&mut self);
}
```

3. Add a field `logits_processor: Option<Box<dyn LogitsProcessor>>` to `Nemotron` (per-instance, not on `NemotronHandle`: decoder state is per-stream) plus `pub fn set_logits_processor(&mut self, p: Option<Box<dyn LogitsProcessor>>)`. Initialize to `None` in `from_shared`.
4. Wire three call sites in `decode_chunk` and `reset`:
   - after `run_decoder` returns and before the argmax scan: `if let Some(p) = self.logits_processor.as_mut() { if let Some(s) = logits.as_slice_mut() { p.process(s, self.blank_id); } }` (the `Array1` is contiguous, so `as_slice_mut` succeeds),
   - after `tokens.push(max_idx)`: `p.on_emit(max_idx)`,
   - inside `reset()`: `p.on_reset()`.
5. In the workspace root `Cargo.toml`, add a `[patch.crates-io]` entry pointing `parakeet-rs` at the fork, pinned by `rev` (never a floating branch; this repo runs cargo-deny and pins its supply chain). `crates/rekody-stt/Cargo.toml` keeps its normal `parakeet-rs = { version = "0.3.6", optional = true }` line.
6. Offer the patch upstream as a PR. If upstream merges, drop the patch entry at the next version bump. (Demand exists: sherpa-onnx issue #3572 is the same request against their runtime.)

Why fork rather than reimplement on the public API: `NemotronModel::run_encoder` and `run_decoder` are public, but the mel pipeline (`audio::create_mel_filterbank`, `stft`, `apply_preemphasis`) and the whole chunk/cache state machine are private. Rebuilding those in Rekody would duplicate roughly 400 lines of numerically sensitive code to avoid a 40 line patch, and would drift from upstream fixes.

### Step 2: SentencePiece encoding of terms (`crates/rekody-stt/src/biasing/sp.rs`, new)

To bias token paths we need each term AS THE MODEL WOULD EMIT IT: a sequence of vocabulary ids. parakeet-rs only decodes (its `SentencePieceVocab` parses piece strings from `tokenizer.model` but skips the scores and has no encode). We add a small self-contained encoder; it lives in rekody-stt so the fork stays minimal.

1. Protobuf parse of `tokenizer.model` (same file already sitting in the model dir): walk the `ModelProto`, field 1 is the repeated `SentencePiece` message; inside it, field 1 is the piece string, field 2 is the float score (wire type 5, 4 bytes little-endian), field 3 the piece type varint. Keep pieces of type NORMAL (and USER_DEFINED); record `(piece, id, score)`. The parser in parakeet-rs `src/nemotron.rs` lines 114 to 228 is the template; ours additionally captures field 2 instead of skipping it.
2. Unigram Viterbi encode: normalize the input (trim, collapse internal whitespace to the word-start marker U+2581, prepend the marker since dictated terms virtually always follow a space), then standard dynamic programming over character positions choosing the piece segmentation with maximal summed score (ties: fewer pieces). A HashMap from piece string to (id, score) plus the max piece length bound makes this a few dozen lines.
3. Casing variants: encode up to three forms per term and deduplicate: as written ("Rekody"), lowercase ("rekody"), and first-letter-capitalized if distinct. The acoustic model emits its own casing; we bias every plausible surface form and let the existing `correct_text` recase the final text.
4. If a term contains characters no piece covers, skip it with a `tracing::warn!` (it still gets Dictionary v1 treatment). Return type: `Vec<Vec<usize>>` per term.
5. Correctness check built into the API: encoding then decoding through parakeet's `SentencePieceVocab::decode` must reproduce the term text (with its leading space). This becomes a unit test against the real shipped tokenizer.

Multi-word terms ("Core ML") need no special handling: the internal space becomes a word-start marker and the phrase is simply a longer token path in the trie.

### Step 3: the trie and the processor (`crates/rekody-stt/src/biasing/mod.rs`, new)

Data structures:

```rust
pub struct BiasSettings {
    pub terms: Vec<String>,
    pub boost: f32,          // default 3.0 logits per matched token
    pub margin: f32,         // default 6.0: only boost tokens within this of the step's best logit
    pub depth_factor: f32,   // default 1.5: multiplier for tokens at depth >= 2
    pub max_terms: usize,    // default 200, hard cap
}

struct Node { children: Vec<(usize /*token id*/, u32 /*node index*/)>, terminal: bool, depth: u16 }

pub struct TermBias {
    nodes: Vec<Node>,        // node 0 is the root
    active: Vec<u32>,        // current partial-match states (root implicit, always active)
    cfg: BiasSettings,
    hits: u64,               // completed term matches this utterance, for logging
}
```

`impl LogitsProcessor for TermBias`:

- `process(logits, blank_id)`:
  1. Find `best = max(logits)` in one pass.
  2. Candidate set: children of the root (term-start tokens) plus children of every active node (continuations). For each candidate token `t` with node depth `d`:
     - never touch `blank_id`, never touch any id outside `0..vocab_size`, never touch language-tag ids (matters when the multilingual variant ships),
     - margin gate: only if `logits[t] >= best - cfg.margin`,
     - boost: `logits[t] += cfg.boost * if d >= 2 { cfg.depth_factor } else { 1.0 }`.
       If several states propose the same token, apply the single largest boost, never a sum. Depth weighting follows the TurboBias finding: on a greedy single-hypothesis decode, deeper transitions need more push so a started phrase can be carried to completion instead of dying halfway.
- `on_emit(token)`: advance. New active set = every `child(state, token)` over the old active set, plus `child(root, token)` if it exists. A node with `terminal = true` counts a hit (`hits += 1`) and contributes its children only if it has any (a term that is a prefix of another term keeps matching). Deduplicate, cap `active` at 16 states. If the emitted token continues nothing, the set naturally empties: no penalties, no retraction needed, the boost simply stops.
- `on_reset()`: clear `active`, log and zero `hits`.

Notes for the implementer:

- Blank never calls `on_emit` (the upstream loop breaks before pushing), so partial matches survive silence and chunk boundaries within an utterance. That is correct: the state only exists because its tokens were genuinely emitted.
- The per-step cost is a scan over at most (16 active states + root) children with one float compare each. For a 200-term dictionary this is hundreds of operations against an ONNX session call that costs milliseconds: not measurable.
- `max_symbols_per_step = 10` upstream stays as the hard cap on runaway emission per frame. Do not raise it.

### Step 4: engine wiring (`crates/rekody-stt/src/nemotron.rs`)

1. Add `NemotronStreamingEngine::new_with_bias(model_dir: &str, bias: Option<BiasSettings>) -> Result<Self>`. Keep `new` delegating with `None` so existing callers and the `nemotron_stream` example compile unchanged.
2. When `bias` is `Some` and non-empty: parse `tokenizer.model` from the same `model_dir`, encode terms (step 2), build `TermBias` (step 3), and install it with `model.set_logits_processor(Some(Box::new(...)))`. When `None`, empty, or over `max_terms` after truncation warning: install nothing. Disabled means the fork hook is never even consulted beyond an `Option` check.
3. Add `pub fn set_terms(&mut self, terms: Vec<String>)`: rebuilds the trie and swaps the processor. Only call between utterances; the engine wrapper enforces this by stashing the request if `buf` or `transcript` is non-empty and applying it inside `finish()` after `model.reset()`.

### Step 5: pipeline plumbing (`crates/rekody-core`)

1. Config (`lib.rs`, `RekodyConfig`): add a nested optional table, serde-defaulted so every existing `config.toml` keeps parsing:

```toml
[term_biasing]
enabled = false        # feature flag; stays false until the eval gate passes
boost = 3.0
margin = 6.0
depth_factor = 1.5
max_terms = 200
```

   `enabled = true` has an effect only when `stt_engine = "nemotron"`; document that on the field.
2. `streaming.rs`: `spawn(model_dir: PathBuf, bias: Option<BiasSettings>)`, passed into `NemotronStreamingEngine::new_with_bias` on the engine thread. Add a message variant `StreamMsg::ReloadTerms(Vec<String>)` handled by calling `engine.set_terms(...)` (which defers mid-utterance as per step 4.3).
3. `lib.rs` `run_streaming`: build the initial `BiasSettings` from `RekodyConfig.term_biasing` plus `dictionary::Dictionary::load_or_empty().terms()`. Track the dictionary file's mtime; at each utterance start (the same place the hotkey-press branch begins forwarding `StreamMsg::Samples`, around lines 870 to 905), re-stat and send `ReloadTerms` when it changed. This preserves the Dictionary v1 behavior that `rekody dictionary add` takes effect without a daemon restart.
4. Observability: on `StreamEvent::Final`, log `bias_hits` (carried back from the engine thread inside the existing event or a `tracing` line on the engine thread) so dogfooding sessions can see the feature fire. No history schema changes in v1.
5. `config_tui.rs`: expose an on/off row for term biasing only in the rollout phase where the flag becomes user-facing (section 7); not needed for the first landing.

Nothing changes in the post-STT pipeline: `StreamEvent::Final` text continues through `process_transcript`, which still applies `correct_text`, the LLM vocabulary prompt, numbers, and snippets exactly as today (`lib.rs` lines 1030 to 1170).

### Tunables summary

| Knob | Default | Meaning | Raising it | Lowering it |
|---|---|---|---|---|
| `boost` | 3.0 | logits added per matched token at depth 1 | more term wins, more hallucination risk | feature does less |
| `margin` | 6.0 | candidate must be within this of the step's best logit | boosts fire on less-competitive tokens | only near-ties flip |
| `depth_factor` | 1.5 | boost multiplier at depth >= 2 | phrases complete more reliably | started phrases die midway |
| `max_terms` | 200 | dictionary size cap for v1 | more terms, more root children | tighter |

Defaults are starting points chosen from the shallow-fusion literature (sherpa-onnx defaults to 1.5 per token in log-prob space with beam retraction available; greedy without retraction wants a margin gate and a moderate boost). They MUST be re-tuned on our int8 artifact using the harness in section 6; quantization shifts absolute logit scales, so numbers from papers or other runtimes do not transfer directly.

### Guards against over-boosting (complete list)

1. Blank is never modified. Silence stays silence.
2. Margin gate: a token acoustically out of the running is never boosted, so terms cannot materialize out of nothing.
3. Max-not-sum when multiple trie states propose the same token.
4. Depth-gated boost only along paths whose earlier tokens were actually emitted.
5. Active-state cap (16) and term cap (200).
6. Upstream `max_symbols_per_step = 10` unchanged.
7. Language-tag and out-of-range ids excluded.
8. Feature flag off by default; disabled installs no processor at all.
9. The conservative post-hoc pass still runs afterwards and can only improve things (recase, repair near-misses). It never undoes a biased win because a correctly-emitted term is an exact match, which `correct_text` shields from fuzzy rewriting.

---

## 6. Test plan

### Unit tests (fast, hermetic, run in CI)

In `crates/rekody-stt/src/biasing/` under `#[cfg(test)]`, plus `crates/rekody-stt/tests/`:

1. Protobuf parser: piece count, scores present, known piece ids stable against a checked-in miniature SentencePiece model built for tests (do not commit the real 13k-piece file; a tiny synthetic .model with a dozen pieces exercises every wire type).
2. Viterbi encoder: known segmentations on the synthetic model; round-trip property (`decode(encode(term))` reproduces the term with leading space); whitespace collapse; casing variants deduplicate; unencodable input returns None.
3. Trie: build from token sequences; shared prefixes; phrase spanning a word boundary; terminal-with-children; advance and natural death on a non-matching emit; cap behavior; reset.
4. Processor semantics on synthetic logit vectors: margin gate blocks an out-of-range candidate; a near-tie flips only with the boost applied; blank untouched byte-for-byte; max-not-sum verified with two states proposing one token; depth_factor applied at depth 2; `on_reset` clears state.
5. Disabled path: `new_with_bias(dir, None)` installs no processor and produces bit-identical transcripts to `new` (guards the "zero cost when off" claim).
6. Real-tokenizer smoke test, skip-if-missing like `tests/short_audio_smoke.rs`: when `~/.local/share/rekody/models/nemotron-en-int8/tokenizer.model` exists, encode "Rekody", "Chamgei", "Core ML" and round-trip through parakeet's `SentencePieceVocab::decode`.

### Real-audio eval harness

New example `crates/rekody-stt/examples/nemotron_bias_eval.rs` (`required-features = ["nemotron"]`), the sibling of the existing `nemotron_stream.rs`:

- Input: model dir; a JSONL manifest in the exact NeMo-style shape `training_data.rs` already writes (`audio_filepath`, `text`, `duration`); a terms file or the live `dictionary.toml`; `--boost/--margin/--depth-factor` overrides. FLAC clips are converted through macOS's bundled `afconvert` into a temp dir, matching the `training_data.rs` convention; WAV plays directly via `hound` (already a dev-dependency).
- Method: load the model once, then run every clip twice through a fresh engine state, bias off and bias on, feeding variable-length sample runs the way `nemotron_stream.rs` does so the chunking matches production.
- Metrics, computed with the same normalize-then-word-Levenshtein semantics as `crates/rekody-review/src/wer.rs` (copy the two small functions into the example with a comment naming the source of truth):
  - overall WER, off vs on;
  - term recall: fraction of reference occurrences of dictionary terms that appear (normalized) in the hypothesis;
  - false-term rate: hypotheses containing a dictionary term whose reference does not, as a fraction of term-free clips;
  - per-chunk decode time mean/max, off vs on.
- Data:
  - the opt-in personal dataset at `~/.local/share/rekody/training-data/manifest.jsonl` (labels cleaned via rekody-review) for the "no regression on ordinary dictation" side;
  - a small recorded term set: 15 to 25 clips of dictionary terms spoken inside natural sentences ("open Rekody and check the Chamgei eval"), stored locally next to the personal dataset, never committed (privacy, delete-by-default policy);
  - adversarial term-free set: clips containing the confusable everyday words ("change the settings", "start recording"), plus 5 clips of silence and room noise with a full 200-term dictionary loaded.

### Gate to pass before the flag may default on

All four, measured by the harness on the int8 artifact with the shipped defaults:

1. Term recall on the term set improves by at least 30 points absolute over bias-off.
2. WER on the term-free personal set regresses by less than 0.3 absolute.
3. False-term rate under 1 percent of term-free clips, and zero term emissions on the silence/noise clips.
4. p95 per-chunk decode time within 5 percent of bias-off.

Publish nothing about the feature until it ships and the numbers are repo-verified (site claims policy in `CLAUDE.md`).

---

## 7. Rollout

Feature-flagged, default off, in four phases. Each phase is independently revertible.

- Phase 0, plumbing lands: fork pinned by rev in `[patch.crates-io]`, hook merged, biasing module and tests in tree, `term_biasing.enabled` defaults to false, processor never installed unless enabled. Behavior of every existing build path is bit-identical; CI proves it via the disabled-path test.
- Phase 1, internal dogfood: Tony enables `[term_biasing] enabled = true` locally with his real dictionary. Watch `bias_hits` logs and daily dictation quality. Tune `boost`/`margin` with the harness.
- Phase 2, eval gate: run the section 6 harness on the tuned defaults; record the numbers in the PR that changes anything. No gate, no advance.
- Phase 3, default on for the Nemotron engine: flip the serde default for `enabled` to true (still only effective when `stt_engine = "nemotron"`), add the config TUI row, document in the docs site dictionary page. The flag remains the kill switch; a user can always set `enabled = false`.

Release-surface note: this feature adds no artifacts (no new model files, no new binaries), so the four-place release checklist in `CLAUDE.md` is unchanged. The one new supply-chain obligation is the pinned fork: rev-pin it, keep cargo-deny green (license is unchanged MIT OR Apache-2.0), and track the upstream PR so the patch entry can eventually be deleted.

---

## 8. Risks

1. Over-boosting hallucinates terms. The signature failure of shallow fusion. Mitigations: the nine guards in section 5, the false-term and silence gates in section 6, default off until gated. Residual risk accepted and monitored via `bias_hits` logging during dogfood.
2. Partial-term garbles ("Rekodyne"). A boosted path can die after two tokens, leaving a hybrid. Greedy cannot retract. Mitigations: depth_factor makes completion more likely than abandonment; margin gate keeps entries limited to acoustically plausible starts; `correct_text` frequently repairs the leftover (it IS a near-miss at that point); measured by the WER-regression gate.
3. Segmentation mismatch. The model may emit a tokenization of the term that differs from our Viterbi-best (especially across casings). Mitigations: three casing variants per term; round-trip unit tests on the real tokenizer; if dogfood shows misses, add n-best segmentations per term (the trie absorbs them at trivial cost) before considering anything heavier.
4. Defaults tuned on the wrong scale. int8 logit scales differ from fp checkpoints and from other runtimes. Mitigation: tunables are config values, defaults set only from our own harness on the shipped artifact (the standing rule: verify against the actual artifact, not assumptions).
5. Fork drift. parakeet-rs moves and our patch pins us. Mitigations: additive 40-line diff, rev-pinned, upstream PR filed at landing time; revisit at every dependency bump. If upstream merges, the risk retires.
6. Beam envy. If greedy boosting cannot hit the recall gate, the temptation is to jump to Option B. Decision rule written down now: attempt n-best segmentations and tuning first; Option B only with a fresh latency budget and with sherpa-onnx issue #3267 (NeMo-family beam instability) re-checked against their current state.
7. Multilingual future. The shipped model is English-only; the multilingual variant has language-tag tokens and a 13k vocab. The design already excludes tag ids and reads vocab size from the tokenizer, so nothing structural breaks, but tuning must be redone per artifact before enabling there.
8. LLM cleanup undoing wins. Cleanup can rewrite a rare term it does not know. Existing mitigation stays: `inject_vocabulary_prompt` lists the terms in the cleanup prompt; the cleanup guard policy already constrains aggressive rewrites.
9. Privacy surface. Biasing is fully local (terms, tokenizer, trie, decode all on device), consistent with the runtime-scoped "0 network calls" claim. Terms already reach a cloud LLM only if the user chose a cloud cleanup provider; this feature does not widen that.

---

## Appendix A: the fine-tuning path (every step APPROVAL-GATED)

Nothing in the main spec depends on this appendix. It exists so the ceiling is scoped honestly. Every numbered item below is APPROVAL-GATED: it is not to be started, scheduled, or partially prototyped until Tony explicitly approves it.

Why it might eventually matter: decode-time boosting can only promote tokens the acoustic model already considers plausible. A term whose sounds the base model barely represents (hard OOV, heavy accent mismatch) may never be within the margin at any step. Training is the only fix for that class.

1. APPROVAL-GATED: decision and data readiness. Confirm the miss class actually observed in dogfood is acoustic (term never within margin, verifiable by instrumenting the harness to log the term-token logit gap) rather than a tuning or segmentation issue. Data already exists by design: the opt-in capture in `crates/rekody-core/src/training_data.rs` writes 16 kHz FLAC plus a NeMo-style JSONL manifest exactly for this, and `rekody-review` cleans the labels.
2. APPROVAL-GATED: base checkpoint provenance. Fine-tune only from the NVIDIA-published fp checkpoint under its license terms, using Rekody's own conversion pipeline. Never derive from third-party conversions (the repo already excludes an unlicensed-parent int8 conversion from serving for exactly this reason).
3. APPROVAL-GATED, option one, personalization fine-tune (no architecture change). Fine-tune (full or LoRA-style) on the user's cleaned clips plus synthetic term-rich data (term-bearing sentences via text injection into training transcripts, optionally TTS-generated audio). The ONNX export graph is unchanged, so the existing streaming cache-aware export and int8 quantization pipeline rerun as-is, and parakeet-rs needs nothing. This is also the technical basis of the managed fine-tune service, the product's one revenue line, so work here should be designed once for both purposes.
4. APPROVAL-GATED, option two, trainable context biasing (architecture change). Add a contextual-adapter / CLAS / TCPGen-style biasing module conditioned on a dynamic term list. Highest ceiling and per-request dynamism, but it changes the ONNX contract (a biasing-list input tensor), which cascades: new export code, fork changes to feed the tensor, requantization, and revalidation of everything. Treat as a research project, not a feature.
5. APPROVAL-GATED: re-export, re-quantize with the existing int8 pipeline, and re-gate. Any fine-tuned model must pass the section 6 harness AND the standing benchmark methodology before replacing the shipped artifact, then ship through the model vault with provenance recorded.

---

## References

- NeMo word boosting and context biasing overview (GPU-PB, Flashlight, CTC-WS): https://docs.nvidia.com/nemo-framework/user-guide/latest/nemotoolkit/asr/asr_customization/word_boosting.html
- TurboBias: Universal ASR Context-Biasing powered by GPU-accelerated Phrase-Boosting Tree (greedy-capable shallow fusion, no training): https://arxiv.org/abs/2508.07014
- Fast Context-Biasing for CTC and Transducer ASR models with CTC-based Word Spotter: https://arxiv.org/abs/2406.07096
- sherpa-onnx hotwords documentation (Aho-Corasick context graph, per-token boost, retraction under beam): https://k2-fsa.github.io/sherpa/onnx/hotwords/index.html
- sherpa-onnx issue #3572, hotwords request for the Nemotron streaming transducer: https://github.com/k2-fsa/sherpa-onnx/issues/3572
- sherpa-onnx issue #3267, modified beam search instability on NeMo TDT: https://github.com/k2-fsa/sherpa-onnx/issues/3267
- Contextual adapters for personalized speech recognition in neural transducers (training-based), Amazon: https://www.amazon.science/publications/gated-contextual-adapters-for-selective-contextual-biasing-in-neural-transducers
- Tree-constrained pointer generator (TCPGen, training-based): https://arxiv.org/abs/2305.18824
- Local code anchors: `crates/rekody-stt/src/nemotron.rs`, `crates/rekody-core/src/streaming.rs`, `crates/rekody-core/src/dictionary.rs`, `crates/rekody-core/src/training_data.rs`, `crates/rekody-review/src/wer.rs`, and `~/.cargo/registry/src/index.crates.io-*/parakeet-rs-0.3.6/src/{nemotron.rs,model_nemotron.rs}`.
