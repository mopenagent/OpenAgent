//! Integration tests using mockito to simulate SearXNG and HTTP endpoints.
//!
//! No real network calls. Run with: `cargo test --test integration`

use browser_lib::{extract, fetch, search};
use mockito::Server;

// ── search ────────────────────────────────────────────────────────────────────

#[test]
fn search_returns_parsed_results() {
    let mut server = Server::new();
    let _m = server
        .mock("GET", "/search")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"results":[
                {"url":"https://example.com","title":"Example","content":"A snippet about Rust."},
                {"url":"https://rust-lang.org","title":"Rust Lang","content":"Systems programming."}
            ]}"#,
        )
        .create();

    let results = search::search(&server.url(), "rust", 5).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].url, "https://example.com");
    assert_eq!(results[1].title, "Rust Lang");
    assert_eq!(results[0].snippet, "A snippet about Rust.");
}

#[test]
fn search_respects_max_results() {
    let mut server = Server::new();
    let _m = server
        .mock("GET", "/search")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"results":[
                {"url":"https://a.com","title":"A","content":""},
                {"url":"https://b.com","title":"B","content":""},
                {"url":"https://c.com","title":"C","content":""}
            ]}"#,
        )
        .create();

    let results = search::search(&server.url(), "query", 2).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn search_filters_results_with_empty_url() {
    let mut server = Server::new();
    let _m = server
        .mock("GET", "/search")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"results":[
                {"url":"","title":"No URL","content":"bad"},
                {"url":"https://good.com","title":"Good","content":"good"}
            ]}"#,
        )
        .create();

    let results = search::search(&server.url(), "query", 5).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].url, "https://good.com");
}

#[test]
fn search_returns_err_on_server_error() {
    let mut server = Server::new();
    let _m = server.mock("GET", "/search").with_status(500).create();
    assert!(search::search(&server.url(), "query", 5).is_err());
}

#[test]
fn search_returns_err_on_missing_results_field() {
    let mut server = Server::new();
    let _m = server
        .mock("GET", "/search")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"query":"foo"}"#)
        .create();
    assert!(search::search(&server.url(), "foo", 5).is_err());
}

// ── fetch ─────────────────────────────────────────────────────────────────────

#[test]
fn fetch_returns_html_body_on_200() {
    let html = "<html><body><article>\
        <h1>Hello</h1>\
        <p>This is a test page with enough content for the readability parser.</p>\
        <p>More content to ensure the article body passes the minimum length threshold.</p>\
        </article></body></html>";

    let mut server = Server::new();
    let _m = server
        .mock("GET", "/page")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body(html)
        .create();

    let body = fetch::fetch_html(&format!("{}/page", server.url())).unwrap();
    assert!(body.contains("test page"));
}

#[test]
fn fetch_returns_err_on_404() {
    let mut server = Server::new();
    let _m = server.mock("GET", "/missing").with_status(404).create();
    assert!(fetch::fetch_html(&format!("{}/missing", server.url())).is_err());
}

#[test]
fn fetch_returns_err_on_500() {
    let mut server = Server::new();
    let _m = server.mock("GET", "/error").with_status(500).create();
    assert!(fetch::fetch_html(&format!("{}/error", server.url())).is_err());
}

// ── extract (dom_smoothie integration) ───────────────────────────────────────

#[test]
fn full_pipeline_fetch_then_extract() {
    let html = r#"<!DOCTYPE html><html>
        <head><title>Pipeline Test</title></head>
        <body>
        <article>
            <h1>Pipeline Test</h1>
            <p>This article exercises the full fetch-then-extract pipeline.
            The content is long enough that dom_smoothie will recognise it as
            a readable article and return clean Markdown output.</p>
            <p>Readability algorithms require a minimum character threshold
            before they consider a page readable — this paragraph ensures we
            clear that bar comfortably.</p>
        </article>
        </body></html>"#;

    let mut server = Server::new();
    let _m = server
        .mock("GET", "/article")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body(html)
        .create();

    let raw_html = fetch::fetch_html(&format!("{}/article", server.url())).unwrap();
    let text = extract::extract_text(&raw_html);

    assert!(!text.is_empty(), "pipeline should produce content");
    assert!(
        text.to_lowercase().contains("pipeline"),
        "content should match article"
    );
}
