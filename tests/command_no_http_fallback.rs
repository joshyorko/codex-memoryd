#[cfg(target_os = "linux")]
#[test]
fn configured_command_does_not_also_call_http_endpoint() {
    use codex_memoryd::protocol::ConclusionsRequest;
    use codex_memoryd::{config::Config, service::Service, store::Store};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::{Duration, Instant},
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let observer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut buffer = [0u8; 4096];
                    let _ = stream.read(&mut buffer);
                    let body = r#"{"choices":[{"message":{"content":"[]"}}]}"#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    return true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => panic!("listener error: {e}"),
            }
        }
        false
    });
    let mut config = Config::default();
    config.default_workspace = "ws".into();
    config.dream_scheduler.enabled = true;
    config.dream_scheduler.scheduled_provider_enabled = true;
    config.dream_scheduler.idle_window_seconds = 0;
    config.dream_scheduler.min_session_age_seconds = 0;
    config.dream_provider.enabled = true;
    config.dream_provider.adapter = "command".into();
    config.dream_provider.endpoint = endpoint;
    config.dream_provider.model = "synthetic-model".into();
    config.dream_provider.command = vec!["/bin/sh".into(), "-c".into(), r#"cat >/dev/null; printf '{"schema_version":"dream-preview-v1","profile":"personal","workspace":"ws","candidates":[]}'"#.into()];
    let svc = Service::new(Store::open(":memory:").unwrap(), config);
    let req: ConclusionsRequest = serde_json::from_value(serde_json::json!({"profile":"personal","workspace":"ws","conclusions":["Preference: concise updates"]})).unwrap();
    svc.conclusions(req).unwrap();
    assert_eq!(svc.scheduled_dream(None).unwrap().status, "ok");
    assert!(
        !observer.join().unwrap(),
        "command mode invoked the HTTP provider"
    );
}
