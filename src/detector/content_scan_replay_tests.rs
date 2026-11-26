//! Tests of what a document's scans keep of its forms from page to page
//! (see `FormScans`): a form the pages share is walked once, and run once
//! under each state it is drawn in, and the pages read as they read alone.

use super::super::{
    analyze_page_content_with, detect_from_document, DetectionConfig, PageAnalysis,
};
use super::fixtures::*;
use super::*;
use lopdf::dictionary;

/// Scans that keep no form runs and walk every form bound afresh: each
/// page read as it was before anything was kept from page to page.
fn kept_none<'a>() -> FormScans<'a> {
    FormScans {
        runs: FormRuns::none(),
        walks: HashMap::new(),
    }
}

/// A document of `pages` 612×792 pages that each run `/Fm0 Do`: one form
/// running `form`, with the font `F1` and — when `form_binds_image` — the
/// 2×2 image `Im0` in its own resources. Each page has an inline resource
/// dictionary of its own, binding `F1`, `Fm0` and — on the pages
/// `page_binds_image` picks, by their index from 0 — `Im0`.
fn pages_sharing_a_form(
    pages: usize,
    form: &str,
    form_binds_image: bool,
    page_binds_image: impl Fn(usize) -> bool,
) -> (Document, Vec<ObjectId>) {
    let mut doc = Document::with_version("1.4");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => Object::Name(b"Type1".to_vec()),
        "BaseFont" => Object::Name(b"Helvetica".to_vec()),
    });
    let image_id = doc.add_object(Object::Stream(lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => Object::Name(b"Image".to_vec()),
            "Width" => Object::Integer(2),
            "Height" => Object::Integer(2),
            "ColorSpace" => Object::Name(b"DeviceGray".to_vec()),
            "BitsPerComponent" => Object::Integer(8),
        },
        vec![200, 60, 60, 200],
    )));
    let mut form_resources = dictionary! {
        "Font" => dictionary! { "F1" => Object::Reference(font_id) },
    };
    if form_binds_image {
        form_resources.set(
            "XObject",
            dictionary! { "Im0" => Object::Reference(image_id) },
        );
    }
    let form_id = doc.add_object(Object::Stream(lopdf::Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => Object::Name(b"Form".to_vec()),
            "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => form_resources,
        },
        form.as_bytes().to_vec(),
    )));
    let mut kids = Vec::new();
    for page in 0..pages {
        let mut xobjects = dictionary! { "Fm0" => Object::Reference(form_id) };
        if page_binds_image(page) {
            xobjects.set("Im0", Object::Reference(image_id));
        }
        let content_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            dictionary! {},
            b"/Fm0 Do\n".to_vec(),
        )));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(font_id) },
                "XObject" => xobjects,
            },
            "Contents" => Object::Reference(content_id),
        });
        kids.push(page_id);
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids.iter().map(|&id| Object::Reference(id)).collect::<Vec<_>>(),
            "Count" => Object::Integer(pages as i64),
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    (doc, kids)
}

/// A form every page draws under the same state is run on the first page
/// and replayed on the others, and walked once for all of them; each page
/// reads as it reads alone.
#[test]
fn a_form_the_pages_share_is_run_and_walked_once_for_all_of_them() {
    let layer = glyph_layer(0);
    let (doc, pages) = pages_sharing_a_form(5, &layer, false, |_| false);
    let mut form_scans = FormScans::default();
    for &page_id in &pages {
        let shared = analyze_page_content_with(&doc, page_id, &mut form_scans);
        let alone = analyze_page_content_with(&doc, page_id, &mut kept_none());
        assert_eq!(shared, alone);
        assert_eq!(shared.executed_text_operator_count, 120);
    }
    assert_eq!(form_scans.runs.kept, 1);
    assert_eq!(form_scans.runs.replays, 4);
    assert_eq!(form_scans.walks.len(), 1);
}

/// A form that draws a covering image under a text layer nobody sees is
/// a scan on every page that draws it, replayed or run — through the
/// detector's sampled pages and the pages it reads for their OCR reason
/// alone (#566).
#[test]
fn a_shared_form_s_invisible_text_layer_is_flagged_on_every_page() {
    let form = format!("{FULL_PAGE_IMAGE}{}", glyph_layer(3));
    let (doc, pages) = pages_sharing_a_form(3, &form, true, |_| false);
    let mut form_scans = FormScans::default();
    for &page_id in &pages {
        let executed = page_executed_content(&doc, page_id, &mut form_scans);
        assert!(executed.shows_only_a_hidden_text_layer);
    }
    assert_eq!(form_scans.runs.replays, 2);

    let detected = detect_from_document(&doc, 3, &DetectionConfig::default()).unwrap();
    assert_eq!(detected.pages_needing_ocr, vec![1, 2, 3]);
    for page in 1..=3 {
        assert_eq!(
            detected.ocr_reasons_by_page.get(&page),
            Some(&vec![crate::OCR_REASON_INVISIBLE_TEXT_LAYER.to_string()]),
            "page {page}"
        );
    }
}

/// A form that draws an image its own resources leave to the page's is
/// replayed only on a page whose resources bind the same: on another the
/// name may stand for another image, or for nothing.
#[test]
fn a_run_that_drew_what_the_page_binds_is_replayed_only_where_the_page_binds_the_same() {
    let form = format!("{FULL_PAGE_IMAGE}{}", glyph_layer(3));
    // The second page binds no image.
    let (doc, pages) = pages_sharing_a_form(3, &form, false, |page| page != 1);
    let mut form_scans = FormScans::default();
    let flagged: Vec<bool> = pages
        .iter()
        .map(|&page_id| {
            page_executed_content(&doc, page_id, &mut form_scans).shows_only_a_hidden_text_layer
        })
        .collect();
    assert_eq!(flagged, [true, false, true]);
    // The third page binds what the first does.
    assert_eq!(form_scans.runs.replays, 1);
    assert_eq!(form_scans.runs.kept, 2);
}

/// A form whose own resources bind every name it draws is replayed
/// whatever the page binds.
#[test]
fn a_run_that_drew_only_what_the_form_binds_is_replayed_whatever_the_page_binds() {
    let form = format!("{FULL_PAGE_IMAGE}{}", glyph_layer(3));
    // The second page binds an image of its own as `Im0` too, which the
    // form's own binding shadows.
    let (doc, pages) = pages_sharing_a_form(3, &form, true, |page| page == 1);
    let mut form_scans = FormScans::default();
    for &page_id in &pages {
        assert!(
            page_executed_content(&doc, page_id, &mut form_scans).shows_only_a_hidden_text_layer
        );
    }
    assert_eq!(form_scans.runs.replays, 2);
    assert_eq!(form_scans.runs.kept, 1);
}

/// The same form drawn under another state — here, at half size, where
/// its image no longer covers the page — runs again.
#[test]
fn a_form_drawn_under_another_state_runs_again() {
    let form = format!("{FULL_PAGE_IMAGE}{}", glyph_layer(3));
    let (mut doc, pages) = pages_sharing_a_form(2, &form, true, |_| false);
    let content_id = doc.get_page_contents(pages[1])[0];
    set_page_content(&mut doc, content_id, "q 0.5 0 0 0.5 0 0 cm /Fm0 Do Q\n");
    let mut form_scans = FormScans::default();
    let flagged: Vec<bool> = pages
        .iter()
        .map(|&page_id| {
            page_executed_content(&doc, page_id, &mut form_scans).shows_only_a_hidden_text_layer
        })
        .collect();
    assert_eq!(flagged, [true, false]);
    assert_eq!(form_scans.runs.replays, 0);
    assert_eq!(form_scans.runs.kept, 2);
}

/// A run that looked up a pattern is not kept: the page keeps the
/// pattern's verdict once tested, which a later run would not test again.
#[test]
fn a_run_that_looked_up_a_pattern_is_not_kept() {
    let form = "/Pattern cs /P0 scn 0 0 612 792 re f\n";
    let (doc, pages) = pages_sharing_a_form(2, form, false, |_| false);
    let mut form_scans = FormScans::default();
    for &page_id in &pages {
        page_executed_content(&doc, page_id, &mut form_scans);
    }
    assert_eq!(form_scans.runs.kept, 0);
    assert_eq!(form_scans.runs.replays, 0);
}

/// Every page of every fixture reads the same whether its forms are read
/// afresh or through what the document's earlier pages kept.
#[test]
fn every_fixture_page_reads_the_same_through_the_forms_kept() {
    let mut fixtures: Vec<_> = std::fs::read_dir("tests/fixtures")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pdf"))
        .collect();
    fixtures.sort();
    assert!(!fixtures.is_empty());
    for path in fixtures {
        let Ok((doc, _)) = crate::load_document_from_mem(&std::fs::read(&path).unwrap()) else {
            continue;
        };
        let mut form_scans = FormScans::default();
        for (page, page_id) in doc.get_pages() {
            let shared: PageAnalysis = analyze_page_content_with(&doc, page_id, &mut form_scans);
            let alone = analyze_page_content_with(&doc, page_id, &mut kept_none());
            assert_eq!(shared, alone, "{} page {page}", path.display());
        }
    }
}
