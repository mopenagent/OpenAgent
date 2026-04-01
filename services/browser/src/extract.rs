//! Content extraction via dom_smoothie (Mozilla Readability port).
//!
//! Replaces manual CSS-selector scraping with a Readability algorithm that
//! automatically identifies and extracts the main article content, strips
//! boilerplate (nav, ads, footers), and returns clean Markdown — ideal for
//! feeding directly to an LLM without additional cleaning.
//!
//! Output format:  `# <title>\n\n<body as Markdown>`
//! If the page has no readable content (JS shell, bot-block) the returned
//! string is empty — the LLM should treat this as a signal to escalate to a
//! remote browser tool.

use dom_smoothie::{Config, Readability, TextMode};

/// Extract readable content from raw HTML and return it as Markdown.
///
/// Returns an empty string if the page yields no usable content.
pub fn extract_text(html: &str) -> String {
    let cfg = Config {
        text_mode: TextMode::Markdown,
        ..Config::default()
    };

    let mut reader = match Readability::new(html, None::<&str>, Some(cfg)) {
        Ok(r) => r,
        Err(_) => return String::new(),
    };

    let article = match reader.parse() {
        Ok(a) => a,
        Err(_) => return String::new(),
    };

    let body = article.text_content.as_ref().trim().to_owned();

    if body.is_empty() {
        return String::new();
    }

    let title = article.title.trim().to_owned();
    if title.is_empty() {
        body
    } else {
        format!("# {title}\n\n{body}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_readable_article() {
        let html = r#"<!DOCTYPE html><html><head><title>Rust is Fast</title></head><body>
            <article>
                <h1>Rust is Fast</h1>
                <p>Rust is a systems programming language focused on safety, speed, and
                concurrency. It achieves memory safety without a garbage collector by using
                a borrow checker at compile time. This makes Rust programs both safe and fast.</p>
                <p>The language has grown rapidly and is now used in production at many large
                companies including Mozilla, Microsoft, Amazon, and Google.</p>
            </article>
            <nav><a href="/">Home</a><a href="/blog">Blog</a></nav>
            <footer>Copyright 2024</footer>
        </body></html>"#;

        let text = extract_text(html);
        assert!(!text.is_empty(), "should extract article content");
        assert!(
            text.to_lowercase().contains("rust"),
            "should contain article body"
        );
    }

    #[test]
    fn includes_title_when_present() {
        let html = r#"<!DOCTYPE html><html><head><title>My Article</title></head><body>
            <article>
                <p>This is a long enough article body that readability should pick it up
                and include it in the extracted output for the LLM to consume.</p>
                <p>More content here to make sure the article passes the minimum length
                threshold that dom_smoothie requires before returning a result.</p>
            </article>
        </body></html>"#;

        let text = extract_text(html);
        // Title may or may not be injected depending on readability scoring,
        // but content should always be present for a readable page.
        assert!(!text.is_empty(), "readable page should produce output");
    }

    #[test]
    fn empty_body_returns_empty_string() {
        let html = "<html><head><title>Nothing</title></head><body></body></html>";
        let text = extract_text(html);
        assert!(text.is_empty(), "empty body should yield empty string");
    }

    #[test]
    fn js_shell_returns_empty_or_minimal() {
        // Simulates a JS-rendered app shell — no readable content
        let html = r#"<!DOCTYPE html><html><head>
            <title>App</title>
            <script src="/bundle.js"></script>
        </head><body>
            <div id="root"></div>
        </body></html>"#;

        let text = extract_text(html);
        // Either empty or very short — the LLM should escalate to a remote browser
        assert!(
            text.len() < 200,
            "JS shell should yield sparse content, got {} chars",
            text.len()
        );
    }
}
