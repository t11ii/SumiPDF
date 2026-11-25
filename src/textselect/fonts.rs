//! Character advances are sufficient for highlighting: PDF glyph codes need not
//! be translated to Unicode. Composite Identity-H fonts use a two-byte CID.
use super::{number, standard};
use lopdf::{Dictionary, Document, Object};

struct WidthRun {
    first: u16,
    last: u16,
    width: f32,
}

enum Widths {
    Standard(&'static [u16; 95]),
    Fixed(f32),
    Simple {
        first: usize,
        table: Vec<f32>,
        missing: f32,
    },
    Cid {
        table: Vec<WidthRun>,
        default: f32,
    },
}

pub(super) struct Font {
    pub two_byte: bool,
    pub ascent: f32,
    pub descent: f32,
    widths: Widths,
}

impl Font {
    pub fn from_dict(doc: &Document, font: &Dictionary) -> Option<Self> {
        let subtype = font.get(b"Subtype").ok()?.as_name().ok()?;
        let two_byte = subtype == b"Type0";
        let metrics = if two_byte {
            if font.get_deref(b"Encoding", doc).ok()?.as_name().ok()? != b"Identity-H" {
                return None;
            }
            let kid = font
                .get_deref(b"DescendantFonts", doc)
                .ok()?
                .as_array()
                .ok()?
                .first()?;
            doc.dereference(kid).ok()?.1.as_dict().ok()?
        } else {
            if !matches!(subtype, b"Type1" | b"MMType1" | b"TrueType") {
                return None;
            }
            font
        };
        let descriptor = metrics
            .get_deref(b"FontDescriptor", doc)
            .ok()
            .and_then(|d| d.as_dict().ok());
        let desc_num = |key: &[u8]| descriptor?.get_deref(key, doc).ok().and_then(number);
        let (mut ascent, mut descent) = if two_byte {
            (880.0, -120.0)
        } else {
            (760.0, -210.0)
        };
        if let (Some(a), Some(d)) = (desc_num(b"Ascent"), desc_num(b"Descent"))
            && a > 0.0
            && d <= 0.0
            && a - d <= 2500.0
        {
            (ascent, descent) = (a, d);
        }
        let widths = if two_byte {
            let default = metrics
                .get_deref(b"DW", doc)
                .ok()
                .and_then(number)
                .unwrap_or(1000.0);
            let table = match metrics.get_deref(b"W", doc).ok() {
                Some(o) => cid_widths(doc, o.as_array().ok()?)?,
                None => Vec::new(),
            };
            Widths::Cid { table, default }
        } else if let Ok(array) = metrics.get_deref(b"Widths", doc) {
            let first =
                usize::try_from(metrics.get_deref(b"FirstChar", doc).ok()?.as_i64().ok()?).ok()?;
            let table = array
                .as_array()
                .ok()?
                .iter()
                .take(256)
                .map(|v| number(doc.dereference(v).ok()?.1))
                .collect::<Option<Vec<_>>>()?;
            Widths::Simple {
                first,
                table,
                missing: desc_num(b"MissingWidth").unwrap_or(0.0),
            }
        } else {
            let name = font.get(b"BaseFont").ok()?.as_name().ok()?;
            match name {
                b"Courier" | b"Courier-Oblique" | b"Courier-Bold" | b"Courier-BoldOblique" => {
                    Widths::Fixed(600.0)
                }
                b"Helvetica" | b"Helvetica-Oblique" => Widths::Standard(&standard::HELVETICA),
                b"Helvetica-Bold" | b"Helvetica-BoldOblique" => {
                    Widths::Standard(&standard::HELVETICA_BOLD)
                }
                b"Times-Roman" => Widths::Standard(&standard::TIMES),
                b"Times-Bold" => Widths::Standard(&standard::TIMES_BOLD),
                b"Times-Italic" => Widths::Standard(&standard::TIMES_ITALIC),
                b"Times-BoldItalic" => Widths::Standard(&standard::TIMES_BOLD_ITALIC),
                _ => return None,
            }
        };
        Some(Self {
            two_byte,
            ascent: ascent / 1000.0,
            descent: descent / 1000.0,
            widths,
        })
    }

    pub fn advance(&self, code: u16) -> Option<f32> {
        let width = match &self.widths {
            Widths::Standard(table) => f32::from(*table.get(usize::from(code).checked_sub(32)?)?),
            Widths::Fixed(w) => *w,
            Widths::Simple {
                first,
                table,
                missing,
            } => usize::from(code)
                .checked_sub(*first)
                .and_then(|n| table.get(n))
                .copied()
                .unwrap_or(*missing),
            Widths::Cid { table, default } => {
                let i = table.partition_point(|run| run.last < code);
                table
                    .get(i)
                    .filter(|run| run.first <= code)
                    .map_or(*default, |run| run.width)
            }
        };
        Some(width / 1000.0)
    }
}

fn cid_widths(doc: &Document, objects: &[Object]) -> Option<Vec<WidthRun>> {
    let mut table = Vec::new();
    let mut items = objects.iter();
    while let Some(start) = items.next() {
        let first = u16::try_from(start.as_i64().ok()?).ok()?;
        let second = doc.dereference(items.next()?).ok()?.1;
        if let Ok(widths) = second.as_array() {
            for (offset, width) in widths.iter().enumerate() {
                let cid = first.checked_add(u16::try_from(offset).ok()?)?;
                table.push(WidthRun {
                    first: cid,
                    last: cid,
                    width: number(doc.dereference(width).ok()?.1)?,
                });
            }
        } else {
            let last = u16::try_from(second.as_i64().ok()?).ok()?;
            if last < first {
                return None;
            }
            table.push(WidthRun {
                first,
                last,
                width: number(doc.dereference(items.next()?).ok()?.1)?,
            });
        }
        if table.len() > 65536 {
            return None;
        }
    }
    table.sort_unstable_by_key(|run| run.first);
    if table.windows(2).any(|pair| pair[0].last >= pair[1].first) {
        return None;
    }
    Some(table)
}
