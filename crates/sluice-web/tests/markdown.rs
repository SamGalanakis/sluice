use sluice_web::markdown::{allowed_url, render};
#[test]
fn headings_tables_code_lists_and_html_have_safe_structure() {
    let html = render(
        "## A\n#### B\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n<script>alert(1)</script>\n\n**text** <b>raw</b>\n\n- one\n  - two\n\n```rust\n<thing>\n```",
    );
    let html = html.as_str();
    assert!(html.contains("<h4>A</h4>"));
    assert!(html.contains("<h6>B</h6>"));
    assert!(html.contains("<table>"));
    assert!(html.contains("<strong>text</strong>"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(html.contains("&lt;b&gt;raw&lt;/b&gt;"));
    assert!(html.matches("<ul>").count() == 2);
    assert!(html.contains("language-rust"));
    assert!(html.contains("&lt;thing&gt;"));
}
#[test]
fn unsafe_destinations_and_encoded_spellings_are_removed_for_links_and_images() {
    for destination in [
        "javascript:alert",
        "JaVaScRiPt:alert",
        "jav&#x61;script:alert",
        "javascript&colon;alert",
        "java%73cript:alert",
        "%6a%61vascript%3aalert",
        "%256aavascript%253aalert",
        "java&#9;script:alert",
        "java%0ascript:alert",
        "vbscript:run",
        "file:///etc/passwd",
        "data:image/png;base64,abc",
        "data:text/html,x",
        "ftp://host/x",
        "//other.example/x",
        "%2f%2fother.example/x",
    ] {
        let html = render(&format!("[bad]({destination})\n\n![bad]({destination})"));
        assert!(html.as_str().contains("href=\"\""), "{destination}: {html}");
        assert!(html.as_str().contains("src=\"\""), "{destination}: {html}");
    }
}
#[test]
fn allowlist_accepts_only_relative_fragment_and_explicit_allowed_schemes() {
    for url in [
        "/projects/id/1",
        "/percent%25literal",
        "%é/x",
        "../readme",
        "docs/file",
        "#section",
        "?tab=one",
        "http://example.org",
        "HTTPS://example.org",
        "mailto:sam@example.org",
    ] {
        assert!(allowed_url(url), "{url}");
    }
    for url in [
        "java\nscript:alert",
        "java\tscript:alert",
        "\u{0}https://host",
        "\\\\host/x",
        "%5c%5chost",
        "%00safe",
    ] {
        assert!(!allowed_url(url), "{url}");
    }
}
