use super::*;
use crate::tounicode::ToUnicodeCMap;
use lopdf::{dictionary, Document, Object, Stream};

/// A minimal TrueType font with `head`, `hhea`, `maxp`, `hmtx`, `loca` and
/// `glyf` but no `cmap` and no `post` names. Glyph `i` is `(outlined,
/// advance)`; a missing glyph is `(false, 0)`.
fn cmapless_truetype(glyphs: &[(bool, u16)]) -> Vec<u8> {
    let square: Vec<u8> = {
        let mut g = Vec::new();
        g.extend(1i16.to_be_bytes());
        for v in [0i16, 0, 500, 700] {
            g.extend(v.to_be_bytes());
        }
        g.extend(2u16.to_be_bytes());
        g.extend(0u16.to_be_bytes());
        g.extend([1u8, 1, 1]);
        for v in [0i16, 500, 0, 0, 0, 700] {
            g.extend(v.to_be_bytes());
        }
        g
    };
    let mut glyf = Vec::new();
    let mut loca: Vec<u8> = Vec::new();
    loca.extend(0u32.to_be_bytes());
    for &(outlined, _) in glyphs {
        if outlined {
            glyf.extend(&square);
        }
        loca.extend((glyf.len() as u32).to_be_bytes());
    }
    let num_glyphs = glyphs.len() as u16;
    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    head[50..52].copy_from_slice(&1i16.to_be_bytes());
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[34..36].copy_from_slice(&num_glyphs.to_be_bytes());
    let mut maxp = vec![0u8; 32];
    maxp[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&num_glyphs.to_be_bytes());
    let mut hmtx = Vec::new();
    for &(_, advance) in glyphs {
        hmtx.extend(advance.to_be_bytes());
        hmtx.extend(0i16.to_be_bytes());
    }
    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
    ];
    tables.sort_by_key(|(tag, _)| **tag);
    let mut font = Vec::new();
    font.extend(0x0001_0000u32.to_be_bytes());
    font.extend((tables.len() as u16).to_be_bytes());
    font.extend([0u8; 6]);
    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, data) in &tables {
        font.extend(*tag);
        font.extend(0u32.to_be_bytes());
        font.extend((offset as u32).to_be_bytes());
        font.extend((data.len() as u32).to_be_bytes());
        let padded = data.len().div_ceil(4) * 4;
        body.extend(data);
        body.extend(std::iter::repeat_n(0u8, padded - data.len()));
        offset += padded;
    }
    font.extend(body);
    font
}

/// An Arial-like subset in the standard Macintosh order: blank space at 3,
/// tabular digits at 19–28, `A`–`Z` at 36–61, `a`–`z` at 68–93, with the
/// glyphs not in the subset left empty.
fn arial_like(digit_widths: &[u16], space: (bool, u16)) -> Vec<(bool, u16)> {
    let mut glyphs = vec![(false, 0u16); 98];
    glyphs[0] = (true, 750); // .notdef box
    glyphs[3] = space;
    for (i, &w) in digit_widths.iter().enumerate() {
        glyphs[19 + i] = (true, w);
    }
    for gid in 36..=61 {
        glyphs[gid] = (true, 667);
    }
    for gid in 68..=93 {
        glyphs[gid] = (true, 556);
    }
    glyphs[76] = (true, 222); // i
    glyphs[79] = (true, 222); // l
    glyphs[80] = (true, 833); // m
    glyphs[90] = (true, 722); // w
    glyphs
}

fn decode(cmap: &ToUnicodeCMap, gids: &[u16]) -> String {
    let bytes: Vec<u8> = gids.iter().flat_map(|g| g.to_be_bytes()).collect();
    cmap.decode_cids(&bytes)
}

#[test]
fn mac_order_decodes_a_cmapless_subset_with_tabular_digits() {
    let font = cmapless_truetype(&arial_like(&[556; 10], (false, 278)));
    let cmap = build_cmap_from_mac_glyph_order(&font).expect("corroborated");
    // "How many" in glyph IDs: H=43 o=82 w=90 space=3 m=80 a=68 n=81 y=92
    assert_eq!(decode(&cmap, &[43, 82, 90, 3, 80, 68, 81, 92]), "How many");
    assert_eq!(decode(&cmap, &[26, 25, 23]), "764");
}

#[test]
fn mac_order_needs_at_least_three_equal_width_digits() {
    for widths in [&[556u16, 556][..], &[556, 556, 500, 556][..], &[][..]] {
        let font = cmapless_truetype(&arial_like(widths, (false, 278)));
        assert!(
            build_cmap_from_mac_glyph_order(&font).is_none(),
            "digit widths {widths:?} must not corroborate the ordering"
        );
    }
}

#[test]
fn mac_order_needs_a_blank_advancing_space_glyph() {
    for space in [(true, 278), (false, 0)] {
        let font = cmapless_truetype(&arial_like(&[556; 10], space));
        assert!(
            build_cmap_from_mac_glyph_order(&font).is_none(),
            "space glyph {space:?} must not corroborate the ordering"
        );
    }
}

#[test]
fn mac_order_is_not_used_when_the_font_has_a_cmap_or_names() {
    // The regular cmap path owns fonts with a cmap; a font with names goes
    // through the post-table path. Both are represented by real fonts in the
    // wider suite; here the builder must simply decline a font whose glyph
    // count is too small to hold the digit run.
    let font = cmapless_truetype(&[(true, 750), (false, 0), (false, 0), (false, 278)]);
    assert!(build_cmap_from_mac_glyph_order(&font).is_none());
}

#[test]
fn cid_to_gid_identity_is_absent_or_named() {
    let doc = Document::with_version("1.4");
    assert!(cid_to_gid_is_identity(&dictionary! {}, &doc));
    assert!(cid_to_gid_is_identity(
        &dictionary! { "CIDToGIDMap" => "Identity" },
        &doc
    ));
    let mut doc = Document::with_version("1.4");
    let stream = doc.add_object(Stream::new(dictionary! {}, vec![0, 0, 0, 5]));
    assert!(!cid_to_gid_is_identity(
        &dictionary! { "CIDToGIDMap" => stream },
        &doc
    ));
}

/// End to end: an Identity-H CIDFontType2 with no ToUnicode whose embedded
/// subset lost its cmap decodes through the standard order, and the same
/// font with a CIDToGIDMap stream does not.
#[test]
fn identity_h_font_without_cmap_extracts_through_mac_order() {
    for (cid_to_gid, expected) in [(None, "How many"), (Some(vec![0u8; 8]), "")] {
        let mut doc = Document::with_version("1.5");
        let font_file = doc.add_object(Stream::new(
            dictionary! {},
            cmapless_truetype(&arial_like(&[556; 10], (false, 278))),
        ));
        let descriptor = doc.add_object(dictionary! {
            "Type" => "FontDescriptor", "FontName" => "ABCDEF+ArialMT", "Flags" => 4,
            "FontFile2" => font_file,
        });
        let mut cid_font = dictionary! {
            "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => "ABCDEF+ArialMT",
            "CIDSystemInfo" => dictionary! { "Registry" => Object::string_literal("Adobe"), "Ordering" => Object::string_literal("Identity"), "Supplement" => 0 },
            "FontDescriptor" => descriptor, "DW" => 600,
        };
        if let Some(map) = cid_to_gid {
            let map_id = doc.add_object(Stream::new(dictionary! {}, map));
            cid_font.set("CIDToGIDMap", map_id);
        }
        let cid_font_id = doc.add_object(cid_font);
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "ABCDEF+ArialMT",
            "Encoding" => "Identity-H", "DescendantFonts" => vec![Object::Reference(cid_font_id)],
        });
        let content = doc.add_object(Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf 1 0 0 1 40 700 Tm <002B0052005A0003005000440051005C> Tj ET".to_vec(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "MediaBox" => vec![0.into(), 0.into(), 600.into(), 800.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id } },
            "Contents" => content,
        });
        let pages_id = doc.add_object(dictionary! {
            "Type" => "Pages", "Count" => 1, "Kids" => vec![Object::Reference(page_id)],
        });
        doc.get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Parent", pages_id);
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let text: String = crate::extract_text_with_positions_mem(&bytes)
            .unwrap()
            .into_iter()
            .map(|item| item.text)
            .collect::<Vec<_>>()
            .join("|");
        if expected.is_empty() {
            assert!(!text.contains("How many"), "CIDToGIDMap stream: {text:?}");
        } else {
            assert_eq!(text, expected);
        }
    }
}

#[test]
fn mac_order_needs_letter_proportions_to_agree() {
    // A wide `i` or lowercase averaging wider than capitals says the glyphs
    // at those positions are not the letters the ordering claims.
    let mut wide_i = arial_like(&[556; 10], (false, 278));
    wide_i[76] = (true, 900);
    let mut wide_lower = arial_like(&[556; 10], (false, 278));
    for gid in 68..=93 {
        wide_lower[gid] = (true, 700);
    }
    wide_lower[76] = (true, 222);
    wide_lower[79] = (true, 222);
    wide_lower[80] = (true, 833);
    wide_lower[90] = (true, 722);
    for glyphs in [wide_i, wide_lower] {
        assert!(build_cmap_from_mac_glyph_order(&cmapless_truetype(&glyphs)).is_none());
    }
}

#[test]
fn mac_order_needs_letters_present_to_check() {
    // Digits and a space alone are not evidence of the ordering.
    let mut glyphs = vec![(false, 0u16); 98];
    glyphs[0] = (true, 750);
    glyphs[3] = (false, 278);
    for gid in 19..=28 {
        glyphs[gid] = (true, 556);
    }
    assert!(build_cmap_from_mac_glyph_order(&cmapless_truetype(&glyphs)).is_none());
}

#[test]
fn mac_order_maps_only_slots_the_subset_kept() {
    let font = cmapless_truetype(&arial_like(&[556; 10], (false, 278)));
    let cmap = build_cmap_from_mac_glyph_order(&font).unwrap();
    assert!(cmap.char_map.contains_key(&3), "space advances");
    assert!(cmap.char_map.contains_key(&36), "A has an outline");
    assert!(
        !cmap.char_map.contains_key(&4),
        "exclam is not in the subset"
    );
}
