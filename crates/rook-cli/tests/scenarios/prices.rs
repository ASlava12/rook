use super::*;
use serde_json::{Value, json};

fn seed(rook: &Rook) {
    rook.write_config("# operator comment\n[agent]\nmodel='cloud'\n[models.cloud]\nmodel='physical'\nendpoint='public'\ncontext_window=1234\ninput_usd_per_million=9.0\n[endpoints.public]\napi='responses'\nurl='https://api.openai.com/v1'\nkey='secret:account'\nproxy='direct'\n[models.local]\nmodel='physical'\napi='openai'\nurl='http://127.0.0.1:1/v1'\nproxy='direct'\n");
    let marker = rook.home.path().join("helper-ran");
    let command = if cfg!(windows) {
        format!("cmd:cmd /c echo surprise > {}", marker.display())
    } else {
        format!("cmd:sh -c 'echo surprise > {}'", marker.display())
    };
    std::fs::write(
        rook.home.path().join("secrets.toml"),
        format!("[account]\nsource={}\n", serde_json::to_string(&command).unwrap()),
    )
    .unwrap();
    let cache = rook.home.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let body = json!({"version":1,"observed_at":rook_store::now_unix(),"data":{"openai":{"id":"openai","models":{"physical":{"id":"physical","limit":{"context":99999},"cost":{"input":2.0,"output":6.0,"cache_read":0.2}}}}}});
    std::fs::write(cache.join("price-reference-v1.json"), body.to_string()).unwrap();
}

#[test]
fn actual_cli_and_daemon_reference_reviews_preserve_manual_rates_and_refuse_stale_accounts() {
    rook_llm::init_tls();
    for shared in [false, true] {
        let rook = Rook::new();
        seed(&rook);
        let daemon = shared.then(|| Daemon::start(&rook));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::new();
        let inspect = || -> Value {
            if let Some(daemon) = &daemon {
                runtime.block_on(async {
                    client
                        .get(format!("{}/api/models/prices", daemon.address))
                        .send()
                        .await
                        .unwrap()
                        .error_for_status()
                        .unwrap()
                        .json()
                        .await
                        .unwrap()
                })
            } else {
                rook.json(&["prices"])
            }
        };
        let apply = |token: &str| -> bool {
            if let Some(daemon) = &daemon {
                runtime.block_on(async {
                    client
                        .post(format!("{}/api/models/prices/apply", daemon.address))
                        .json(&json!({"source":"cloud","review_token":token}))
                        .send()
                        .await
                        .unwrap()
                        .status()
                        .is_success()
                })
            } else {
                rook.run(&["prices", "--source", "cloud", "--apply", token, "--json"]).status.success()
            }
        };
        let listing = inspect();
        assert_eq!(listing["source_url"], rook_core::price_catalog::SOURCE);
        let cloud = listing["models"].as_array().unwrap().iter().find(|m| m["source"] == "cloud").unwrap();
        let token = cloud["review_token"].as_str().unwrap();
        assert_eq!(cloud["configured"]["input"], 9.0);
        let local = listing["models"].as_array().unwrap().iter().find(|m| m["source"] == "local").unwrap();
        assert!(
            local["provider"].is_null() && local["reference"].is_null() && local["review_token"].is_null()
        );
        assert!(!rook.home.path().join("helper-ran").exists());
        // A credential definition rotation invalidates the previously seen form.
        std::fs::write(rook.home.path().join("secrets.toml"), "[account]\nvalue='rotated-account'\n")
            .unwrap();
        let before = std::fs::read(rook.home.path().join("config.toml")).unwrap();
        assert!(!apply(token));
        assert_eq!(std::fs::read(rook.home.path().join("config.toml")).unwrap(), before);
        let fresh = inspect();
        let token = fresh["models"][0]["review_token"].as_str().unwrap();
        assert!(apply(token));
        assert!(!apply(token), "a consumed review cannot overwrite a later form");
        let result = inspect();
        assert_eq!(result["models"][0]["configured"]["input"], 9.0);
        assert_eq!(result["models"][0]["configured"]["output"], 6.0);
        assert_eq!(result["models"][0]["configured"]["cache_read"], 0.2);
        let text = std::fs::read_to_string(rook.home.path().join("config.toml")).unwrap();
        assert!(text.contains("# operator comment") && text.contains("context_window=1234"));
        assert!(!rook.home.path().join("helper-ran").exists());
        if let Some(daemon) = &daemon {
            let response = runtime.block_on(async {
                client
                    .post(format!("{}/api/models/prices/apply", daemon.address))
                    .body(" ".repeat(4097))
                    .header("content-type", "application/json")
                    .send()
                    .await
                    .unwrap()
            });
            assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
        }
        drop(daemon);
    }
}

#[test]
fn inline_and_vault_environment_account_rotations_invalidate_native_reviews_without_helpers() {
    for vault_env in [false, true] {
        let rook = Rook::new();
        seed(&rook);
        if vault_env {
            std::fs::write(
                rook.home.path().join("secrets.toml"),
                "[account]\nsource='env:ROOK_PRICE_TEST_ACCOUNT'\n",
            )
            .unwrap();
        } else {
            let path = rook.home.path().join("config.toml");
            std::fs::write(
                &path,
                std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("secret:account", "env:ROOK_PRICE_TEST_ACCOUNT"),
            )
            .unwrap();
        }
        let run = |account: &str, token: Option<&str>| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_rook"));
            command
                .env("ROOK_HOME", rook.home.path())
                .env("ROOK_PRICE_TEST_ACCOUNT", account)
                .args(["prices", "--json", "--source", "cloud"]);
            if let Some(token) = token {
                command.args(["--apply", token]);
            }
            command.output().unwrap()
        };
        let listing: Value = serde_json::from_slice(&run("first", None).stdout).unwrap();
        let token = listing["models"][0]["review_token"].as_str().unwrap();
        assert!(!run("second", Some(token)).status.success());
        assert!(run("first", Some(token)).status.success());
    }
}
