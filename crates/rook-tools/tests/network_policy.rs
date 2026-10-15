use rook_tools::{Tool, ToolContext, exec::RunCommand, isolate::Mode, network::Settings};
use serde_json::json;
async fn one_at_a_time() -> tokio::sync::MutexGuard<'static, ()> {
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    ONE.lock().await
}
fn context(root: &std::path::Path, allow: Vec<String>) -> ToolContext {
    let mut ctx = ToolContext::new(root.into());
    ctx.isolate = Mode::Required;
    ctx.isolation.network_policy = Settings { enabled: true, allow, ..Default::default() };
    ctx
}
#[tokio::test]
async fn off_and_unsupported_platforms_never_fall_back_to_unrestricted_execution() {
    let _one = one_at_a_time().await;
    let root = tempfile::tempdir().unwrap();
    let mut ctx = context(root.path(), vec![]);
    ctx.isolate = Mode::Off;
    let args = json!({"command":"echo forbidden > launched"});
    let result = RunCommand.call(&ctx, &args).await.unwrap();
    assert!(result.is_error, "{}", result.content);
    assert!(!root.path().join("launched").exists());
    #[cfg(not(target_os = "macos"))]
    {
        ctx.isolate = Mode::Auto;
        let result = RunCommand.call(&ctx, &args).await.unwrap();
        assert!(result.is_error);
        assert!(!root.path().join("launched").exists());
    }
}
#[cfg(target_os = "macos")]
#[tokio::test]
async fn seatbelt_permits_only_brokered_destinations_and_blocks_direct_tcp_udp_and_other_local_ports() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let _one = one_at_a_time().await;
    rook_llm::init_tls();
    assert_eq!(rook_tools::isolate::available(), Ok(rook_tools::isolate::Backend::Seatbelt));
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        let _ = stream.read(&mut request).await.unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nBROKER_RECEIPT")
            .await
            .unwrap();
    });
    let ctx = context(root.path(), vec![format!("127.0.0.1:{port}")]);
    let command = format!("/usr/bin/curl -fsS --max-time 10 http://127.0.0.1:{port}/");
    let accepted = RunCommand.call(&ctx, &json!({"command":command})).await.unwrap();
    assert!(!accepted.is_error, "{}", accepted.content);
    assert!(accepted.content.contains("BROKER_RECEIPT"));
    assert_eq!(accepted.meta["network_policy"]["decisions"][0]["decision"], "allow");
    server.await.unwrap();
    let denied = RunCommand
        .call(&ctx, &json!({"command":"/usr/bin/curl -fsS --max-time 5 https://example.org/"}))
        .await
        .unwrap();
    assert!(denied.is_error);
    assert_eq!(denied.meta["network_policy"]["decisions"][0]["decision"], "deny");
    // The same real local listener is reachable uncontained, and is blocked by
    // Seatbelt even when the command discards all proxy environment variables.
    let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let direct_port = direct.local_addr().unwrap().port();
    let code =
        format!("import socket\ns=socket.create_connection(('127.0.0.1',{direct_port}),2)\ns.close()\n");
    let py = std::process::Command::new("/usr/bin/python3").arg("-c").arg(&code).output().unwrap();
    assert!(py.status.success(), "precondition: local service reachable");
    for code in [
        code,
        "import socket\ns=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)\ns.sendto(b'x',('127.0.0.1',53))\n"
            .into(),
    ] {
        let command = format!("/usr/bin/python3 -c '{}'", code.replace('\'', "'\\''"));
        let result = RunCommand.call(&ctx, &json!({"command":command})).await.unwrap();
        assert!(result.is_error, "direct socket bypass succeeded: {}", result.content);
        assert!(
            result.content.contains("Operation not permitted")
                || result.content.contains("Permission denied"),
            "{}",
            result.content
        );
    }
    let escape=RunCommand.call(&ctx,&json!({"command":format!("/usr/bin/curl --noproxy '*' -fsS --max-time 3 http://127.0.0.1:{direct_port}/")})).await.unwrap();
    assert!(escape.is_error);
    let foreign = rook_tools::ToolBox::standard()
        .call(&ctx, "crate_api", &json!({"crate":"anything"}))
        .await
        .unwrap_err();
    assert!(foreign.to_string().contains("no domain-contained"));
    assert!(RunCommand.call(&ctx, &json!({"command":"echo no","background":true})).await.unwrap().is_error);
}
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires the authorized LAN model host and public npm registry"]
async fn allowed_lan_and_public_registry_work_through_actual_os_containment() {
    rook_llm::init_tls();
    let _one = one_at_a_time().await;
    let root = tempfile::tempdir().unwrap();
    let ctx = context(root.path(), vec!["192.168.1.72:8080".into(), "registry.npmjs.org:443".into()]);
    for command in [
        "/usr/bin/curl -sS --max-time 15 -w '\nLAN_HTTP_STATUS:%{http_code}' http://192.168.1.72:8080/v1/models",
        "/usr/bin/curl -fsS --max-time 20 https://registry.npmjs.org/-/ping",
    ] {
        let result = RunCommand.call(&ctx, &json!({"command":command})).await.unwrap();
        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.meta["network_policy"]["decisions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["decision"] == "allow")
        );
        if command.contains("LAN_HTTP_STATUS") {
            assert!(
                result.content.contains("LAN_HTTP_STATUS:401")
                    || result.content.contains("LAN_HTTP_STATUS:200"),
                "{}",
                result.content
            );
        }
    }
}
