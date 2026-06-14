//! Integration test: verify named-pipe transport on Windows.
//!
//! On non-Windows platforms this file compiles but all tests are no-ops
//! (gated with `#[cfg(windows)]`).

#[cfg(windows)]
mod windows_pipe {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

    use nexus::daemon::protocol::{read_frame, write_frame};
    use nexus::daemon::{DaemonRequest, DaemonResponse};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_pipe_name() -> String {
        let id = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!(r"\\.\pipe\nexus-agentd-test-{id}-{seq}")
    }

    #[tokio::test]
    async fn ping_pong_over_named_pipe() {
        let pipe_name = unique_pipe_name();
        let server_name = pipe_name.clone();

        let server_handle = tokio::spawn(async move {
            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&server_name)
                .unwrap();
            server.connect().await.unwrap();

            let (rd, wr) = tokio::io::split(server);
            let mut rd = tokio::io::BufReader::new(rd);
            let mut wr = tokio::io::BufWriter::new(wr);

            let req: DaemonRequest = read_frame(&mut rd).await.unwrap();
            assert!(matches!(req, DaemonRequest::Ping));

            let resp = DaemonResponse::Pong {
                version: "test".into(),
            };
            write_frame(&mut wr, &resp).await.unwrap();
        });

        let client = loop {
            match ClientOptions::new().open(&pipe_name) {
                Ok(c) => break c,
                Err(e) if e.raw_os_error() == Some(231) || e.raw_os_error() == Some(2) => {
                    // ERROR_PIPE_BUSY (231) or ERROR_FILE_NOT_FOUND (2) — retry
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(e) => panic!("client connect failed: {e}"),
            }
        };

        let (rd, wr) = tokio::io::split(client);
        let mut rd = tokio::io::BufReader::new(rd);
        let mut wr = tokio::io::BufWriter::new(wr);

        write_frame(&mut wr, &DaemonRequest::Ping).await.unwrap();
        let resp: DaemonResponse = read_frame(&mut rd).await.unwrap();

        match resp {
            DaemonResponse::Pong { version } => assert_eq!(version, "test"),
            other => panic!("unexpected response: {other:?}"),
        }

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn multiple_sequential_requests() {
        let pipe_name = unique_pipe_name();
        let server_name = pipe_name.clone();

        let server_handle = tokio::spawn(async move {
            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&server_name)
                .unwrap();
            server.connect().await.unwrap();

            let (rd, wr) = tokio::io::split(server);
            let mut rd = tokio::io::BufReader::new(rd);
            let mut wr = tokio::io::BufWriter::new(wr);

            for _ in 0..3 {
                let req: DaemonRequest = read_frame(&mut rd).await.unwrap();
                assert!(matches!(req, DaemonRequest::Ping));
                let resp = DaemonResponse::Pong {
                    version: "v".into(),
                };
                write_frame(&mut wr, &resp).await.unwrap();
            }
        });

        let client = loop {
            match ClientOptions::new().open(&pipe_name) {
                Ok(c) => break c,
                Err(e) if e.raw_os_error() == Some(231) || e.raw_os_error() == Some(2) => {
                    // ERROR_PIPE_BUSY (231) or ERROR_FILE_NOT_FOUND (2) — retry
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(e) => panic!("client connect failed: {e}"),
            }
        };

        let (rd, wr) = tokio::io::split(client);
        let mut rd = tokio::io::BufReader::new(rd);
        let mut wr = tokio::io::BufWriter::new(wr);

        for _ in 0..3 {
            write_frame(&mut wr, &DaemonRequest::Ping).await.unwrap();
            let resp: DaemonResponse = read_frame(&mut rd).await.unwrap();
            assert!(matches!(resp, DaemonResponse::Pong { .. }));
        }

        server_handle.await.unwrap();
    }
}
