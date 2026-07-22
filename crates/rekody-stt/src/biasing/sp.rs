//! SentencePiece unigram parsing and Viterbi term encoding (spec step 2).
//!
//! Engine-level biasing needs each dictionary term as the model would emit
//! it: a sequence of vocabulary token ids. parakeet-rs only decodes (its
//! `SentencePieceVocab` reads piece strings from `tokenizer.model` and skips
//! the scores), so this module re-parses the same protobuf file keeping
//! `(piece, score, type)` per entry and adds a standard unigram Viterbi
//! encoder over those scores. Self-contained on purpose: the parakeet-rs
//! fork stays limited to the three decode-loop hook call sites.
//!
//! Wire layout walked here (SentencePiece `ModelProto`):
//! - top level: field 1 (wire type 2, repeated) is one `SentencePiece`
//!   message per vocabulary entry. The entry's ordinal position is the token
//!   id, so every field 1 message is recorded (control pieces included) to
//!   stay id-compatible with the piece table parakeet-rs builds from the
//!   same bytes.
//! - piece message: field 1 (wire type 2) is the piece string, field 2
//!   (wire type 5, 4 bytes little-endian) is the f32 score, field 3
//!   (wire type 0) is the piece type varint.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// SentencePiece word-start marker (U+2581). Decodes to a space. Every term
/// is encoded with a leading marker because dictated terms virtually always
/// follow a space in running text.
pub const WORD_START: char = '\u{2581}';

/// Piece type from field 3 of a `SentencePiece` message. Only
/// [`PieceKind::Normal`] and [`PieceKind::UserDefined`] pieces participate
/// in encoding; the rest stay in the table solely so token ids line up with
/// the decoder's view of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceKind {
    Normal,
    Unknown,
    Control,
    UserDefined,
    Unused,
    Byte,
}

impl PieceKind {
    fn from_proto(value: u64) -> Self {
        match value {
            1 => PieceKind::Normal,
            2 => PieceKind::Unknown,
            4 => PieceKind::UserDefined,
            5 => PieceKind::Unused,
            6 => PieceKind::Byte,
            // 3 is CONTROL in the proto; map unknown future values there too
            // so they can never enter a bias path.
            _ => PieceKind::Control,
        }
    }
}

/// One vocabulary entry. The token id is the entry's index in [`SpModel`].
#[derive(Debug, Clone)]
pub struct SpPiece {
    pub text: String,
    pub score: f32,
    pub kind: PieceKind,
}

/// Parsed SentencePiece unigram model: the full piece table (for decoding
/// and id parity with parakeet-rs) plus a piece-string lookup used by the
/// Viterbi encoder.
pub struct SpModel {
    /// Index = token id, in file order, control pieces included.
    pieces: Vec<SpPiece>,
    /// Piece string to `(id, score)` for NORMAL and USER_DEFINED pieces.
    encode_map: HashMap<String, (usize, f32)>,
    /// Longest encodable piece in chars; bounds the Viterbi window.
    max_piece_chars: usize,
}

impl SpModel {
    /// Parse a `tokenizer.model` file from disk.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let data = std::fs::read(path.as_ref())
            .with_context(|| format!("reading {}", path.as_ref().display()))?;
        Self::from_bytes(&data)
    }

    /// Parse `ModelProto` bytes. Field walk mirrors the parakeet-rs template
    /// (skip-by-wire-type, stop on unknown wire types) so both parsers keep
    /// identical piece counts, and therefore identical token ids.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut pieces: Vec<SpPiece> = Vec::new();
        let mut pos = 0usize;

        while pos < data.len() {
            let Some((header, used)) = read_varint(data, pos) else {
                bail!("invalid varint in tokenizer.model at byte {pos}");
            };
            pos += used;
            let field = header >> 3;
            let wire = header & 0x7;

            match (field, wire) {
                (1, 2) => {
                    let Some((len, used)) = read_varint(data, pos) else {
                        bail!("invalid piece length varint at byte {pos}");
                    };
                    pos += used;
                    let len = len as usize;
                    if len > data.len() - pos {
                        break;
                    }
                    // Mirror the template: an unparseable piece message is
                    // skipped on both sides, keeping id parity.
                    if let Some(piece) = parse_piece(&data[pos..pos + len]) {
                        pieces.push(piece);
                    }
                    pos += len;
                }
                (_, 0) => {
                    let Some((_, used)) = read_varint(data, pos) else {
                        bail!("invalid varint field at byte {pos}");
                    };
                    pos += used;
                }
                (_, 1) => pos += 8,
                (_, 2) => {
                    let Some((len, used)) = read_varint(data, pos) else {
                        bail!("invalid length varint at byte {pos}");
                    };
                    pos += used;
                    pos = pos.saturating_add(len as usize);
                }
                (_, 5) => pos += 4,
                // Wire types 3/4 (groups) never appear in SentencePiece
                // models; stop like the template rather than misparse.
                _ => break,
            }
        }

        if pieces.is_empty() {
            bail!("no pieces found in tokenizer.model");
        }

        let mut encode_map: HashMap<String, (usize, f32)> = HashMap::with_capacity(pieces.len());
        let mut max_piece_chars = 1usize;
        for (id, piece) in pieces.iter().enumerate() {
            let encodable = matches!(piece.kind, PieceKind::Normal | PieceKind::UserDefined);
            if !encodable || piece.text.is_empty() {
                continue;
            }
            match encode_map.entry(piece.text.clone()) {
                Entry::Vacant(slot) => {
                    slot.insert((id, piece.score));
                    max_piece_chars = max_piece_chars.max(piece.text.chars().count());
                }
                Entry::Occupied(mut slot) => {
                    // Duplicate piece strings should not exist in a valid
                    // model; keep the higher-scoring id if they do.
                    if piece.score > slot.get().1 {
                        slot.insert((id, piece.score));
                    }
                }
            }
        }

        Ok(Self {
            pieces,
            encode_map,
            max_piece_chars,
        })
    }

    /// Number of pieces, equal to the vocabulary size the decoder sees.
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    /// True when the piece table is empty (never, for a parsed model).
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// The piece for a token id, if in range.
    pub fn piece(&self, id: usize) -> Option<&SpPiece> {
        self.pieces.get(id)
    }

    /// Decode token ids through the parsed piece table, mapping the
    /// word-start marker to a space. Unlike parakeet's
    /// `SentencePieceVocab::decode`, the leading space is preserved so
    /// round-trip checks can assert it. Out-of-range ids decode to nothing,
    /// matching the decoder's behavior.
    pub fn decode(&self, ids: &[usize]) -> String {
        let mut out = String::new();
        for &id in ids {
            if let Some(piece) = self.pieces.get(id) {
                for ch in piece.text.chars() {
                    out.push(if ch == WORD_START { ' ' } else { ch });
                }
            }
        }
        out
    }

    /// Encode one exact surface form into a token path via unigram Viterbi:
    /// normalize (trim, collapse internal whitespace to the word-start
    /// marker, prepend the marker), then pick the segmentation with maximal
    /// summed piece score, breaking ties toward fewer pieces. Returns `None`
    /// when the term is empty or contains characters no piece covers.
    ///
    /// Correctness check built into the API (spec step 2.5): the encoded
    /// pieces must concatenate back to the exact normalized text; a mismatch
    /// is skipped with a warning instead of biasing a wrong path.
    pub fn encode(&self, term: &str) -> Option<Vec<usize>> {
        let normalized = normalize_term(term)?;
        let ids = self.viterbi(&normalized)?;
        let expected = normalized.replace(WORD_START, " ");
        let decoded = self.decode(&ids);
        if decoded != expected {
            tracing::warn!(
                term = %term,
                "sentencepiece round-trip mismatch; term skipped from biasing"
            );
            return None;
        }
        Some(ids)
    }

    /// Encode up to three casing variants of a term (spec step 2.3): as
    /// written, lowercase, and first-letter-capitalized when distinct. The
    /// acoustic model emits its own casing; biasing every plausible surface
    /// form lets the existing post-hoc `correct_text` recase the output.
    /// Deduplicated; empty (with a warning) when no variant is encodable,
    /// in which case the term still gets Dictionary v1 treatment.
    pub fn encode_term_variants(&self, term: &str) -> Vec<Vec<usize>> {
        let trimmed = term.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        let lower = trimmed.to_lowercase();
        let capitalized = capitalize_first(&lower);
        let mut forms: Vec<String> = Vec::with_capacity(3);
        for form in [trimmed.to_string(), lower, capitalized] {
            if !forms.contains(&form) {
                forms.push(form);
            }
        }

        let mut paths: Vec<Vec<usize>> = Vec::with_capacity(forms.len());
        for form in &forms {
            if let Some(ids) = self.encode(form)
                && !paths.contains(&ids)
            {
                paths.push(ids);
            }
        }
        if paths.is_empty() {
            tracing::warn!(
                term = %term,
                "no SentencePiece coverage for term; engine biasing skipped \
                 (the post-hoc dictionary pass still applies)"
            );
        }
        paths
    }

    /// Token ids whose pieces look like language tags (`<en>`, `<en-US>`).
    /// Same detection shape as parakeet-rs, so the biasing exclusion list
    /// matches what the engine strips from transcripts. Empty for the
    /// English-only vocabulary.
    pub fn lang_tag_ids(&self) -> Vec<usize> {
        self.pieces
            .iter()
            .enumerate()
            .filter_map(|(id, piece)| is_lang_tag(&piece.text).then_some(id))
            .collect()
    }

    /// Dynamic program over char boundaries of the normalized term, choosing
    /// the piece segmentation with maximal summed score (ties: fewer
    /// pieces). `None` when no full segmentation exists.
    fn viterbi(&self, normalized: &str) -> Option<Vec<usize>> {
        #[derive(Clone, Copy)]
        struct Best {
            score: f32,
            prev: usize,
            piece: usize,
            count: u32,
        }

        let mut bounds: Vec<usize> = normalized.char_indices().map(|(i, _)| i).collect();
        bounds.push(normalized.len());
        let n = bounds.len() - 1;
        if n == 0 {
            return None;
        }

        let mut best: Vec<Option<Best>> = vec![None; n + 1];
        best[0] = Some(Best {
            score: 0.0,
            prev: 0,
            piece: usize::MAX,
            count: 0,
        });

        for i in 1..=n {
            let lo = i.saturating_sub(self.max_piece_chars);
            for j in lo..i {
                let Some(prev) = best[j] else { continue };
                let Some(&(id, score)) = self.encode_map.get(&normalized[bounds[j]..bounds[i]])
                else {
                    continue;
                };
                let cand = Best {
                    score: prev.score + score,
                    prev: j,
                    piece: id,
                    count: prev.count + 1,
                };
                let better = match best[i] {
                    None => true,
                    Some(cur) => {
                        cand.score > cur.score
                            || (cand.score == cur.score && cand.count < cur.count)
                    }
                };
                if better {
                    best[i] = Some(cand);
                }
            }
        }

        let mut ids = Vec::new();
        let mut at = n;
        while at > 0 {
            let step = best[at]?;
            ids.push(step.piece);
            at = step.prev;
        }
        ids.reverse();
        Some(ids)
    }
}

/// Trim, collapse internal whitespace runs to the word-start marker, and
/// prepend the marker. `None` for empty or whitespace-only input.
fn normalize_term(term: &str) -> Option<String> {
    let mut out = String::new();
    for word in term.split_whitespace() {
        out.push(WORD_START);
        out.push_str(word);
    }
    (!out.is_empty()).then_some(out)
}

/// Uppercase the first char (Unicode-aware), keep the rest as given.
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Piece looks like a language tag: `<xx>` or `<xx-XX>`.
fn is_lang_tag(piece: &str) -> bool {
    let bytes = piece.as_bytes();
    if bytes.len() < 4 || bytes[0] != b'<' || bytes[bytes.len() - 1] != b'>' {
        return false;
    }
    let inner = &bytes[1..bytes.len() - 1];
    match inner.len() {
        2 => inner.iter().all(u8::is_ascii_lowercase),
        5 => {
            inner[0].is_ascii_lowercase()
                && inner[1].is_ascii_lowercase()
                && inner[2] == b'-'
                && inner[3].is_ascii_uppercase()
                && inner[4].is_ascii_uppercase()
        }
        _ => false,
    }
}

/// Read a protobuf varint at `start`. Returns `(value, bytes_used)`.
fn read_varint(data: &[u8], start: usize) -> Option<(u64, usize)> {
    let bytes = data.get(start..)?;
    let mut result: u64 = 0;
    for (i, &byte) in bytes.iter().take(10).enumerate() {
        result |= u64::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
    }
    None
}

/// Parse one `SentencePiece` submessage: field 1 text, field 2 f32 score
/// (defaults 0.0), field 3 type varint (defaults NORMAL, as in the proto).
fn parse_piece(data: &[u8]) -> Option<SpPiece> {
    let mut pos = 0usize;
    let mut text = String::new();
    let mut score = 0.0f32;
    let mut kind = PieceKind::Normal;

    while pos < data.len() {
        let (header, used) = read_varint(data, pos)?;
        pos += used;
        let field = header >> 3;
        let wire = header & 0x7;

        match (field, wire) {
            (1, 2) => {
                let (len, used) = read_varint(data, pos)?;
                pos += used;
                let len = len as usize;
                if len > data.len() - pos {
                    return None;
                }
                text = String::from_utf8_lossy(&data[pos..pos + len]).into_owned();
                pos += len;
            }
            (2, 5) => {
                let bytes = data.get(pos..pos + 4)?;
                score = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                pos += 4;
            }
            (3, 0) => {
                let (value, used) = read_varint(data, pos)?;
                pos += used;
                kind = PieceKind::from_proto(value);
            }
            (_, 0) => {
                let (_, used) = read_varint(data, pos)?;
                pos += used;
            }
            (_, 1) => pos += 8,
            (_, 2) => {
                let (len, used) = read_varint(data, pos)?;
                pos += used;
                pos = pos.saturating_add(len as usize);
            }
            (_, 5) => pos += 4,
            _ => break,
        }
    }

    Some(SpPiece { text, score, kind })
}

/// Builders for tiny synthetic `.model` bytes used by the hermetic tests
/// here and in `biasing::mod`. The real 13k-piece file is never committed;
/// this fixture exercises every wire type the parser walks.
#[cfg(test)]
pub(crate) mod testutil {
    use super::SpModel;

    pub(crate) fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out
    }

    pub(crate) fn key(field: u64, wire: u64) -> Vec<u8> {
        varint((field << 3) | wire)
    }

    /// One `SentencePiece` submessage: text (field 1), optional score
    /// (field 2, fixed32) and type (field 3, varint).
    pub(crate) fn piece_bytes(text: &str, score: Option<f32>, kind: Option<u64>) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(key(1, 2));
        out.extend(varint(text.len() as u64));
        out.extend_from_slice(text.as_bytes());
        if let Some(score) = score {
            out.extend(key(2, 5));
            out.extend_from_slice(&score.to_le_bytes());
        }
        if let Some(kind) = kind {
            out.extend(key(3, 0));
            out.extend(varint(kind));
        }
        out
    }

    /// Wrap piece submessages into a top-level `ModelProto`.
    pub(crate) fn model_bytes(pieces: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        for piece in pieces {
            out.extend(key(1, 2));
            out.extend(varint(piece.len() as u64));
            out.extend_from_slice(piece);
        }
        out
    }

    // Fixture token ids referenced by tests.
    pub(crate) const ID_RE: usize = 3; // "\u{2581}re"
    pub(crate) const ID_K: usize = 4; // "k"
    pub(crate) const ID_REKODY: usize = 6; // "\u{2581}rekody"
    pub(crate) const ID_AB: usize = 7; // "\u{2581}ab"
    pub(crate) const ID_CD: usize = 10; // "\u{2581}cd"
    pub(crate) const ID_CORE: usize = 13; // "\u{2581}core"
    pub(crate) const ID_ML: usize = 14; // "\u{2581}ml"
    pub(crate) const ID_X: usize = 15; // "\u{2581}x" (USER_DEFINED)

    /// The standard 23-piece fixture: control/unknown/user-defined/byte
    /// pieces, absent score and type fields, lang-tag lookalikes, plus
    /// score layouts that pin the Viterbi max-score and tie-break rules.
    pub(crate) fn tiny_model_pieces() -> Vec<Vec<u8>> {
        vec![
            piece_bytes("<unk>", None, Some(2)),             // 0
            piece_bytes("<s>", None, Some(3)),               // 1
            piece_bytes("</s>", None, Some(3)),              // 2
            piece_bytes("\u{2581}re", Some(-1.0), Some(1)),  // 3
            piece_bytes("k", Some(-2.0), None),              // 4
            piece_bytes("ody", Some(-1.5), None),            // 5
            piece_bytes("\u{2581}rekody", Some(-3.0), None), // 6
            piece_bytes("\u{2581}ab", Some(-1.0), None),     // 7
            piece_bytes("\u{2581}a", Some(-1.0), None),      // 8
            piece_bytes("b", Some(-1.0), None),              // 9
            piece_bytes("\u{2581}cd", Some(-2.0), None),     // 10
            piece_bytes("\u{2581}c", Some(-1.0), None),      // 11
            piece_bytes("d", Some(-1.0), None),              // 12
            piece_bytes("\u{2581}core", Some(-1.0), None),   // 13
            piece_bytes("\u{2581}ml", Some(-1.0), None),     // 14
            piece_bytes("\u{2581}x", Some(-0.5), Some(4)),   // 15
            piece_bytes("\u{2581}sys", Some(-0.1), Some(3)), // 16
            piece_bytes("<en>", None, Some(3)),              // 17
            piece_bytes("<en-US>", None, Some(3)),           // 18
            piece_bytes("<EN>", None, Some(3)),              // 19
            piece_bytes("<notatag>", None, Some(3)),         // 20
            piece_bytes("zz", None, None),                   // 21
            piece_bytes("<0x41>", Some(-9.0), Some(6)),      // 22
        ]
    }

    pub(crate) fn tiny_model() -> SpModel {
        SpModel::from_bytes(&model_bytes(&tiny_model_pieces())).expect("fixture parses")
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn parser_reads_pieces_scores_and_types() {
        let sp = tiny_model();
        assert_eq!(sp.len(), 23);
        assert!(!sp.is_empty());

        let unk = sp.piece(0).unwrap();
        assert_eq!(unk.text, "<unk>");
        assert_eq!(unk.kind, PieceKind::Unknown);

        let re = sp.piece(ID_RE).unwrap();
        assert_eq!(re.text, "\u{2581}re");
        assert_eq!(re.score, -1.0);
        assert_eq!(re.kind, PieceKind::Normal);

        // Absent type field defaults to NORMAL; score field present.
        let k = sp.piece(ID_K).unwrap();
        assert_eq!(k.score, -2.0);
        assert_eq!(k.kind, PieceKind::Normal);

        // Absent score and type fields default to 0.0 / NORMAL.
        let zz = sp.piece(21).unwrap();
        assert_eq!(zz.text, "zz");
        assert_eq!(zz.score, 0.0);
        assert_eq!(zz.kind, PieceKind::Normal);

        assert_eq!(sp.piece(ID_X).unwrap().kind, PieceKind::UserDefined);
        assert_eq!(sp.piece(16).unwrap().kind, PieceKind::Control);
        let byte = sp.piece(22).unwrap();
        assert_eq!(byte.kind, PieceKind::Byte);
        assert_eq!(byte.score, -9.0);

        assert!(sp.piece(23).is_none());
    }

    #[test]
    fn parser_skips_unknown_fields_and_wire_types() {
        // Same fixture with junk sprayed in: unknown top-level fields of
        // every skippable wire type, plus unknown fields inside a piece.
        let mut data = Vec::new();
        data.extend(key(6, 0)); // varint field
        data.extend(varint(99));
        data.extend(key(7, 1)); // fixed64 field
        data.extend_from_slice(&42u64.to_le_bytes());
        data.extend(key(2, 2)); // trainer-spec-shaped submessage
        data.extend(varint(3));
        data.extend_from_slice(b"\x08\x01\x00");
        data.extend(key(8, 5)); // fixed32 field
        data.extend_from_slice(&1.5f32.to_le_bytes());

        for (idx, piece) in tiny_model_pieces().into_iter().enumerate() {
            let mut piece = piece;
            if idx == 3 {
                // Junk inside one piece message: varint, fixed64, fixed32,
                // and length-delimited unknown fields.
                piece.extend(key(10, 0));
                piece.extend(varint(7));
                piece.extend(key(11, 1));
                piece.extend_from_slice(&7u64.to_le_bytes());
                piece.extend(key(12, 5));
                piece.extend_from_slice(&2.5f32.to_le_bytes());
                piece.extend(key(13, 2));
                piece.extend(varint(2));
                piece.extend_from_slice(b"hi");
            }
            data.extend(key(1, 2));
            data.extend(varint(piece.len() as u64));
            data.extend_from_slice(&piece);
        }

        let noisy = SpModel::from_bytes(&data).expect("noisy fixture parses");
        let clean = tiny_model();
        assert_eq!(noisy.len(), clean.len());
        for id in 0..clean.len() {
            let (a, b) = (noisy.piece(id).unwrap(), clean.piece(id).unwrap());
            assert_eq!(a.text, b.text, "text mismatch at id {id}");
            assert_eq!(a.score, b.score, "score mismatch at id {id}");
            assert_eq!(a.kind, b.kind, "kind mismatch at id {id}");
        }
    }

    #[test]
    fn parser_rejects_input_without_pieces() {
        assert!(SpModel::from_bytes(&[]).is_err());
        // A lone unknown varint field, no pieces.
        let mut data = key(6, 0);
        data.extend(varint(5));
        assert!(SpModel::from_bytes(&data).is_err());
        // Truncated piece: declared length exceeds the remaining bytes.
        let mut data = key(1, 2);
        data.extend(varint(100));
        data.extend_from_slice(b"abc");
        assert!(SpModel::from_bytes(&data).is_err());
    }

    #[test]
    fn viterbi_picks_max_score_segmentation() {
        let sp = tiny_model();
        // "\u{2581}ab" (-1.0) beats "\u{2581}a" + "b" (-2.0).
        assert_eq!(sp.encode("ab"), Some(vec![ID_AB]));
        // "\u{2581}rekody" (-3.0) beats "\u{2581}re" + "k" + "ody" (-4.5).
        assert_eq!(sp.encode("rekody"), Some(vec![ID_REKODY]));
        // No single piece covers "rek": multi-piece path wins.
        assert_eq!(sp.encode("rek"), Some(vec![ID_RE, ID_K]));
    }

    #[test]
    fn viterbi_breaks_ties_with_fewer_pieces() {
        let sp = tiny_model();
        // "\u{2581}cd" (-2.0) ties "\u{2581}c" + "d" (-2.0): fewer pieces win.
        assert_eq!(sp.encode("cd"), Some(vec![ID_CD]));
    }

    #[test]
    fn round_trip_reproduces_term_with_leading_space() {
        let sp = tiny_model();
        for (term, expected) in [
            ("rekody", " rekody"),
            ("rek", " rek"),
            ("core ml", " core ml"),
        ] {
            let ids = sp.encode(term).unwrap();
            assert_eq!(sp.decode(&ids), expected);
        }
        // Multi-word phrase spans a word boundary as a longer token path.
        assert_eq!(sp.encode("core ml"), Some(vec![ID_CORE, ID_ML]));
    }

    #[test]
    fn whitespace_collapses_before_encoding() {
        let sp = tiny_model();
        assert_eq!(sp.encode("  core \t  ml "), sp.encode("core ml"));
        assert_eq!(sp.encode("   "), None);
        assert_eq!(sp.encode(""), None);
    }

    #[test]
    fn casing_variants_deduplicate() {
        let sp = tiny_model();
        // Fixture vocabulary is lowercase-only: for "Rekody" only the
        // lowercase form encodes; as-written and capitalized fail.
        assert_eq!(sp.encode_term_variants("Rekody"), vec![vec![ID_REKODY]]);
        // "rekody": as-written and lowercase are the same string, and the
        // capitalized form is unencodable, so exactly one path survives.
        assert_eq!(sp.encode_term_variants("rekody"), vec![vec![ID_REKODY]]);
        // "REKODY": only the lowercase form encodes.
        assert_eq!(sp.encode_term_variants("REKODY"), vec![vec![ID_REKODY]]);
    }

    #[test]
    fn unencodable_input_returns_none_and_no_variants() {
        let sp = tiny_model();
        assert_eq!(sp.encode("qqq"), None);
        assert!(sp.encode_term_variants("qqq").is_empty());
        assert!(sp.encode_term_variants("   ").is_empty());
        assert!(sp.encode_term_variants("").is_empty());
    }

    #[test]
    fn control_pieces_never_encode_user_defined_do() {
        let sp = tiny_model();
        // "\u{2581}sys" exists but is CONTROL, and no other pieces cover the
        // letters, so the term is unencodable.
        assert_eq!(sp.encode("sys"), None);
        // USER_DEFINED pieces are encodable.
        assert_eq!(sp.encode("x"), Some(vec![ID_X]));
        assert_eq!(sp.decode(&[ID_X]), " x");
    }

    #[test]
    fn lang_tag_ids_detected_by_shape() {
        let sp = tiny_model();
        // `<en>` and `<en-US>` match; `<EN>` and `<notatag>` do not.
        assert_eq!(sp.lang_tag_ids(), vec![17, 18]);
    }

    #[test]
    fn decode_skips_out_of_range_ids() {
        let sp = tiny_model();
        assert_eq!(sp.decode(&[ID_REKODY, 999]), " rekody");
    }

    /// Real-tokenizer validation, skip-if-missing like
    /// `tests/short_audio_smoke.rs`: parse the shipped `tokenizer.model`,
    /// assert id parity with parakeet's parser, encode the real dictionary
    /// terms, and round-trip the ids through both piece tables.
    #[test]
    fn real_tokenizer_round_trips_dictionary_terms() {
        let Some(path) = real_tokenizer_path() else {
            eprintln!(
                "skipping real-tokenizer test: no tokenizer.model under \
                 ~/.local/share/rekody/models/nemotron-en-int8"
            );
            return;
        };

        let sp = SpModel::from_file(&path).expect("shipped tokenizer.model parses");
        let vocab = parakeet_rs::SentencePieceVocab::from_file(&path)
            .expect("parakeet parses the same file");
        assert_eq!(
            sp.len(),
            vocab.size(),
            "piece table must stay id-compatible with parakeet's parser"
        );

        for term in ["Rekody", "Kipkemboi", "Chamgei", "Ollama", "Core ML"] {
            let paths = sp.encode_term_variants(term);
            assert!(
                !paths.is_empty(),
                "term {term:?} must have at least one encodable casing variant"
            );

            let lower = term.to_lowercase();
            let forms = [term.to_string(), lower.clone(), capitalize_first(&lower)];

            for ids in &paths {
                let ours = sp.decode(ids);
                let theirs = vocab.decode(ids);
                let pieces: Vec<&str> = ids
                    .iter()
                    .map(|&id| sp.piece(id).map(|p| p.text.as_str()).unwrap_or_default())
                    .collect();
                eprintln!("term {term:?}: ids {ids:?} pieces {pieces:?} decoded {ours:?}");

                // Ours keeps the leading space; parakeet's decode trims it.
                let matched = forms.iter().any(|form| {
                    let collapsed = form.split_whitespace().collect::<Vec<_>>().join(" ");
                    ours == format!(" {collapsed}") && theirs == collapsed
                });
                assert!(
                    matched,
                    "round-trip mismatch for {term:?}: ids {ids:?} ours {ours:?} \
                     parakeet {theirs:?}"
                );
            }
        }
    }

    fn real_tokenizer_path() -> Option<std::path::PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let path = std::path::PathBuf::from(home)
            .join(".local/share/rekody/models/nemotron-en-int8/tokenizer.model");
        path.exists().then_some(path)
    }
}
