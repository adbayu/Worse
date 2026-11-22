use super::*;

const SAMPLE: &str = "---\ntitle: Proposal\npaper: letter\nnumbering: true\n---\n# Intro {#intro}\n\nNorthwind wants **Q4**.[^1]\n\n[^1]: Signed in May.\n\n- one\n- two\n  - nested\n\n| Item | Qty |\n|---|---|\n| Licences | 40 |\n\n![Cost chart](assets/c.png)\n{#fig-cost}\n\n> [!NOTE]\n> Budget approved.\n\n<!-- pagebreak -->\n\n```chart\n{\"type\":\"bar\",\"categories\":[\"A\"],\"series\":[{\"name\":\"x\",\"values\":[1]}]}\n```\n";

#[test]
fn frontmatter_and_blocks() {
    let doc = Doc::parse(SAMPLE);
    assert_eq!(doc.meta.title, "Proposal");
    assert_eq!(doc.meta.paper, "letter");
    assert!(doc.meta.numbering);
    let blocks = doc.blocks();
    assert!(matches!(&blocks[0], Block::Heading { level: 1, id, .. } if id == "intro"));
    let Block::Para(p) = &blocks[1] else {
        panic!("para")
    };
    assert!(p
        .iter()
        .any(|i| matches!(i, Inline::Note(n) if inline_plain(n) == "Signed in May.")));
    assert!(matches!(&blocks[2], Block::List { items, .. } if items.len() == 3 && items[2].0 == 1));
    assert!(matches!(&blocks[3], Block::Table { rows, .. } if rows.len() == 1));
    assert!(
        matches!(&blocks[4], Block::Image { id, caption, .. } if id == "fig-cost" && caption == "Cost chart")
    );
    assert!(matches!(&blocks[5], Block::Callout { kind, .. } if kind == "note"));
    assert!(matches!(blocks[6], Block::PageBreak));
    assert!(matches!(blocks[7], Block::Chart(_)));
}

#[test]
fn title_falls_back_to_first_heading() {
    assert_eq!(Doc::parse("# Hello world\n\ntext").title(), "Hello world");
    assert_eq!(heading_id("Pricing & Terms"), "pricing-terms");
}
