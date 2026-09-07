//! Documentation the agent gathers and keeps.
//!
//! Against a server on loopback, standing in for both the search engine and the
//! site: a test that reaches the internet tests the internet, and what is being
//! claimed here is what happens to what comes back.

use rook_core::docs::{self, Sources};
use rook_core::{Config, Rook};
use rook_skills::{Environment, SkillIndex};
use rook_store::Store;

/// A search engine and a documentation site in one process.
///
/// `/lite/?q=…` answers the way DuckDuckGo's lite page does, with results
/// pointing back at this same server — which is the only way to have a
/// gathering follow links without leaving the machine.
async fn site(pages: Vec<(&'static str, String)>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    let mut results = String::new();
    for (path, _) in &pages {
        results.push_str(&format!(
            "<a rel=\"nofollow\" class=\"result-link\" href=\"{base}{path}\">Page {path}</a>\
             <td class=\"result-snippet\">what {path} says</td>"
        ));
    }

    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = [0u8; 4096];
            let read = socket.read(&mut scratch).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&scratch[..read]).to_string();
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, body) = if path.starts_with("/lite/") {
                ("200 OK", results.clone())
            } else {
                match pages.iter().find(|(p, body)| path.starts_with(p) && !body.is_empty()) {
                    Some((_, body)) => ("200 OK", body.clone()),
                    // A result the engine offers and the site no longer serves,
                    // which is what a search of anything a year old is full of.
                    None => ("404 Not Found", "<html><body>no such page</body></html>".into()),
                }
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nConnection: close\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    base
}

fn sources(base: &str, pages: usize, bytes: usize) -> Sources {
    let patience = std::time::Duration::from_secs(5);
    Sources {
        search: rook_tools::web::Search::new(rook_tools::web::Engine::DuckDuckGo(base.into()), patience)
            .unwrap(),
        fetch: rook_tools::web::Fetch::new(patience).unwrap(),
        pages,
        bytes,
    }
}

fn page(heading: &str, paragraphs: &[&str]) -> String {
    let body: String = paragraphs.iter().map(|p| format!("<p>{p}</p>")).collect();
    format!("<html><head><title>{heading}</title></head><body><h1>{heading}</h1>{body}</body></html>")
}

fn rook() -> (tempfile::TempDir, Rook) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (skills, _) = SkillIndex::discover(&[]);
    let env = Environment::bare("linux", "x86_64", "0.1.0");
    let rook = Rook::from_parts(store, Config::default(), env, skills, dir.path().to_path_buf());
    (dir, rook)
}

#[tokio::test]
async fn a_gathered_set_keeps_the_reading_and_the_address_it_was_read_from() {
    let base = site(vec![(
        "/persistence",
        page(
            "Persistence",
            &[
                "Redis writes an append only file so that a restart does not lose the last \
                 seconds of writes, and rewrites it in the background when it grows.",
                "Snapshotting is the other durability option, and the two can be used together \
                 so that a restart replays the log on top of the last snapshot.",
            ],
        ),
    )])
    .await;

    let (set, notes) = docs::gather("redis", docs::LATEST, &sources(&base, 3, 100_000)).await.unwrap();

    assert!(notes.is_empty(), "everything served was readable: {notes:?}");
    assert_eq!(set.topic, "redis");
    assert_eq!(set.version, "latest");
    let kept = &set.pages[0];
    assert!(kept.text.contains("append only file"), "the prose is kept: {}", kept.text);
    assert!(!kept.text.contains("<p>"), "the markup is not: {}", kept.text);
    assert!(
        kept.url.starts_with(&base) && kept.url.ends_with("/persistence"),
        "the address it was read from travels with it: {}",
        kept.url
    );
}

#[tokio::test]
async fn a_set_stops_at_the_size_it_may_keep_rather_than_after() {
    // Two pages, each on its own bigger than the whole allowance.
    let long = "Every sentence here says the same thing at length. ".repeat(200);
    let base = site(vec![("/one", page("One", &[&long])), ("/two", page("Two", &[&long]))]).await;
    const ALLOWED: usize = 4_000;
    assert!(long.len() > ALLOWED, "the bound has to be reachable to be tested: {}", long.len());

    let (set, _) = docs::gather("redis", docs::LATEST, &sources(&base, 5, ALLOWED)).await.unwrap();

    assert!(
        set.bytes() <= ALLOWED,
        "a set holds what it may and no more: {} bytes against {ALLOWED}",
        set.bytes()
    );
    assert!(set.pages[0].text.contains("was past the size"), "and says where it stopped");
}

#[tokio::test]
async fn a_result_that_cannot_be_read_is_reported_rather_than_ending_the_gathering() {
    // Three results: one that 404s, one that is a heading with a link under it,
    // and one that is documentation. Every real search has all three.
    let base = site(vec![
        ("/gone", String::new()),
        ("/stub", page("Stub", &["See also."])),
        (
            "/alive",
            page(
                "Alive",
                &[
                    "The page that does answer, at enough length to be worth keeping around \
                     rather than being read as a stub.",
                    "A second paragraph, so that what is kept is a page and not a heading with \
                     a link under it.",
                ],
            ),
        ),
    ])
    .await;

    let (set, notes) = docs::gather("redis", docs::LATEST, &sources(&base, 3, 100_000)).await.unwrap();

    assert_eq!(set.pages.len(), 1, "the one readable page is kept: {:?}", set.pages);
    assert!(set.pages[0].url.ends_with("/alive"), "and it is the right one: {}", set.pages[0].url);
    assert_eq!(notes.len(), 2, "both failures are said out loud: {notes:?}");
    assert!(notes.iter().any(|n| n.contains("/gone") && n.contains("404")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("/stub") && n.contains("almost nothing")), "{notes:?}");
}

#[test]
fn a_passage_comes_back_with_the_page_it_was_read_from() {
    let set = docs::DocSet::new(
        "redis",
        docs::LATEST,
        vec![
            docs::Page {
                url: "https://redis.io/persistence".into(),
                title: "Persistence".into(),
                text: "Redis persists to an append only file, rewritten in the background.\n\n\
                       Replication copies a primary to its replicas asynchronously by default."
                    .into(),
            },
            docs::Page {
                url: "https://redis.io/clustering".into(),
                title: "Clustering".into(),
                text: "A cluster shards keys across nodes by hash slot, sixteen thousand of them.".into(),
            },
        ],
    );

    let found = set.passages("how does replication work", 2);

    assert!(!found.is_empty(), "the words of the question are in one of the pages");
    let (text, url) = &found[0];
    assert!(text.contains("Replication"), "the passage answers the question: {text}");
    assert_eq!(*url, "https://redis.io/persistence", "and names where it was read");
}

#[test]
fn reading_the_same_topic_again_replaces_the_copy_instead_of_keeping_two() {
    let (_dir, rook) = rook();
    let page =
        |text: &str| docs::Page { url: "https://redis.io/".into(), title: "Redis".into(), text: text.into() };

    rook.keep_docs(&docs::DocSet::new("redis", docs::LATEST, vec![page("the first reading")])).unwrap();
    rook.keep_docs(&docs::DocSet::new("redis", docs::LATEST, vec![page("the second reading")])).unwrap();

    let kept = rook.docs_kept().unwrap();
    assert_eq!(kept.len(), 1, "one topic at one version is one set: {kept:?}");
    let set = rook.docs("redis", None).unwrap().unwrap();
    assert_eq!(set.pages[0].text, "the second reading");
}

#[test]
fn two_versions_of_a_topic_are_two_sets_and_the_unnamed_one_is_latest() {
    let (_dir, rook) = rook();
    let page =
        |text: &str| docs::Page { url: "https://redis.io/".into(), title: "R".into(), text: text.into() };
    rook.keep_docs(&docs::DocSet::new("redis", docs::LATEST, vec![page("current")])).unwrap();
    rook.keep_docs(&docs::DocSet::new("redis", "6.2", vec![page("older")])).unwrap();

    assert_eq!(rook.docs_kept().unwrap().len(), 2);
    assert_eq!(rook.docs("redis", None).unwrap().unwrap().pages[0].text, "current");
    assert_eq!(rook.docs("redis", Some("6.2")).unwrap().unwrap().pages[0].text, "older");

    // Dropping without a version drops the topic, which is what somebody who
    // did not name one means both times.
    assert_eq!(rook.forget_docs("redis", None).unwrap(), 2);
    assert!(rook.docs("redis", None).unwrap().is_none());
}

#[test]
fn a_topic_spelled_with_anything_lands_in_one_reference() {
    assert_eq!(docs::reference("Redis", "Latest"), "docs/redis/latest");
    assert_eq!(docs::reference("PostgreSQL 16", "17.2"), "docs/postgresql-16/17.2");
    // A name that would otherwise open a second level of reference, which is
    // where a set would go missing.
    assert_eq!(docs::reference("k8s/networking", "v1.30"), "docs/k8s-networking/v1.30");
}

#[tokio::test]
async fn gathering_with_the_web_off_says_so_instead_of_coming_back_empty() {
    let (_dir, mut rook) = rook();
    rook.config.web.enabled = false;

    let refused = match rook.doc_sources() {
        Ok(_) => panic!("the web is off; there is nothing to gather with"),
        Err(e) => e.to_string(),
    };

    assert!(refused.contains("web access is off"), "{refused}");
    assert!(refused.contains("[web] enabled"), "and says what to change: {refused}");
}
