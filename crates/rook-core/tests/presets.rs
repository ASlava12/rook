use rook_core::{
    Config, Rook,
    presets::{Preset, Request},
};
fn open(root: &std::path::Path) -> Rook {
    Rook::from_parts(
        rook_store::Store::open(root).unwrap(),
        Config::default(),
        rook_skills::Environment::bare("linux", "x86_64", "1"),
        Default::default(),
        root.into(),
    )
}
#[test]
fn preset_replacement_and_deletion_require_the_exact_reviewed_revision() {
    let root = tempfile::tempdir().unwrap();
    let rook = open(root.path());
    let preset =
        || Preset { prompt: "Review this change".into(), model: None, effort: Some(rook_llm::Effort::Low) };
    rook.preset_request(Request::Save { name: "review".into(), preset: preset(), expected: None }).unwrap();
    assert!(
        rook.preset_request(Request::Save { name: "review".into(), preset: preset(), expected: None })
            .is_err()
    );
    let first = rook.preset_request(Request::Get { name: "review".into() }).unwrap();
    let revision = first["revision"].as_str().unwrap();
    let changed = Preset { prompt: "Changed prompt".into(), ..preset() };
    rook.preset_request(Request::Save {
        name: "review".into(),
        preset: changed,
        expected: Some(revision.into()),
    })
    .unwrap();
    assert!(
        rook.preset_request(Request::Delete { name: "review".into(), expected: revision.into() }).is_err()
    );
    let current = rook.preset_request(Request::Get { name: "review".into() }).unwrap();
    rook.preset_request(Request::Delete {
        name: "review".into(),
        expected: current["revision"].as_str().unwrap().into(),
    })
    .unwrap();
    assert!(rook.preset_request(Request::List).unwrap()["items"].as_array().unwrap().is_empty());
}
#[test]
fn presets_persist_but_cannot_store_permissions_or_exceed_the_catalog_budget() {
    let root = tempfile::tempdir().unwrap();
    {
        let rook = open(root.path());
        assert!(rook_core::presets::arguments(r#"{"action":"save","name":"x","preset":{"prompt":"hello","model":null,"effort":null,"stance":"free"},"expected":null}"#).is_err());
        for n in 0..32 {
            rook.preset_request(Request::Save {
                name: format!("p{n}"),
                preset: Preset { prompt: "hello".into(), model: None, effort: None },
                expected: None,
            })
            .unwrap();
        }
        assert!(
            rook.preset_request(Request::Save {
                name: "overflow".into(),
                preset: Preset { prompt: "hello".into(), model: None, effort: None },
                expected: None
            })
            .is_err()
        );
    }
    assert_eq!(
        open(root.path()).preset_request(Request::List).unwrap()["items"].as_array().unwrap().len(),
        32
    );
}
