//! Tester acceptance for T-12b (docs/04 W-1..W-4, rules 2 and 5; D-509..D-513): one
//! adversarial test per item, driven through `harness_core::mcp::dispatch` like `serve`.
//! The proxy item (4) lives in its own binary, tests/accept_t12b_proxy.rs.

mod common;

use common::*;
use serde_json::json;
use std::path::PathBuf;

fn no_tools() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no-tools")
}

/// A `<` an HTML parser would read as markup: `<` then an ASCII letter, `!`, `/` or `?`.
fn tag_like(t: &str) -> Option<String> {
    let b = t.as_bytes();
    (0..b.len().saturating_sub(1))
        .find(|&i| b[i] == b'<' && (b[i + 1].is_ascii_alphabetic() || b"!/?".contains(&b[i + 1])))
        .map(|i| t[i..t.len().min(i + 40)].to_string())
}

fn b64(data: &[u8], url: bool) -> String {
    let s = base64(data);
    if url {
        s.replace('+', "-")
            .replace('/', "_")
            .trim_end_matches('=')
            .to_string()
    } else {
        s
    }
}

/// Every byte string that a run of base64 (standard or url alphabet) in `t` decodes to, at
/// each of the four alignments, lenient about padding.
fn base64_decodings(t: &str) -> Vec<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    }
    let mut out = Vec::new();
    for run in t.split(|c: char| val(c as u8).is_none() || !c.is_ascii()) {
        let r = run.as_bytes();
        for off in 0..4.min(r.len()) {
            let mut bytes = Vec::new();
            let mut acc = 0u32;
            let mut bits = 0;
            for &c in &r[off..] {
                acc = (acc << 6) | val(c).unwrap();
                bits += 6;
                if bits >= 8 {
                    bits -= 8;
                    bytes.push((acc >> bits) as u8);
                    acc &= (1 << bits) - 1;
                }
            }
            out.push(bytes);
        }
    }
    out
}

fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Item 1 adversarial (rule 2, D-77 on main): an API that answers with the vault value in
/// base64 or base64url, at any alignment and with any prefix, never shows it to the model
/// or the ledger in a decodable form.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_vault_value_in_any_base64_spelling_is_scrubbed() {
    let canary = canary();
    let mut fields = serde_json::Map::new();
    for (i, prefix) in ["", "x", "xy", "token ", "Bearer ", "user:"]
        .iter()
        .enumerate()
    {
        let raw = format!("{prefix}{canary}");
        fields.insert(format!("std{i}"), json!(b64(raw.as_bytes(), false)));
        fields.insert(format!("url{i}"), json!(b64(raw.as_bytes(), true)));
    }
    // A JWT-shaped answer: header.payload.signature with the value in the payload.
    let payload = b64(format!("{{\"sub\":\"{canary}\"}}").as_bytes(), true);
    fields.insert(
        "jwt".into(),
        json!(format!("eyJhbGciOiJIUzI1NiJ9.{payload}.c2ln")),
    );
    let body = serde_json::Value::Object(fields).to_string();
    let server = Server::start(vec![
        route(
            Some("api.github.com"),
            "/b64",
            200,
            "application/json",
            body.clone(),
        ),
        route(Some("api.github.com"), "/b64.txt", 200, "text/plain", body),
        Route {
            host: Some("api.github.com".into()),
            path: "/echo64".into(),
            reply: Reply::EchoHeaderBase64("authorization".into()),
        },
    ])
    .await;
    let e = env(config(&server, &["api.github.com"], no_tools())).await;
    let mut texts = Vec::new();
    for p in ["/b64", "/b64.txt"] {
        let out = call(
            &e,
            "api",
            json!({ "url": format!("http://api.github.com{p}") }),
        )
        .await
        .unwrap_or_else(|err| panic!("api {p}: {err}"));
        assert!(out.text.contains("HTTP 200"), "{p}");
        texts.push(out.text);
    }
    for hdr in [
        "token @vault(gh_token)",
        "x@vault(gh_token)",
        "Bearer xy@vault(gh_token)",
    ] {
        let out = call(
            &e,
            "api",
            json!({"url": "http://api.github.com/echo64", "headers": {"Authorization": hdr}}),
        )
        .await
        .expect("api echo64");
        texts.push(out.text);
    }
    texts.push(ledger_dump(&e));
    let needle = canary.as_bytes();
    for (i, t) in texts.iter().enumerate() {
        assert!(!t.contains(&canary), "plain vault value in output {i}");
        assert!(
            !base64_decodings(t)
                .iter()
                .any(|d| contains_bytes(d, needle)),
            "a base64 spelling of the vault value survived in output {i}"
        );
    }
}

/// Item 2 adversarial (rule 5, D-508, D-510): HTML labelled as text/plain or JSON, in every
/// spelling tried, is never shown raw by api.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_never_shows_a_page_dressed_as_text_or_json() {
    let page = "<!DOCTYPE html><html><body><p>hi</p><script>stealCookies()</script></body></html>";
    let escaped = r#""<!DOCTYPE html><script>stealCookies()</script>""#;
    let mut utf16 = vec![0xff, 0xfe];
    for u in page.encode_utf16() {
        utf16.extend_from_slice(&u.to_le_bytes());
    }
    let mut utf16_nobom = Vec::new();
    for u in page.encode_utf16() {
        utf16_nobom.extend_from_slice(&u.to_le_bytes());
    }
    let cases: Vec<(&str, &str, Vec<u8>)> = vec![
        ("/p1", "text/plain", page.as_bytes().to_vec()),
        ("/p2", "TEXT/PLAIN; charset=UTF-8", page.as_bytes().to_vec()),
        ("/p3", "text/plain", utf16),
        ("/p4", "text/plain; charset=utf-16le", utf16_nobom),
        (
            "/p5",
            "text/plain",
            format!("\u{feff}  {page}").into_bytes(),
        ),
        ("/p6", "text/plain", escaped.as_bytes().to_vec()),
        (
            "/p7",
            "text/plain",
            br#"{"a": {"b": ["<script>stealCookies()</script>"]}}"#.to_vec(),
        ),
        (
            "/p8",
            "text/plain",
            br#"["<img src=x onerror=alert(1)>"]"#.to_vec(),
        ),
        ("/j1", "application/json", page.as_bytes().to_vec()),
        ("/j2", "application/json", escaped.as_bytes().to_vec()),
        (
            "/j3",
            "application/json",
            format!(")]}}'\n{escaped}").into_bytes(),
        ),
        (
            "/j4",
            "application/problem+json",
            escaped.as_bytes().to_vec(),
        ),
        (
            "/j5",
            "application/json",
            format!("  \n {escaped} \n").into_bytes(),
        ),
        (
            "/j6",
            "text/json",
            serde_json::to_vec(&json!(page)).unwrap(),
        ),
        ("/n1", "", page.as_bytes().to_vec()),
        ("/n2", "", escaped.as_bytes().to_vec()),
        ("/x1", "application/xhtml+xml", page.as_bytes().to_vec()),
        ("/x2", "text/csv", page.as_bytes().to_vec()),
    ];
    // The same JSON bodies with every `<` and `>` written as a JSON unicode escape
    // (backslash, `u003c` / `u003e`), so the raw body holds no `<` at all. The backslash is
    // built from its byte value: a literal escape in this source could be rewritten by an
    // editor or a tool into the character itself.
    let bs = char::from(92u8);
    let esc = |s: &str| {
        s.replace('<', &format!("{bs}u003c"))
            .replace('>', &format!("{bs}u003e"))
    };
    let mut cases = cases;
    let escaped_u = esc(escaped);
    assert!(!escaped_u.contains('<') && escaped_u.contains("u003c"));
    let deep_u = esc(r#"{"a": {"b": ["<script>stealCookies()</script>"]}}"#);
    let arr_u = esc(r#"["<img src=x onerror=alert(1)>"]"#);
    let obj_u = esc(r#"{"html": "<!DOCTYPE html><p>hi</p>"}"#);
    for (p, ct, b) in [
        ("/u1", "text/plain", escaped_u.clone()),
        ("/u2", "text/plain", deep_u),
        ("/u3", "text/plain", arr_u),
        ("/u4", "text/plain", obj_u),
        (
            "/u5",
            "text/plain; charset=utf-8",
            format!(")]}}'\n{escaped_u}"),
        ),
        ("/u6", "application/json", escaped_u.clone()),
        ("/u7", "application/json", format!(")]}}'\n{escaped_u}")),
        ("/u8", "", escaped_u.clone()),
        ("/u9", "application/vnd.api+json", escaped_u),
    ] {
        cases.push((p, ct, b.into_bytes()));
    }
    let routes = cases
        .iter()
        .map(|(p, ct, body)| {
            let mut r = route(Some("a.test"), p, 200, ct, body.clone());
            if ct.is_empty() {
                r.reply = Reply::Fixed {
                    status: 200,
                    headers: vec![],
                    body: body.clone(),
                };
            }
            r
        })
        .collect();
    let server = Server::start(routes).await;
    let e = env(config(&server, &["a.test"], no_tools())).await;
    let mut bad = Vec::new();
    for (p, ct, _) in &cases {
        let r = call(&e, "api", json!({ "url": format!("http://a.test{p}") })).await;
        let t = text(&r);
        let lower = t.to_ascii_lowercase();
        if let Some(tag) = tag_like(&t) {
            bad.push(format!("{p} ({ct}): markup shown: {tag:?}"));
        } else if lower.contains("stealcookies") || lower.contains("onerror") {
            bad.push(format!("{p} ({ct}): page text shown: {t}"));
        } else if !t.contains("not shown") {
            bad.push(format!("{p} ({ct}): no 'not shown' line: {t}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Item 3 adversarial (D-512): text/plain keeps angle-bracket text (escaped) and drops real
/// markup, in read and sheet mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_plain_keeps_angle_text_and_drops_real_tags() {
    let body = "Generics and includes\n\n\
        fn names() -> Vec<String> returns them.\n\n\
        #include <stdio.h> first.\n\n\
        when a < b > c holds.\n\n\
        nested Result<Vec<u8>, Error> here.\n\n\
        map HashMap<K, V> there.\n\n\
        <script>stealCookies()</script>\n\n\
        <img src=x onerror=alert(1)>after img\n\n\
        <a href=\"javascript:alert(1)\">link text</a>\n\n\
        <style>body{color:red}</style>\n\n\
        <x-evil>custom words</x-evil>\n\n\
        <unknown data=1>attr words</unknown>\n\n\
        <scr<script>ipt>mixed</script>\n\n\
        <!-- hidden comment --> visible tail\n";
    let server = Server::start(vec![route(
        Some("t.test"),
        "/notes.txt",
        200,
        "text/plain; charset=utf-8",
        body,
    )])
    .await;
    let e = env(config(&server, &["t.test"], no_tools())).await;
    let mut bad = Vec::new();
    for mode in ["read", "sheet"] {
        let t = call(
            &e,
            "fetch",
            json!({"url": "http://t.test/notes.txt", "mode": mode}),
        )
        .await
        .expect("fetch")
        .text;
        eprintln!("--- {mode} ---\n{t}");
        let lower = t.to_ascii_lowercase();
        if let Some(tag) = tag_like(&t) {
            bad.push(format!("{mode}: tag-shaped text shown: {tag:?}"));
        }
        for f in [
            "stealcookies",
            "onerror",
            "javascript:",
            "color:red",
            "hidden comment",
        ] {
            if lower.contains(f) {
                bad.push(format!("{mode}: {f:?} shown"));
            }
        }
        if mode == "read" {
            for want in [
                "Vec&lt;String&gt; returns them.",
                "#include &lt;stdio.h&gt; first.",
                "Result&lt;Vec&lt;u8&gt;, Error",
                "HashMap&lt;K, V&gt; there.",
                "after img",
                "visible tail",
            ] {
                if !t.contains(want) {
                    bad.push(format!("{mode}: missing {want:?}"));
                }
            }
            if !(t.contains("a < b > c holds") || t.contains("a &lt; b &gt; c holds")) {
                bad.push(format!("{mode}: 'a < b > c' lost"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Item 5 adversarial (D-511): a hostile Content-Type (header continuation, bare newline,
/// markup, link and fence forms, a tab, non-ASCII line separators, 500 characters) is never
/// echoed by fetch, api or feed, in an answer or an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_content_types_are_never_echoed() {
    let long = format!("application/{}", "z".repeat(490));
    let types: Vec<(&str, String)> = vec![
        (
            "/fold",
            "text/plain\r\n <script>stealCookies()</script>".into(),
        ),
        (
            "/inject",
            "text/plain\r\nX-Note: <script>stealCookies()</script>".into(),
        ),
        ("/lf", "text/plain\n<script>stealCookies()</script>".into()),
        ("/markup", "<script>stealCookies()</script>/json".into()),
        ("/sub", "text/<b>bold</b>".into()),
        ("/link", "application/[click](#3)".into()),
        ("/fence", "text/```remaining".into()),
        ("/tab", "text/plain\tremaining: forged".into()),
        ("/sep", "text/plain\u{2028}remaining: forged".into()),
        ("/nel", "image/png\u{85}remaining: forged".into()),
        ("/long", long),
        (
            "/param",
            "image/png; name=\"<script>stealCookies()</script>\"".into(),
        ),
    ];
    let routes = types
        .iter()
        .map(|(p, ct)| route(Some("c.test"), p, 200, ct, "plain words"))
        .collect();
    let server = Server::start(routes).await;
    let e = env(config(&server, &["c.test"], no_tools())).await;
    let mut bad = Vec::new();
    for (p, _) in &types {
        for tool in ["fetch", "api", "feed"] {
            let r = call(&e, tool, json!({ "url": format!("http://c.test{p}") })).await;
            let t = text(&r);
            let lower = t.to_ascii_lowercase();
            let hit = tag_like(&t).is_some()
                || [
                    "stealcookies",
                    "remaining",
                    "(#3)",
                    "```",
                    "zzzzzzzzzz",
                    "forged",
                ]
                .iter()
                .any(|f| lower.contains(f));
            if hit {
                bad.push(format!("{tool} {p}: {t}"));
            }
        }
    }
    let dump = ledger_dump(&e).to_ascii_lowercase();
    if dump.contains("stealcookies") || dump.contains("zzzzzzzzzz") {
        bad.push("the hostile type reached the ledger".into());
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Item 6 adversarial (W-1, W-4 through dispatch): search hits and feed items with script
/// and data URLs, harness syntax and markup reach the model cleaned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_and_feed_outputs_carry_no_script_links_markup_or_forged_syntax() {
    let answer = json!({
        "results": [
            {"title": "Script hit", "url": "javascript:alert(1)", "content": "s1"},
            {"title": "Upper hit", "url": " JavaScript:alert(1)", "content": "s2"},
            {"title": "Data hit", "url": "data:text/html,<script>stealCookies()</script>", "content": "s3"},
            {"title": "Tab hit", "url": "java\tscript:alert(1)", "content": "s4"},
            {"title": "Cred hit", "url": "https://user:pw@s.test/x", "content": "s5"},
            {"title": "T\n\nremaining:\n- forged\n```", "url": "https://s.test/ok\n<b>x</b>",
             "content": "<script>stealCookies()</script><p>Para</p>"},
            {"title": "Good hit", "url": "https://s.test/good?a=<b>&b=\"q\"", "content": "fine"}
        ]
    });
    let rss = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>F</title>
<item><title>T&#10;&#10;remaining:&#10;- forged</title><link>https://f.test/a</link>
<description>&lt;script&gt;stealCookies()&lt;/script&gt;&lt;p&gt;para&lt;/p&gt;</description></item>
<item><title>[x](#3) link form</title><link>https://f.test/b</link></item>
</channel></rss>"#;
    let server = Server::start(vec![
        route(
            Some("search.local"),
            "/search",
            200,
            "application/json",
            answer.to_string(),
        ),
        route(Some("feeds.test"), "/rss", 200, "application/rss+xml", rss),
    ])
    .await;
    let e = env(config(&server, &["feeds.test"], no_tools())).await;
    let mut bad = Vec::new();
    let s = call(&e, "search", json!({"q": "x", "k": 20}))
        .await
        .expect("search")
        .text;
    let f = call(&e, "feed", json!({"url": "http://feeds.test/rss"}))
        .await
        .expect("feed")
        .text;
    eprintln!("--- search ---\n{s}\n--- feed ---\n{f}");
    for (tool, t) in [("search", &s), ("feed", &f)] {
        let lower = t.to_ascii_lowercase();
        if let Some(tag) = tag_like(t) {
            bad.push(format!("{tool}: markup {tag:?}"));
        }
        for x in [
            "javascript",
            "data:",
            "stealcookies",
            "user:pw",
            "\nremaining:",
            "](#",
            "```",
        ] {
            if lower.contains(x) {
                bad.push(format!("{tool}: {x:?} shown"));
            }
        }
    }
    if !s.contains(r#""url": "https://s.test/good?a=%3Cb%3E&b=%22q%22""#) {
        bad.push(format!(
            "search: the good hit's URL is not the cleaned one:\n{s}"
        ));
    }
    if !f.contains(r#""url": "https://f.test/a""#) || !f.contains("para") {
        bad.push("feed: item lost".into());
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Item 7 adversarial (D-513): relative feed links resolve against xml:base (absolute and
/// relative) and the final URL after redirects (also across hosts); script and data links,
/// however spelled and wherever they come from (link, xml:base, JSON Feed, iCal), are not
/// shown as links.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feed_links_resolve_and_script_links_are_dropped() {
    let rss = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>R</title>
<item><title>Rel</title><link>posts/2</link></item>
<item><title>Proto</title><link>//proto.test/p</link></item>
<item><title>Js1</title><link>javascript:alert(1)</link></item>
<item><title>Js2</title><link>  JavaScript:alert(2)</link></item>
<item><title>Js3</title><link>java&#9;script:alert(3)</link></item>
<item><title>Data</title><link>data:text/html,&lt;script&gt;x&lt;/script&gt;</link></item>
<item><title>Vb</title><link>vbscript:msgbox(1)</link></item>
</channel></rss>"#;
    let atom = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xml:base="sub/"><title>A</title><id>urn:a</id>
<updated>2026-10-25T12:00:00Z</updated>
<entry><title>RelBase</title><id>urn:1</id><updated>2026-10-25T12:00:00Z</updated>
<link href="y"/></entry>
<entry xml:base="javascript:alert(9)//"><title>JsBase</title><id>urn:2</id>
<updated>2026-10-25T12:00:00Z</updated><link href="x"/></entry>
<entry xml:base="https://base.test/x/"><title>AbsBase</title><id>urn:3</id>
<updated>2026-10-25T12:00:00Z</updated><link href="../z"/></entry>
<entry><title>DataHref</title><id>urn:4</id><updated>2026-10-25T12:00:00Z</updated>
<link href="data:text/html;base64,PHNjcmlwdD4="/></entry>
</feed>"#;
    let jf = r#"{"version": "https://jsonfeed.org/version/1.1", "title": "J",
 "items": [{"id": "1", "title": "JRel", "url": "../j/1", "content_text": "t"},
           {"id": "2", "title": "JJs", "url": "javascript:alert(1)", "content_text": "t"},
           {"id": "3", "title": "JData", "url": "data:,x", "external_url": "javascript:x", "content_text": "t"}]}"#;
    let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Rel\r\nDTSTART:20261025T180000Z\r\n\
        URL:e/1\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Js\r\nDTSTART:20261025T180000Z\r\n\
        URL:javascript:alert(1)\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Data\r\n\
        DTSTART:20261025T180000Z\r\nURL;VALUE=URI:data:text/html,x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let server = Server::start(vec![
        route(
            Some("feeds.test"),
            "/blog/rss",
            200,
            "application/rss+xml",
            rss,
        ),
        route(
            Some("feeds.test"),
            "/blog/atom",
            200,
            "application/atom+xml",
            atom,
        ),
        route(
            Some("feeds.test"),
            "/blog/sub/feed.json",
            200,
            "application/feed+json",
            jf,
        ),
        route(Some("feeds.test"), "/cal/x.ics", 200, "text/calendar", ics),
        redirect(Some("feeds.test"), "/hop1", "/hop2"),
        redirect(Some("feeds.test"), "/hop2", "http://other.test/deep/atom"),
        route(
            Some("other.test"),
            "/deep/atom",
            200,
            "application/atom+xml",
            atom,
        ),
    ])
    .await;
    let e = env(config(&server, &["feeds.test", "other.test"], no_tools())).await;
    let mut bad = Vec::new();
    let get = |path: &str| {
        let path = path.to_string();
        let e = &e;
        async move {
            call(
                e,
                "feed",
                json!({ "url": format!("http://feeds.test{path}") }),
            )
            .await
            .unwrap_or_else(|err| panic!("feed {path}: {err}"))
            .text
        }
    };
    let rss_t = get("/blog/rss").await;
    let atom_t = get("/blog/atom").await;
    let json_t = get("/blog/sub/feed.json").await;
    let ics_t = get("/cal/x.ics").await;
    let hop_t = get("/hop1").await;
    eprintln!("--- rss ---\n{rss_t}\n--- atom ---\n{atom_t}\n--- json ---\n{json_t}\n--- ics ---\n{ics_t}\n--- hop ---\n{hop_t}");
    let wants: &[(&str, &str, &str)] = &[
        ("rss", &rss_t, r#""url": "http://feeds.test/blog/posts/2""#),
        ("rss", &rss_t, r#""url": "http://proto.test/p""#),
        ("atom", &atom_t, r#""url": "http://feeds.test/blog/sub/y""#),
        ("atom", &atom_t, r#""url": "https://base.test/z""#),
        ("json", &json_t, r#""url": "http://feeds.test/blog/j/1""#),
        ("ics", &ics_t, r#""url": "http://feeds.test/cal/e/1""#),
        ("hop", &hop_t, r#""url": "http://other.test/deep/sub/y""#),
        ("hop", &hop_t, r#""url": "https://base.test/z""#),
    ];
    for (name, t, want) in wants {
        if !t.contains(want) {
            bad.push(format!("{name}: missing {want}"));
        }
    }
    for (name, t) in [
        ("rss", &rss_t),
        ("atom", &atom_t),
        ("json", &json_t),
        ("ics", &ics_t),
        ("hop", &hop_t),
    ] {
        let lower = t.to_ascii_lowercase();
        for x in ["javascript", "vbscript", "data:", "alert("] {
            if lower.contains(x) {
                bad.push(format!("{name}: {x:?} shown"));
            }
        }
        if let Some(tag) = tag_like(t) {
            bad.push(format!("{name}: markup {tag:?}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Item 3 adversarial (D-512, W-2 15 s): a text/plain body that is `<x` repeated on one
/// line (1 MB, well under the 5 MB cap) is walked within the request budget. The call runs
/// on its own thread and runtime so a stuck walk fails the test instead of hanging it.
#[test]
fn text_plain_hostile_angle_runs_finish_in_time() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let body = "<x".repeat(512 * 1024);
            let server = Server::start(vec![route(
                Some("t.test"),
                "/slow.txt",
                200,
                "text/plain",
                body,
            )])
            .await;
            let e = env(config(&server, &["t.test"], no_tools())).await;
            let started = std::time::Instant::now();
            let r = call(&e, "fetch", json!({"url": "http://t.test/slow.txt"})).await;
            let _ = tx.send((started.elapsed(), r.is_ok()));
        });
    });
    match rx.recv_timeout(std::time::Duration::from_secs(20)) {
        Ok((took, ok)) => assert!(
            took < std::time::Duration::from_secs(15),
            "fetch took {took:?} (ok={ok}), over W-2's 15 s"
        ),
        Err(_) => panic!("fetch of 1 MB of `<x` did not return within 20 s (W-2 says 15 s)"),
    }
}

/// Item 7 adversarial (D-513): an item with no link, an empty or whitespace link, or a
/// fragment-only link is not given the feed's own URL as its link.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feed_items_without_a_link_do_not_get_the_feed_url() {
    let rss = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>R</title>
<item><title>NoLink</title></item>
<item><title>Empty</title><link></link></item>
<item><title>Space</title><link>   </link></item>
<item><title>Frag</title><link>#</link></item>
</channel></rss>"#;
    let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:No url\r\nDTSTART:20261025T180000Z\r\n\
        END:VEVENT\r\nEND:VCALENDAR\r\n";
    let server = Server::start(vec![
        route(
            Some("feeds.test"),
            "/r/rss?token=private123",
            200,
            "application/rss+xml",
            rss,
        ),
        route(
            Some("feeds.test"),
            "/cal/basic.ics?token=private123",
            200,
            "text/calendar",
            ics,
        ),
    ])
    .await;
    let e = env(config(&server, &["feeds.test"], no_tools())).await;
    let mut bad = Vec::new();
    for p in ["/r/rss?token=private123", "/cal/basic.ics?token=private123"] {
        let t = call(
            &e,
            "feed",
            json!({ "url": format!("http://feeds.test{p}") }),
        )
        .await
        .unwrap_or_else(|err| panic!("feed {p}: {err}"))
        .text;
        eprintln!("--- {p} ---\n{t}");
        let n = t.matches("private123").count();
        if n > 0 {
            bad.push(format!(
                "{p}: the feed's own URL is given as an item link {n} time(s)"
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Item 3 adversarial b (D-512, rule 5): markup spelled with entities in a text/plain body
/// (`&lt;script&gt;`, `&#60;img ...&#62;`) is not handed to the model as a tag.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_plain_entity_spelled_tags_are_not_shown_as_tags() {
    let body = "Entity notes\n\nentity &lt;script&gt;stealCookies()&lt;/script&gt; text\n\n\
                numeric &#60;img src=x onerror=alert(1)&#62; end\n";
    let server = Server::start(vec![route(
        Some("t.test"),
        "/ent.txt",
        200,
        "text/plain",
        body,
    )])
    .await;
    let e = env(config(&server, &["t.test"], no_tools())).await;
    let t = call(&e, "fetch", json!({"url": "http://t.test/ent.txt"}))
        .await
        .expect("fetch")
        .text;
    eprintln!("{t}");
    assert!(
        tag_like(&t).is_none(),
        "tag-shaped text shown: {:?}",
        tag_like(&t)
    );
}

/// Item 6 adversarial b (rule 5 through dispatch): markup spelled with entities in a search
/// snippet or a feed summary is not handed to the model as a tag.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_and_feed_entity_spelled_markup_is_not_shown_as_tags() {
    let answer = json!({"results": [
        {"title": "E &lt;b&gt;t&lt;/b&gt;", "url": "https://s.test/e",
         "content": "x &lt;script&gt;stealCookies()&lt;/script&gt; y"}]});
    let rss = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>F</title>
<item><title>D</title><link>https://f.test/d</link>
<description>&amp;lt;b&amp;gt;double&amp;lt;/b&amp;gt; and &amp;lt;script&amp;gt;stealCookies()&amp;lt;/script&amp;gt;</description></item>
</channel></rss>"#;
    let server = Server::start(vec![
        route(
            Some("search.local"),
            "/search",
            200,
            "application/json",
            answer.to_string(),
        ),
        route(Some("feeds.test"), "/rss", 200, "application/rss+xml", rss),
    ])
    .await;
    let e = env(config(&server, &["feeds.test"], no_tools())).await;
    let s = call(&e, "search", json!({"q": "x"}))
        .await
        .expect("search")
        .text;
    let f = call(&e, "feed", json!({"url": "http://feeds.test/rss"}))
        .await
        .expect("feed")
        .text;
    eprintln!("--- search ---\n{s}\n--- feed ---\n{f}");
    let mut bad = Vec::new();
    for (tool, t) in [("search", &s), ("feed", &f)] {
        if let Some(tag) = tag_like(t) {
            bad.push(format!("{tool}: tag-shaped text shown: {tag:?}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
