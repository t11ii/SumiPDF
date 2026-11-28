use super::{Direction, LIMIT, Line, TextDocument, TextLayout, UnitRect};

struct CachedPage {
    index: u32,
    layout: TextLayout,
    text: SearchText,
}

impl CachedPage {
    fn heap_bytes(&self) -> usize {
        self.layout.glyphs.capacity() * size_of::<UnitRect>()
            + self.layout.lines.capacity() * size_of::<Line>()
            + self.text.text.capacity()
            + self.text.ends.capacity() * size_of::<usize>()
    }
}

/// Owned by Find for one open document. Keep the first pages that fit: Find
/// visits pages in order, so a later full scan always benefits from that prefix.
pub(crate) struct SearchPageCache {
    pages: Vec<CachedPage>,
    page_bytes: usize,
    byte_limit: usize,
}

impl Default for SearchPageCache {
    fn default() -> Self {
        Self::with_limit(8 * 1024 * 1024)
    }
}

impl SearchPageCache {
    pub(super) fn with_limit(byte_limit: usize) -> Self {
        Self {
            pages: Vec::new(),
            page_bytes: 0,
            byte_limit,
        }
    }
    fn position(&self, index: u32) -> Result<usize, usize> {
        self.pages.binary_search_by_key(&index, |p| p.index)
    }

    pub(crate) fn contains(&self, index: u32) -> bool {
        self.position(index).is_ok()
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.page_bytes + self.pages.capacity() * size_of::<CachedPage>()
    }

    pub(crate) fn search(
        &mut self,
        doc: &TextDocument,
        index: u32,
        query: &str,
    ) -> Result<Vec<Vec<UnitRect>>, String> {
        let insert_at = match self.position(index) {
            Ok(slot) => {
                let entry = &self.pages[slot];
                return Ok(entry.text.matches(&entry.layout, query));
            }
            Err(slot) => slot,
        };
        let (layout, text) = doc.scan_page(index, Some(SearchText::default()))?;
        let text = text.expect("successful search scan returns its text");
        let hits = text.matches(&layout, query);
        self.retain(
            insert_at,
            CachedPage {
                index,
                layout,
                text,
            },
        );
        Ok(hits)
    }

    fn retain(&mut self, slot: usize, entry: CachedPage) {
        let bytes = entry.heap_bytes();
        let room = self.byte_limit - self.retained_bytes();
        if bytes > room {
            return;
        }
        if self.pages.len() == self.pages.capacity() {
            // Grow the lookup table geometrically within the same budget.
            let slots = ((room - bytes) / size_of::<CachedPage>()).min(self.pages.len().max(1));
            if slots == 0 || self.pages.try_reserve_exact(slots).is_err() {
                return;
            }
        }
        self.pages.insert(slot, entry);
        self.page_bytes += bytes;
    }
}

#[derive(Default)]
pub(super) struct SearchText {
    text: String,
    ends: Vec<usize>,
}

impl SearchText {
    pub(super) fn interrupt(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\0') {
            self.text.push('\0');
        }
    }

    pub(super) fn append(&mut self, layout: &TextLayout, ch: Option<char>) -> Result<(), String> {
        if let Some(ch) = ch {
            let i = self.ends.len();
            if i > 0 {
                let line = layout.lines.last().unwrap();
                let prev = layout.glyphs[i - 1];
                let next = layout.glyphs[i];
                let wrapped_cjk =
                    line.start == i && self.text.chars().next_back().is_some_and(cjk) && cjk(ch);
                if !wrapped_cjk && (line.start == i || word_gap(prev, next, line.direction)) {
                    fold(' ', &mut self.text);
                }
            }
            fold(ch, &mut self.text);
        } else {
            self.interrupt();
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
