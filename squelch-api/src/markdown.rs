//! Markdown → email-safe HTML for the send path. The composer sends the RAW
//! markdown source as the message body (`body_format: "markdown"`); this module
//! renders the HTML alternative that rides beside it in multipart/alternative.
//!
//! The outbound secret guard scans the SOURCE, and this render is the only
//! place the HTML part comes from — so the guard's verdict covers everything
//! that leaves. Nothing here may take HTML from the caller.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};

/// Schemes a link may keep in outbound mail. Anything else (`javascript:`,
/// `data:`, scheme-relative) renders as its text content, unlinked.
fn safe_link(dest: &str) -> bool {
    let lower = dest.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

/// The token of a `cid:` image destination this renderer will let through, or
/// `None` for anything else. The alphabet is the one the upload door enforces
/// on every stored `content_id` (`handlers::content_id_ok`), so a typed
/// `![x](cid:"><script>)` is not a cid at all and renders as alt text like
/// every other image. Nothing about the reference reaches the recipient's
/// network: `cid:` names a part of this same message.
fn cid_token(dest: &str) -> Option<&str> {
    let token = dest.trim().strip_prefix("cid:")?;
    crate::handlers::content_id_ok(token).then_some(token)
}

/// Whether the heading spanning `src` exists only because a bare `--` sits
/// directly under a line of text. That is the signature delimiter, and
/// CommonMark reads it as a setext underline. Exactly two dashes: `---` under
/// text stays the heading stock markdown says it is. Inside a blockquote the
/// underline's line still carries its `>` markers, so those come off first.
fn underlined_by_signature_dashes(src: &str) -> bool {
    src.trim_end().rsplit('\n').next().is_some_and(|last| {
        last.trim_start_matches(|c: char| c == '>' || c.is_whitespace())
            .trim_end()
            == "--"
    })
}

/// Render markdown to the HTML body of an email.
///
/// Deviations from stock CommonMark, all deliberate:
/// * Raw HTML NEVER passes through — a typed `<script>` renders as the literal
///   characters. The composer's contract is markdown in, not HTML in.
/// * Soft breaks become hard breaks: people write emails with meaningful line
///   endings, and CommonMark folding "hi\nbob" into one line would mangle them.
/// * A bare `--` directly under text is the signature delimiter, not a heading
///   underline: the text above stays a paragraph and the dashes stay dashes.
///   Every composer is seeded with a signature that opens with one, and one
///   line typed against it would otherwise reach the recipient as an `<h2>`
///   with the dashes gone, which the editor gives no hint of.
/// * Images render as their alt text — EXCEPT a `cid:` image, which is how the
///   composer places an attached picture in the body (`![name](cid:token)`)
///   and renders as an `<img>` pointing at the sibling part. Remote images
///   stay refused; outbound mail has no business embedding resources the
///   recipient auto-fetches.
pub fn render_email_html(source: &str) -> String {
    let parser = Parser::new_ext(source, Options::ENABLE_STRIKETHROUGH).into_offset_iter();

    // Depth of suppressed link/image tags, so the matching End is swallowed
    // with its Start while the text between them still flows through.
    let mut suppressed: Vec<&'static str> = Vec::new();
    // The cid image being collected: its token, and the alt text between its
    // Start and End, which becomes the `alt=` rather than flowing through.
    let mut inline: Option<(String, String)> = None;
    // Inside a heading that is being rendered as a paragraph plus its dashes.
    let mut signature_dashes = false;
    let mut events: Vec<Event> = Vec::new();
    for (ev, range) in parser {
        match ev {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H2,
                ..
            }) if underlined_by_signature_dashes(&source[range]) => {
                signature_dashes = true;
                events.push(Event::Start(Tag::Paragraph));
            }
            Event::End(TagEnd::Heading(_)) if signature_dashes => {
                signature_dashes = false;
                events.push(Event::End(TagEnd::Paragraph));
                events.push(Event::Start(Tag::Paragraph));
                events.push(Event::Text("--".into()));
                events.push(Event::End(TagEnd::Paragraph));
            }
            Event::Start(Tag::Image { ref dest_url, .. })
                if inline.is_none() && cid_token(dest_url).is_some() =>
            {
                let token = cid_token(dest_url).unwrap_or_default().to_string();
                inline = Some((token, String::new()));
            }
            Event::End(TagEnd::Image) if inline.is_some() => {
                let (token, alt) = inline.take().unwrap_or_default();
                // The token was validated against a header-safe alphabet and
                // the alt is escaped; nothing else is interpolated.
                events.push(Event::Html(
                    format!(
                        "<img src=\"cid:{token}\" alt=\"{}\" style=\"max-width:100%;height:auto\">",
                        crate::gmail_write::escape_html(&alt)
                    )
                    .into(),
                ));
            }
            Event::Text(t) if inline.is_some() => {
                if let Some((_, alt)) = inline.as_mut() {
                    alt.push_str(&t);
                }
            }
            // Raw HTML re-enters as Text, which `push_html` escapes.
            Event::Html(s) | Event::InlineHtml(s) => events.push(Event::Text(s)),
            Event::SoftBreak => events.push(Event::HardBreak),
            Event::Start(Tag::Link { ref dest_url, .. }) if !safe_link(dest_url) => {
                suppressed.push("link");
            }
            Event::End(TagEnd::Link) if suppressed.last() == Some(&"link") => {
                suppressed.pop();
            }
            Event::Start(Tag::Image { .. }) => suppressed.push("image"),
            Event::End(TagEnd::Image) if suppressed.last() == Some(&"image") => {
                suppressed.pop();
            }
            ev => events.push(ev),
        }
    }

    let mut body = String::with_capacity(source.len() * 2);
    html::push_html(&mut body, events.into_iter());

    // Inline styles only — mail clients strip <style> blocks. One wrapper div
    // keeps the mail readable without dictating much.
    format!(
        "<div style=\"font-family:-apple-system,'Segoe UI',Helvetica,Arial,sans-serif;\
         font-size:14px;line-height:1.45\">\n{body}</div>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_formatting_renders() {
        let html = render_email_html("**bold** and *em* and `code`");
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("<em>em</em>"));
        assert!(html.contains("<code>code</code>"));
    }

    #[test]
    fn raw_html_is_escaped_not_passed_through() {
        let html = render_email_html("hi <script>alert(1)</script> there");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn block_html_is_escaped_too() {
        let html = render_email_html("<div onclick=\"x()\">block</div>");
        assert!(!html.contains("<div onclick"));
        assert!(html.contains("&lt;div onclick"));
    }

    #[test]
    fn javascript_links_are_unlinked_text_survives() {
        let html = render_email_html("[click](javascript:alert(1))");
        assert!(!html.contains("javascript:"));
        assert!(!html.contains("<a "));
        assert!(html.contains("click"));
    }

    #[test]
    fn http_links_keep_their_href() {
        let html = render_email_html("[site](https://example.com)");
        assert!(html.contains("<a href=\"https://example.com\">site</a>"));
    }

    #[test]
    fn soft_breaks_become_br() {
        let html = render_email_html("hi\nbob");
        assert!(html.contains("hi<br />\nbob") || html.contains("hi<br>\nbob"));
    }

    #[test]
    fn a_cid_image_renders_as_an_img_pointing_at_the_part() {
        let html = render_email_html("look ![the shot](cid:abc-123@passband) here");
        assert!(
            html.contains(
                "look <img src=\"cid:abc-123@passband\" alt=\"the shot\" style=\"max-width:100%;height:auto\"> here"
            ),
            "{html}"
        );
    }

    #[test]
    fn a_cid_image_alt_is_escaped_and_the_token_is_policed() {
        // The alt text is the author's; it cannot close the attribute.
        let html = render_email_html("![a\" onerror=\"x](cid:tok)");
        assert!(!html.contains("onerror=\"x"), "{html}");
        assert!(html.contains("alt=\"a&quot; onerror=&quot;x\""), "{html}");
        // A token outside the header-safe alphabet is not a cid at all: the
        // image falls back to alt text, like a remote one.
        for bad in [
            "cid:tok\"><script>",
            "cid:",
            "cid:a b",
            "CID:tok",
            "cid:tok/../x",
        ] {
            let html = render_email_html(&format!("![pic]({bad})"));
            assert!(!html.contains("<img"), "{bad} => {html}");
            assert!(html.contains("pic"), "{bad} => {html}");
        }
    }

    #[test]
    fn images_render_as_alt_text() {
        let html = render_email_html("![a chart](https://example.com/x.png)");
        assert!(!html.contains("<img"));
        assert!(html.contains("a chart"));
    }

    /// The client's default signature (`Prefs.defaultSignature`), seeded two
    /// newlines under whatever was typed. The dashes must stay two literal
    /// dashes in a paragraph of their own: not a heading underline for the
    /// line above, and not a typographic dash.
    #[test]
    fn the_default_signature_renders_as_dashes_then_a_link() {
        let html = render_email_html(
            "Thanks,\nBraelyn\n\n--\n\nSent from [Passband](https://passband.app)",
        );
        assert!(!html.contains("<h2>"), "{html}");
        assert!(html.contains("<p>--</p>"), "{html}");
        assert!(
            html.contains("<p>Sent from <a href=\"https://passband.app\">Passband</a></p>"),
            "{html}"
        );
    }

    /// What typing on the line right above the seeded signature produces. The
    /// editor shows nothing wrong, so the render is the only place to hold it.
    #[test]
    fn signature_dashes_under_text_are_not_a_heading_underline() {
        for dashes in ["--", "-- "] {
            let html = render_email_html(&format!(
                "Thanks,\nBraelyn\n{dashes}\n\nSent from [Passband](https://passband.app)"
            ));
            assert!(!html.contains("<h2>"), "{dashes:?} => {html}");
            assert!(
                html.contains("<p>Thanks,<br />\nBraelyn</p>\n<p>--</p>"),
                "{dashes:?} => {html}"
            );
            assert!(
                html.contains("<p>Sent from <a href="),
                "{dashes:?} => {html}"
            );
        }
        // Quoted, the underline keeps its `>`; the rescue still sees dashes.
        let html = render_email_html("> Thanks\n> --");
        assert!(
            html.contains("<blockquote>\n<p>Thanks</p>\n<p>--</p>\n</blockquote>"),
            "{html}"
        );
        // Formatting in the rescued paragraph survives the rewrite.
        let html = render_email_html("**Thanks** [x](https://example.com)\n--");
        assert!(
            html.contains(
                "<p><strong>Thanks</strong> <a href=\"https://example.com\">x</a></p>\n<p>--</p>"
            ),
            "{html}"
        );
    }

    /// The rescue is for the two-dash delimiter alone. Deliberate setext
    /// headings, and two dashes that are not an underline, are untouched.
    #[test]
    fn real_setext_headings_still_render() {
        assert!(render_email_html("Title\n---").contains("<h2>Title</h2>"));
        assert!(render_email_html("Title\n==").contains("<h1>Title</h1>"));
        assert!(render_email_html("## Title").contains("<h2>Title</h2>"));
        let html = render_email_html("```\na\n--\n```");
        assert!(html.contains("a\n--\n</code>"), "{html}");
    }

    #[test]
    fn headings_and_lists_render() {
        let html = render_email_html("# Title\n\n- one\n- two");
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<li>one</li>"));
    }
}
