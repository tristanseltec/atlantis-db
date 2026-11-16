//! Transaction behaviour through the public API: default isolation, group
//! commit with concurrent durable writers, and the sync modes.

use std::time::Duration;

use atlantis_db::{Config, Engine, Error, Isolation, Object, SyncMode, Value};

fn count(rs: &atlantis_db::ResultSet) -> i64 {
    match rs.rows().next().and_then(|r| r.get("c").cloned()) {
        Some(Value::Integer(n)) => n,
        other => panic!("count is {other:?}"),
    }
}

async fn open(dir: &std::path::Path, sync: SyncMode, isolation: Isolation) -> Engine {
    Engine::open_with_config(
        Config::builder()
            .disk_path(dir)
            .sync_mode(sync)
            .default_isolation(isolation)
            .checkpoint_interval(0)
            .build()
            .unwrap(),
    )
    .await
    .unwrap()
}

/// Two "on call" doctors; each transaction takes one off call if the other
/// is still on call. Under snapshot isolation both commit (write skew);
/// under serializable the second fails.
async fn write_skew(engine: &Engine) -> Result<(), Error> {
    engine
        .execute(
            "CREATE (:Doctor {id: 1, on: true}), (:Doctor {id: 2, on: true})",
            &[],
        )
        .await?;
    engine
        .transaction(|tx| {
            let rs = tx.execute("MATCH (d:Doctor {on: true}) RETURN count(d) AS c", &[])?;
            assert_eq!(count(&rs), 2);
            // A competing transaction takes doctor 2 off call meanwhile.
            futures_lite_block(engine.execute("MATCH (d:Doctor {id: 2}) SET d.on = false", &[]))?;
            tx.execute("MATCH (d:Doctor {id: 1}) SET d.on = false", &[])?;
            Ok(())
        })
        .await
}

/// The engine's async methods finish synchronously; poll once.
fn futures_lite_block<F: std::future::Future>(f: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(v) => v,
        std::task::Poll::Pending => panic!("engine call did not complete synchronously"),
    }
}

#[tokio::test]
async fn default_isolation_applies_to_transactions_and_cypher() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = open(dir.path(), SyncMode::Every, Isolation::Snapshot).await;
    write_skew(&snapshot)
        .await
        .expect("snapshot isolation allows write skew");
    drop(snapshot);

    let dir = tempfile::tempdir().unwrap();
    let serializable = open(dir.path(), SyncMode::Every, Isolation::Serializable).await;
    let err = write_skew(&serializable).await.unwrap_err();
    assert!(matches!(err, Error::TxConflict(_)), "{err}");
    let rs = serializable
        .execute("MATCH (d:Doctor {on: true}) RETURN count(d) AS c", &[])
        .await
        .unwrap();
    assert_eq!(count(&rs), 1, "one doctor stays on call");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_durable_writers_all_commit_and_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let writers = 8;
    let per_writer = 150;
    {
        let engine = open(dir.path(), SyncMode::Every, Isolation::Snapshot).await;
        // The first write registers the namespace, database and table in
        // the catalog; concurrent first writes would conflict on those keys.
        engine
            .execute("CREATE (:Item {writer: -1})", &[])
            .await
            .unwrap();
        let tasks: Vec<_> = (0..writers)
            .map(|w| {
                let engine = engine.clone();
                tokio::spawn(async move {
                    for i in 0..per_writer {
                        engine
                            .execute(
                                "CREATE (:Item {writer: $w, i: $i})",
                                &[("w", Value::Integer(w)), ("i", Value::Integer(i))],
                            )
                            .await
                            .unwrap();
                    }
                })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        let rs = engine
            .execute("MATCH (n:Item) RETURN count(n) AS c", &[])
            .await
            .unwrap();
        assert_eq!(count(&rs), writers * per_writer + 1);
    }
    let engine = open(dir.path(), SyncMode::Every, Isolation::Snapshot).await;
    let rs = engine
        .execute("MATCH (n:Item) RETURN count(n) AS c", &[])
        .await
        .unwrap();
    assert_eq!(count(&rs), writers * per_writer + 1);
}

#[tokio::test]
async fn interval_sync_mode_persists_commits() {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = open(
            dir.path(),
            SyncMode::Interval(Duration::from_millis(10)),
            Isolation::Snapshot,
        )
        .await;
        engine
            .transaction(|tx| {
                for i in 0..50 {
                    let mut o = Object::new();
                    o.insert("i".into(), Value::Integer(i));
                    tx.create("Item", None, vec![], o)?;
                }
                Ok(())
            })
            .await
            .unwrap();
        // Visible at once, without waiting for the background sync.
        let rs = engine
            .execute("MATCH (n:Item) RETURN count(n) AS c", &[])
            .await
            .unwrap();
        assert_eq!(count(&rs), 50);
    }
    let engine = open(dir.path(), SyncMode::Every, Isolation::Snapshot).await;
    let rs = engine
        .execute("MATCH (n:Item) RETURN count(n) AS c", &[])
        .await
        .unwrap();
    assert_eq!(count(&rs), 50);
}

#[test]
fn zero_sync_interval_is_rejected() {
    let err = Config::builder()
        .sync_mode(SyncMode::Interval(Duration::ZERO))
        .build()
        .unwrap_err();
    assert!(matches!(err, Error::Config(_)), "{err}");
}
