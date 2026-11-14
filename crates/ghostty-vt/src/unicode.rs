use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

pub const KITTY_UNICODE_PLACEHOLDER: u32 = 0x10EEEE;

/// Returns the terminal display width of a single Unicode codepoint in grid cells (0, 1, or 2).
///
/// Out-of-range values (`> 0x10FFFF`) return `1`, surrogates (`0xD800..=0xDFFF`) return `0`,
/// regional indicators return `2`, and all other scalar values follow terminal cell width rules.
#[inline]
pub fn unicode_codepoint_width(cp: u32) -> u8 {
    if cp > 0x10FFFF {
        return 1;
    }
    if (0xD800..=0xDFFF).contains(&cp) {
        return 0;
    }
    if cp == KITTY_UNICODE_PLACEHOLDER {
        return 1;
    }
    // Regional indicator symbols are width 2 in terminal grids.
    if (0x1F1E6..=0x1F1FF).contains(&cp) {
        return 2;
    }
    // Three-em / two-em dashes that some tables report >= 2 are clamped to 2.
    if matches!(cp, 0x2E3A | 0x2E3B) {
        return 2;
    }
    // Default-ignorable codepoints (zero-width spaces, joiners, variation selectors, tags).
    if is_zero_width_ignorable(cp) {
        return 0;
    }
    let Some(ch) = char::from_u32(cp) else {
        return 0;
    };
    UnicodeWidthChar::width(ch).unwrap_or(0).min(2) as u8
}

/// Measures the first extended grapheme cluster in `cps`, returning `(consumed_codepoints, width)`.
///
/// Matches Ghostty's mode-2027 grapheme width rules:
/// - Empty input returns `(0, 0)`.
/// - Out-of-range initial codepoint (`> 0x10FFFF`) returns `(1, 1)`; initial surrogate returns `(1, 0)`.
/// - VS16 (`U+FE0F`) promotes a valid emoji variation base to width `2`; VS15 (`U+FE0E`) narrows it to `1`.
/// - Subsequent non-zero-width codepoints in the cluster (ZWJ sequences, regional indicator pairs,
///   emoji skin-tone modifiers, halfwidth katakana voiced marks) promote the cluster to width `2`.
pub fn unicode_grapheme_width(cps: &[u32]) -> (usize, u8) {
    if cps.is_empty() {
        return (0, 0);
    }
    let first = cps[0];
    if first > 0x10FFFF {
        return (1, 1);
    }
    let Some(first_char) = char::from_u32(first) else {
        return (1, 0);
    };

    // Fast path for single ASCII scalar with no combining continuation.
    if first < 0x80
        && (cps.len() == 1 || cps[1] < 0x80 && cps[1] >= 0x20 && cps[1] != 0x7F)
        && !(first == u32::from(b'\r') && cps.get(1) == Some(&u32::from(b'\n')))
    {
        return (1, unicode_codepoint_width(first));
    }

    // Collect valid scalar prefix up to the first invalid u32 or control boundary.
    let mut text = String::with_capacity(cps.len().min(16) * 4);
    text.push(first_char);
    for &cp in &cps[1..] {
        let Some(ch) = char::from_u32(cp) else {
            break;
        };
        text.push(ch);
    }

    let Some(cluster) = text.graphemes(true).next() else {
        return (1, unicode_codepoint_width(first));
    };

    let mut chars = cluster.chars();
    let Some(base) = chars.next() else {
        return (1, unicode_codepoint_width(first));
    };

    let mut count = 1usize;
    let mut width = unicode_codepoint_width(u32::from(base));
    let mut prev = u32::from(base);

    for ch in chars {
        count += 1;
        let cp = u32::from(ch);
        match grapheme_width_effect(prev, cp) {
            GraphemeWidthEffect::Ignore => {}
            GraphemeWidthEffect::NoChange => {
                prev = cp;
            }
            GraphemeWidthEffect::Wide => {
                width = 2;
                prev = cp;
            }
            GraphemeWidthEffect::Narrow => {
                width = 1;
                prev = cp;
            }
        }
    }

    (count, width)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GraphemeWidthEffect {
    Ignore,
    NoChange,
    Wide,
    Narrow,
}

#[inline]
pub(crate) fn grapheme_width_effect(prev: u32, cp: u32) -> GraphemeWidthEffect {
    if cp == 0xFE0F || cp == 0xFE0E {
        if !is_emoji_vs_base(prev) {
            return GraphemeWidthEffect::Ignore;
        }
        return if cp == 0xFE0F {
            GraphemeWidthEffect::Wide
        } else {
            GraphemeWidthEffect::Narrow
        };
    }

    if !is_width_zero_in_grapheme(cp) {
        return GraphemeWidthEffect::Wide;
    }

    GraphemeWidthEffect::NoChange
}

/// Checks whether `next` continues the existing cell grapheme cluster `(base, zerowidth)` under
/// mode 2027, and if so returns the resulting cluster width (`1` or `2`) and whether `next` should
/// be stored (vs ignored for invalid variation selectors).
pub(crate) fn check_grapheme_continuation(
    base: char,
    zerowidth: &[char],
    current_wide: bool,
    next: char,
) -> Option<(u8, bool)> {
    // Printable ASCII never continues a previous terminal cell's grapheme cluster.
    if (next as u32) < 0x80 {
        return None;
    }
    // Control characters never continue a grapheme cluster.
    if next.is_control() {
        return None;
    }

    let mut buf = String::with_capacity((zerowidth.len() + 2) * 4);
    buf.push(base);
    for &zw in zerowidth {
        buf.push(zw);
    }
    buf.push(next);

    let mut graphemes = buf.graphemes(true);
    let first = graphemes.next()?;
    if first.len() != buf.len() || graphemes.next().is_some() {
        return None;
    }

    let prev = zerowidth
        .last()
        .copied()
        .map(u32::from)
        .unwrap_or(u32::from(base));
    let mut width = if current_wide {
        2
    } else {
        unicode_codepoint_width(u32::from(base)).max(1)
    };
    let next_cp = u32::from(next);
    match grapheme_width_effect(prev, next_cp) {
        GraphemeWidthEffect::Ignore => Some((width, false)),
        GraphemeWidthEffect::NoChange => Some((width, true)),
        GraphemeWidthEffect::Wide => {
            width = 2;
            Some((width, true))
        }
        GraphemeWidthEffect::Narrow => {
            width = 1;
            Some((width, true))
        }
    }
}

#[inline]
fn is_zero_width_ignorable(cp: u32) -> bool {
    matches!(
        cp,
        0x00AD
            | 0x034F
            | 0x061C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFF8
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

#[inline]
fn is_width_zero_in_grapheme(cp: u32) -> bool {
    // Halfwidth katakana voiced/semi-voiced marks (U+FF9E, U+FF9F) have width 1 on their own
    // and widen halfwidth katakana clusters to 2 cells in Ghostty.
    if matches!(cp, 0xFF9E | 0xFF9F) {
        return false;
    }
    // Emoji skin-tone modifiers widen emoji bases to 2 cells.
    if (0x1F3FB..=0x1F3FF).contains(&cp) {
        return false;
    }
    unicode_codepoint_width(cp) == 0
}

/// Checks if `cp` can form a valid Unicode Emoji Variation Sequence (`emoji-variation-sequences.txt`).
#[inline]
fn is_emoji_vs_base(cp: u32) -> bool {
    matches!(
        cp,
        0x0023
            | 0x002A
            | 0x0030..=0x0039
            | 0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2194..=0x2199
            | 0x21A9..=0x21AA
            | 0x231A..=0x231B
            | 0x2328
            | 0x23CF
            | 0x23E9..=0x23F3
            | 0x23F8..=0x23FA
            | 0x24C2
            | 0x25AA..=0x25AB
            | 0x25B6
            | 0x25C0
            | 0x25FB..=0x25FE
            | 0x2600..=0x27BF
            | 0x2934..=0x2935
            | 0x2B05..=0x2B07
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1FAFF
    )
}
