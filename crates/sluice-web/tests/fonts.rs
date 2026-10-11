//! The faces are Sluice's own: every file under `assets/fonts/` is the one its manifest's sha256
//! names, each family's licence (the SIL Open Font License) sits beside its files, every
//! `@font-face` rule points at the dashboard's `/static/fonts/<file>?v=<fingerprint>`, which
//! serves the file's bytes as `font/woff2` to be kept for good, and nothing the dashboard serves
//! (its pages, sheet, scripts and faces) names jsDelivr.
mod board_fixture;
mod neutral;
mod seed;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use board_fixture::Fixture;
use sluice_store::artifacts::fingerprint;
use sluice_web::views::{FONTS, STYLESHEET, served_assets};
use std::{collections::BTreeMap, path::PathBuf};
use tower::ServiceExt;

fn fonts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts")
}

/// The manifest: each file's path under `assets/fonts/` with its sha256.
fn manifest() -> BTreeMap<String, String> {
    std::fs::read_to_string(fonts_dir().join("SHA256SUMS"))
        .unwrap()
        .lines()
        .map(|line| {
            let (sha, path) = line.split_once("  ").expect("<sha256>  <path>");
            (path.to_owned(), sha.to_owned())
        })
        .collect()
}

#[test]
fn every_vendored_file_is_the_one_its_manifest_names() {
    let manifest = manifest();
    let mut on_disk = vec![];
    for family in std::fs::read_dir(fonts_dir()).unwrap() {
        let family = family.unwrap().path();
        if !family.is_dir() {
            assert_eq!(family.file_name().unwrap(), "SHA256SUMS", "{family:?}");
            continue;
        }
        let licence = std::fs::read_to_string(family.join("LICENSE"))
            .unwrap_or_else(|_| panic!("{family:?} carries its licence"));
        assert!(
            licence.contains("SIL Open Font License, Version 1.1"),
            "{family:?}: the OFL"
        );
        for file in std::fs::read_dir(&family).unwrap() {
            let file = file.unwrap().path();
            on_disk.push(
                file.strip_prefix(fonts_dir())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    on_disk.sort();
    assert_eq!(
        on_disk,
        manifest.keys().cloned().collect::<Vec<_>>(),
        "the manifest lists every vendored file and no other"
    );
    for (path, sha) in &manifest {
        let bytes = std::fs::read(fonts_dir().join(path)).unwrap();
        assert_eq!(
            &fingerprint(&bytes),
            sha,
            "{path} is the file its sha256 names"
        );
        if path.ends_with(".woff2") {
            assert!(bytes.starts_with(b"wOF2"), "{path} is a woff2 file");
        }
    }
    // the dashboard embeds exactly the manifest's faces
    let mut served: Vec<&str> = FONTS.iter().flat_map(|a| a.names.iter().copied()).collect();
    served.sort();
    let mut faces: Vec<String> = manifest
        .keys()
        .filter(|p| p.ends_with(".woff2"))
        .map(|p| format!("fonts/{}", p.rsplit('/').next().unwrap()))
        .collect();
    faces.sort();
    assert_eq!(served, faces);
}

/// Every face URL the sheet names, in order.
fn face_urls() -> Vec<&'static str> {
    STYLESHEET
        .split("@font-face {")
        .skip(1)
        .filter_map(|rule| {
            let rule = &rule[..rule.find('}').unwrap()];
            let at = rule.find("url(")? + 4;
            Some(&rule[at..at + rule[at..].find(')').unwrap()])
        })
        .collect()
}

#[tokio::test]
async fn every_face_is_served_by_the_dashboard_and_kept_for_good() {
    let manifest = manifest();
    let by_name: BTreeMap<&str, (&str, &str)> = manifest
        .iter()
        .map(|(path, sha)| {
            (
                path.rsplit('/').next().unwrap(),
                (path.as_str(), sha.as_str()),
            )
        })
        .collect();
    let urls = face_urls();
    assert_eq!(
        urls.len(),
        FONTS.len(),
        "every vendored face has one rule: {urls:?}"
    );
    let f = Fixture::new().await;
    for url in urls {
        let rest = url
            .strip_prefix("/static/fonts/")
            .unwrap_or_else(|| panic!("{url}: a face the dashboard serves"));
        let (name, version) = rest.split_once("?v=").expect("a versioned URL");
        let (path, sha) = by_name[name];
        assert_eq!(
            version,
            &sha[..16],
            "{url} is versioned by its file's sha256"
        );
        let response = f
            .router()
            .oneshot(Request::get(url).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{url}");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "font/woff2",
            "{url}"
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable",
            "{url}"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            body.as_ref(),
            std::fs::read(fonts_dir().join(path)).unwrap(),
            "{url}"
        );
        // without its version, revalidated on every use
        let plain = f
            .router()
            .oneshot(
                Request::get(format!("/static/fonts/{name}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            plain.headers()[header::CACHE_CONTROL],
            "public, max-age=0, must-revalidate",
            "{name}"
        );
    }
    let missing = f
        .router()
        .oneshot(
            Request::get("/static/fonts/nope.woff2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn nothing_the_dashboard_serves_names_jsdelivr() {
    let named = |bytes: &[u8]| {
        bytes
            .to_ascii_lowercase()
            .windows(8)
            .any(|w| w == b"jsdelivr")
    };
    for asset in served_assets() {
        assert!(!named(asset.bytes), "{:?} names jsdelivr", asset.names);
    }
    assert!(
        !STYLESHEET.contains("url(http"),
        "every url() in the sheet is the dashboard's"
    );
    let f = Fixture::new().await;
    let n = neutral::seed(&f.writer, f._home.path()).await;
    for path in [
        "/".to_owned(),
        "/_ui".to_owned(),
        format!("/projects/id/{}", n.almanac),
        format!("/projects/id/{}", n.chores),
        "/inbox".to_owned(),
        "/fns".to_owned(),
    ] {
        let (status, html) = f.get(&path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(!named(html.as_bytes()), "{path} names jsdelivr");
    }
}
