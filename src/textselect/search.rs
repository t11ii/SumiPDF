use super::{Direction, LIMIT, TextLayout, UnitRect, fonts::Font};
use lopdf::{Dictionary, Document, Encoding, Object, content::Content};

pub(super) enum Decoder<'a> {
    Simple(Encoding<'a>),
    Mapped(Vec<UnicodeRange>),
}

pub(super) struct UnicodeRange {
    first: u16,
    last: u16,
    utf16: Vec<u16>,
}

impl<'a> Decoder<'a> {
    pub(super) fn for_font(
        doc: &'a Document,
        font: &'a Dictionary,
        two_byte: bool,
        budget: &mut usize,
    ) -> Option<Self> {
        if let Ok(stream) = font
            .get_deref(b"ToUnicode", doc)
            .and_then(Object::as_stream)
        {
            let bytes = stream
                .get_plain_content_with_limit((*budget).min(4 * 1024 * 1024))
                .ok()?;
            *budget = budget.checked_sub(bytes.len())?;
            return unicode_ranges(&bytes, two_byte).map(Self::Mapped);
        }
        // A CID is an index in its font's character collection, not a Unicode value.
        if two_byte {
            return None;
        }
        font.get_font_encoding_with_limit(doc, *budget)
            .ok()
            .map(Self::Simple)
    }

    fn decode(&self, code: u16, bytes: &[u8], dest: &mut String) {
        match self {
            Self::Simple(enc) => {
                if enc.write_to_string(bytes, dest).is_err() {
                    dest.clear();
                }
            }
            Self::Mapped(ranges) => {
                let i = ranges.partition_point(|r| r.last < code);
                if let Some(range) = ranges.get(i).filter(|r| r.first <= code) {
                    let (last, prefix) = range.utf16.split_last().expect("nonempty CMap mapping");
                    let units = prefix
                        .iter()
                        .copied()
                        .chain([last.wrapping_add(code - range.first)]);
                    for ch in char::decode_utf16(units) {
                        let Ok(ch) = ch else {
                            dest.clear();
                            break;
                        };
                        dest.push(ch);
                    }
                }
            }
        }
    }
}

// Read the forward ToUnicode mappings with lopdf's bounded stream and PDF object
// parsers. Keep bfrange intervals compressed: PDF code ranges can be enormous,
// and Find never needs to enumerate a reverse Unicode-to-character-code map.
fn unicode_ranges(bytes: &[u8], two_byte: bool) -> Option<Vec<UnicodeRange>> {
    let code = |obj: &Object| match (two_byte, obj.as_str().ok()?) {
        (false, &[x]) => Some(u16::from(x)),
        (true, &[hi, lo]) => Some(u16::from_be_bytes([hi, lo])),
        _ => None,
    };
    let mut ranges = Vec::new();
    let mut add = |first, last, target: &Object| -> Option<()> {
        let dest = target.as_str().ok()?;
        if dest.is_empty() || dest.len() > 512 || dest.len() % 2 != 0 || ranges.len() >= 65536 {
            return None;
        }
        let utf16 = dest
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_be_bytes)
            .collect();
        ranges.push(UnicodeRange { first, last, utf16 });
        Some(())
    };
    for op in Content::decode_iter(bytes) {
        let op = op.ok()?;
        match op.operator.as_str() {
            "endbfchar" => {
                for item in op.operands.as_chunks::<2>().0 {
                    if let Some(c) = code(&item[0]) {
                        add(c, c, &item[1])?;
                    }
                }
            }
            "endbfrange" => {
                for item in op.operands.as_chunks::<3>().0 {
                    let (Some(first), Some(last)) = (code(&item[0]), code(&item[1])) else {
                        continue;
                    };
                    if last < first {
                        return None;
                    }
                    if let Object::Array(targets) = &item[2] {
                        if targets.len() != usize::from(last - first) + 1 {
                            return None;
                        }
                        for (c, target) in (first..=last).zip(targets) {
                            add(c, c, target)?;
                        }
                    } else {
                        add(first, last, &item[2])?;
                    }
                }
            }
            _ => {}
        }
    }
    ranges.sort_unstable_by_key(|r| r.first);
    if ranges.is_empty() || ranges.windows(2).any(|r| r[0].last >= r[1].first) {
        return None;
    }
    Some(ranges)
}

// End offsets associate every character in the normalized text with a painted
// glyph, including one-to-many mappings (ligatures and Unicode case folds).
#[derive(Default)]
pub(super) struct SearchText {
    text: String,
    ends: Vec<usize>,
    decoded: String,
}

impl SearchText {
    pub(super) fn interrupt(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\0') {
            self.text.push('\0');
        }
    }

    pub(super) fn append(
        &mut self,
        layout: &TextLayout,
        font: &Font<'_>,
        code: u16,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.decoded.clear();
        if let Some(decoder) = &font.decoder {
            decoder.decode(code, bytes, &mut self.decoded);
        }
        if self.decoded.is_empty() {
            self.interrupt();
        } else {
            let i = self.ends.len();
            if i > 0 {
                let line = layout.lines.last().unwrap();
                let prev = layout.glyphs[i - 1];
                let next = layout.glyphs[i];
                let wrapped_cjk = line.start == i
                    && self.text.chars().next_back().is_some_and(cjk)
                    && self.decoded.chars().next().is_some_and(cjk);
                if !wrapped_cjk && (line.start == i || word_gap(prev, next, line.direction)) {
                    fold(' ', &mut self.text);
                }
            }
            for ch in self.decoded.chars() {
                fold(ch, &mut self.text);
            }
        }
        if self.text.len() > 2 * 1024 * 1024 {
            return Err(LIMIT.into());
        }
        self.ends.push(self.text.len());
        Ok(())
    }

    pub(super) fn matches(&self, layout: &TextLayout, query: &str) -> Vec<Vec<UnitRect>> {
        let mut needle = String::new();
        for ch in query.trim().chars() {
            fold(ch, &mut needle);
        }
        if needle.is_empty() || needle.contains('\0') {
            return Vec::new();
        }
        let mut matches = Vec::new();
        let mut previous = None;
        for (byte, _) in self.text.match_indices(needle.as_str()) {
            let first = self.ends.partition_point(|&end| end <= byte);
            let last = self.ends.partition_point(|&end| end < byte + needle.len());
            if previous != Some((first, last)) {
                matches.push(
                    layout
                        .runs(first, last)
                        .into_iter()
                        .map(|r| r.bounds)
                        .collect(),
                );
                previous = Some((first, last));
            }
        }
        matches
    }
}

fn fold(ch: char, dest: &mut String) {
    match ch {
        ws if ws.is_whitespace() => {
            if !dest.ends_with(' ') {
                dest.push(' ');
            }
        }
        'ß' | 'ẞ' => dest.push_str("ss"),
        'ς' => dest.push('σ'),
        'ﬀ' => dest.push_str("ff"),
        'ﬁ' => dest.push_str("fi"),
        'ﬂ' => dest.push_str("fl"),
        'ﬃ' => dest.push_str("ffi"),
        'ﬄ' => dest.push_str("ffl"),
        _ => dest.extend(ch.to_lowercase()),
    }
}

fn cjk(ch: char) -> bool {
    matches!(ch as u32, 0x2e80..=0x30ff | 0x3400..=0x9fff | 0xf900..=0xfaff | 0xff01..=0xffef | 0x20000..=0x323af)
}

fn word_gap(a: UnitRect, b: UnitRect, direction: Direction) -> bool {
    let (gap, scale) = match direction {
        Direction::Right => (b.left - a.right, (a.right - a.left).max(b.right - b.left)),
        Direction::Left => (a.left - b.right, (a.right - a.left).max(b.right - b.left)),
        Direction::Down => (b.top - a.bottom, (a.bottom - a.top).max(b.bottom - b.top)),
        Direction::Up => (a.top - b.bottom, (a.bottom - a.top).max(b.bottom - b.top)),
    };
    gap > 0.40 * scale
}
