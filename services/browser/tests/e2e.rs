//! End-to-end tests against real services.
//!
//! All tests are `#[ignore]` by default. Run with:
//!   cargo test --test e2e -- --ignored
//!
//! Requirements:
//!   - SearXNG running at http://100.96.81.109:8888
//!   - Outbound internet access for fetch tests

use browser_lib::{extract, fetch, search};

const SEARXNG: &str = "http://100.96.81.109:8888";

// ── SearXNG ───────────────────────────────────────────────────────────────────

#[test]
#[ignore = "requires SearXNG at 100.96.81.109:8888"]
fn e2e_searxng_returns_results_for_known_query() {
    let results = search::search(SEARXNG, "rust programming language", 3).unwrap();
    assert!(!results.is_empty(), "SearXNG returned no results");
    for r in &results {
        assert!(!r.url.is_empty(), "result URL must not be empty");
        assert!(
            r.url.starts_with("http"),
            "result URL must be HTTP(S), got: {}",
            r.url
        );
    }
}

#[test]
#[ignore = "requires SearXNG at 100.96.81.109:8888"]
fn e2e_searxng_respects_max_results_limit() {
    let results = search::search(SEARXNG, "open source software", 2).unwrap();
    assert!(results.len() <= 2, "got {} results, expected ≤ 2", results.len());
}

// ── Fetch + Extract ───────────────────────────────────────────────────────────

#[test]
#[ignore = "requires internet access"]
fn e2e_fetch_example_com() {
    // example.com is maintained by IANA — always returns simple readable HTML
    let html = fetch::fetch_html("http://example.com").unwrap();
    assert!(!html.is_empty(), "HTML body should not be empty");

    let text = extract::extract_text(&html);
    assert!(
        text.len() >= 50,
        "extracted text too short: {} chars — '{}'",
        text.len(),
        &text[..text.len().min(100)]
    );
}

#[test]
#[ignore = "requires internet access"]
fn e2e_fetch_wikipedia_article() {
    let html =
        fetch::fetch_html("https://en.wikipedia.org/wiki/Rust_(programming_language)").unwrap();
    assert!(!html.is_empty());

    let text = extract::extract_text(&html);
    assert!(
        text.len() >= 500,
        "Wikipedia article should yield substantial content, got {} chars",
        text.len()
    );
    assert!(
        text.to_lowercase().contains("rust"),
        "content should mention Rust"
    );
}

// ── Full Pipeline: search → fetch → extract ───────────────────────────────────

#[test]
#[ignore = "requires SearXNG + internet"]
fn e2e_search_then_fetch_pipeline() {
    // Simulates the two-turn LLM workflow:
    //   Turn 1: web.search  → LLM picks a URL
    //   Turn 2: web.fetch   → LLM reads the content

    let results = search::search(SEARXNG, "what is the Rust borrow checker", 3).unwrap();
    assert!(!results.is_empty(), "search produced no results");

    println!("\n=== Search results ===");
    for r in &results {
        println!("  [{}] {}", r.title, r.url);
    }

    // Fetch the first result
    let url = &results[0].url;
    match fetch::fetch_html(url) {
        Ok(html) => {
            let text = extract::extract_text(&html);
            println!("\n=== Extracted ({} chars) from {} ===", text.len(), url);
            println!("{}", &text[..text.len().min(500)]);
            if text.len() < 200 {
                println!("NOTE: sparse content — LLM should escalate to remote browser");
            }
        }
        Err(e) => println!("fetch failed for {url}: {e}"),
    }
}
