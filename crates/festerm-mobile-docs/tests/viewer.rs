use festerm_markdown::{
    Block, LocalMarkdownSource, MarkdownCancellation, MarkdownLoadError, MarkdownSource,
    RemoteMarkdownSource, RemoteSourceOwner,
};
use festerm_mobile_docs::{
    read_capped, BlockReason, BudgetError, DocumentViewer, ImageAction, ImageBudget, LinkAction,
    PathRejection, ViewerError, ViewerOptions, MAX_IMAGES, MOBILE_MAX_SOURCE_BYTES,
};
use std::io::Read;

const README: &str = include_str!("fixtures/README.md");

fn local(path: &str, text: &str) -> Result<DocumentViewer, ViewerError> {
    DocumentViewer::open(
        MarkdownSource::Local(LocalMarkdownSource::new(path).unwrap()),
        Some(text.len() as u64),
        text.as_bytes(),
        ViewerOptions {
            root: Some("/sandbox".into()),
        },
        &MarkdownCancellation::new(),
    )
}

fn readme() -> DocumentViewer {
    local("/sandbox/proj/README.md", README).unwrap()
}

#[test]
fn headless_render_of_sample_readme() {
    let text = readme().plain_text();
    let expected = [
        "# Nimbus Runbook",
        "**sample**",
        "section link <#deploy-steps>",
        "external site <https://example.com/status>",
        "## Deploy steps",
        "- [x] Build the image",
        "- [ ] Roll out to staging",
        "| Service | Port | Notes |",
        "| :-- | --: | :-: |",
        "| api | 8080 | main |",
        "> Remember: never deploy on Fridays.",
        "```rust\nfn main() {",
        "[image: diagram]",
        "---",
        "### Alerts",
    ];
    for line in expected {
        assert!(text.contains(line), "missing {line:?} in:\n{text}");
    }
    assert!(!text.contains("onclick"), "raw html must stay inert");
}

#[test]
fn model_has_tables_task_lists_highlighted_code_and_outline() {
    let viewer = readme();
    let doc = viewer.document();
    assert!(doc.blocks().iter().any(|b| matches!(b, Block::Table(_))));
    let code = doc
        .blocks()
        .iter()
        .find_map(|b| match b {
            Block::CodeBlock(c) => Some(c),
            _ => None,
        })
        .unwrap();
    assert_eq!(code.language(), Some("rust"));
    assert!(code
        .highlighted_lines()
        .iter()
        .any(|l| !l.spans().is_empty()));
    let outline: Vec<_> = viewer
        .outline()
        .into_iter()
        .map(|e| (e.level, e.anchor))
        .collect();
    assert_eq!(
        outline,
        [
            (1, "nimbus-runbook".into()),
            (2, "deploy-steps".into()),
            (3, "alerts".into())
        ]
    );
}

#[test]
fn anchors_scroll_to_headings_and_named_anchors() {
    let v = readme();
    match v.resolve_link("#deploy-steps") {
        LinkAction::ScrollTo { byte_offset, .. } => {
            assert!(v.document().source_text()[byte_offset..].starts_with("## Deploy steps"))
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        v.resolve_link("#custom-anchor"),
        LinkAction::ScrollTo { .. }
    ));
    assert!(matches!(
        v.resolve_link("#Deploy-Steps"),
        LinkAction::ScrollTo { .. }
    ));
    assert_eq!(
        v.resolve_link("#nope"),
        LinkAction::UnknownAnchor {
            anchor: "nope".into()
        }
    );
}

#[test]
fn link_policy_is_same_source_and_never_fetches() {
    let v = readme();
    assert_eq!(
        v.resolve_link("docs/ops.md#alerts"),
        LinkAction::OpenDocument {
            path: "/sandbox/proj/docs/ops.md".into(),
            anchor: Some("alerts".into())
        }
    );
    assert_eq!(
        v.resolve_link("../notes/a%20b.txt"),
        LinkAction::OpenFile {
            path: "/sandbox/notes/a b.txt".into()
        }
    );
    assert_eq!(
        v.resolve_link("https://example.com/status"),
        LinkAction::ExternalHttps {
            url: "https://example.com/status".into()
        }
    );
    for bad in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "http://example.com",
        "/etc/passwd",
        "\\\\host\\share",
    ] {
        assert_eq!(
            v.resolve_link(bad),
            LinkAction::Blocked(BlockReason::DangerousScheme),
            "{bad}"
        );
    }
    for bad in [
        "../../etc/passwd",
        "../../../x",
        "a%2f..%2fb",
        "%2e%2e/x",
        "a%zz",
        "a\\b",
    ] {
        assert!(
            matches!(
                v.resolve_link(bad),
                LinkAction::Blocked(BlockReason::Path(_))
            ),
            "{bad}"
        );
    }
    assert_eq!(
        v.resolve_link("../../etc/passwd"),
        LinkAction::Blocked(BlockReason::Path(PathRejection::EscapesRoot))
    );
}

#[test]
fn image_policy_is_same_source_allowlisted_and_budgeted() {
    let v = readme();
    assert_eq!(
        v.resolve_image("img/arch.png"),
        ImageAction::Load {
            path: "/sandbox/proj/img/arch.png".into(),
            max_bytes: 2 * 1024 * 1024
        }
    );
    assert_eq!(
        v.resolve_image("https://example.com/x.png"),
        ImageAction::RemoteBlocked
    );
    assert!(matches!(
        v.resolve_image("../../../etc/passwd.png"),
        ImageAction::Blocked(BlockReason::Path(PathRejection::EscapesRoot))
    ));
    assert_eq!(
        v.resolve_image("img/logo.svg"),
        ImageAction::Blocked(BlockReason::UnsupportedImageType)
    );
    assert_eq!(
        v.resolve_image("data:image/png;base64,AAAA"),
        ImageAction::Blocked(BlockReason::DangerousScheme)
    );

    let mut b = ImageBudget::default();
    assert_eq!(b.admit(3 * 1024 * 1024), Err(BudgetError::ImageTooLarge));
    for _ in 0..4 {
        b.admit(2 * 1024 * 1024).unwrap();
    }
    assert_eq!(b.admit(1), Err(BudgetError::TotalTooLarge));
    let mut b = ImageBudget::default();
    for _ in 0..MAX_IMAGES {
        b.admit(1).unwrap();
    }
    assert_eq!(b.admit(1), Err(BudgetError::TooManyImages));
}

#[test]
fn remote_sources_resolve_inside_the_sftp_origin() {
    let remote = RemoteMarkdownSource::new(
        "Host.example",
        22,
        RemoteSourceOwner::username("me").unwrap(),
        "SHA256:abc",
        "/home/me/app/README.md",
        1,
    )
    .unwrap();
    let v = DocumentViewer::open(
        MarkdownSource::Remote(remote),
        None,
        "# T\n![i](a/b.png)\n".as_bytes(),
        ViewerOptions {
            root: Some("/home/me".into()),
        },
        &MarkdownCancellation::new(),
    )
    .unwrap();
    assert_eq!(
        v.resolve_image("a/b.png"),
        ImageAction::Load {
            path: "/home/me/app/a/b.png".into(),
            max_bytes: 2 * 1024 * 1024
        }
    );
    assert!(matches!(
        v.resolve_image("../../root/x.png"),
        ImageAction::Blocked(_)
    ));
}

struct Endless;
impl Read for Endless {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        buf.fill(b'a');
        Ok(buf.len())
    }
}

#[test]
fn size_cap_is_enforced_for_declared_and_streamed_sizes() {
    assert_eq!(
        read_capped(Endless, None, 1000),
        Err(ViewerError::TooLarge { limit: 1000 })
    );
    assert_eq!(
        read_capped(std::io::empty(), Some(2000), 1000),
        Err(ViewerError::TooLarge { limit: 1000 })
    );
    assert_eq!(read_capped(&b"abc"[..], Some(3), 3).unwrap(), b"abc");
    let err = DocumentViewer::open(
        MarkdownSource::Local(LocalMarkdownSource::new("/a.md").unwrap()),
        None,
        Endless,
        ViewerOptions::default(),
        &MarkdownCancellation::new(),
    );
    assert!(
        matches!(err, Err(ViewerError::TooLarge { limit }) if limit == MOBILE_MAX_SOURCE_BYTES)
    );
}

#[test]
fn binary_and_invalid_utf8_are_rejected_without_content() {
    for bytes in [&b"\xff\xfe\xfa"[..], &[0u8; 64][..]] {
        let r = DocumentViewer::open(
            MarkdownSource::Local(LocalMarkdownSource::new("/a.md").unwrap()),
            None,
            bytes,
            ViewerOptions::default(),
            &MarkdownCancellation::new(),
        );
        assert!(matches!(
            r,
            Err(ViewerError::Markdown(
                MarkdownLoadError::InvalidUtf8 | MarkdownLoadError::BinaryContent
            ))
        ));
    }
}
