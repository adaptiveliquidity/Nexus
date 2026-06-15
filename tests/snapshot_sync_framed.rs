use nexus::snapshot::manager::{ExecutionState, FilesystemDiff, Snapshot, SnapshotMetadata};
use nexus::snapshot::sync::{
    replicate_framed, FramedSyncTransport, SyncAuthConfig, SyncMessage, SyncNode,
};

fn snap(mem: &[u8], op: &str) -> Snapshot {
    Snapshot::new(
        mem.to_vec(),
        FilesystemDiff::new(),
        ExecutionState::default(),
        SnapshotMetadata::new(op.into(), "input".into()),
    )
    .unwrap()
}

#[tokio::test]
async fn framed_loopback_replicates_snapshot() {
    let mut a = SyncNode::new();
    let mut b = SyncNode::new();
    let digest = a.try_insert(snap(b"phase3-frame", "replicate")).unwrap();

    let auth = SyncAuthConfig::new([0x42; 32]);
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (client_reader, client_writer) = tokio::io::split(client);
    let (server_reader, server_writer) = tokio::io::split(server);

    let (initiator, acceptor) = tokio::join!(
        FramedSyncTransport::connect_initiator(client_reader, client_writer, auth.clone()),
        FramedSyncTransport::accept(server_reader, server_writer, auth)
    );
    let mut ta = initiator.unwrap();
    let mut tb = acceptor.unwrap();

    replicate_framed(&mut a, &mut ta, &mut b, &mut tb, 20)
        .await
        .unwrap();

    assert!(b.has(&digest));
}

#[tokio::test]
async fn framed_transport_rejects_wrong_node_key() {
    let auth_a = SyncAuthConfig::new([0x42; 32]);
    let auth_b = SyncAuthConfig::new([0x24; 32]);
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (client_reader, client_writer) = tokio::io::split(client);
    let (server_reader, server_writer) = tokio::io::split(server);

    let (initiator, acceptor) = tokio::join!(
        FramedSyncTransport::connect_initiator(client_reader, client_writer, auth_a),
        FramedSyncTransport::accept(server_reader, server_writer, auth_b)
    );
    let mut initiator = initiator.unwrap();
    let mut acceptor = acceptor.unwrap();

    let digest = nexus::snapshot::sync::digest_of(&snap(b"x", "wrong-key")).unwrap();
    initiator
        .send(SyncMessage::Ack { digest })
        .await
        .expect("sender can write a frame with its own key");

    assert!(acceptor.recv().await.is_err());
}
