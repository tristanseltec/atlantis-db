//! End-to-end tests of the record model through the public API.

use atlantis_db::{Direction, Engine, Error, Object, RecordId, RecordKey, TableKind, Value};

fn obj(pairs: &[(&str, Value)]) -> Object {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn id(s: &str) -> RecordId {
    s.parse().unwrap()
}

#[tokio::test]
async fn records_survive_checkpoint_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path()).await.unwrap();
        engine
            .transaction(|tx| {
                tx.define_table(
                    "WORKS_AT",
                    TableKind::Edge {
                        from: vec!["Person".into()],
                        to: vec!["Company".into()],
                    },
                )?;
                let address = obj(&[("city", "Hue".into())]);
                let alice = tx.create_node(
                    &["Person", "Admin"],
                    obj(&[("id", "alice".into()), ("address", Value::Object(address))]),
                )?;
                let acme = tx.create_node(&["Company"], obj(&[("id", "acme".into())]))?;
                tx.relate(
                    &alice.id,
                    "WORKS_AT",
                    &acme.id,
                    obj(&[("since", 2021i64.into())]),
                )?;
                Ok(())
            })
            .await
            .unwrap();
        engine.shutdown().await.unwrap();
    }

    let engine = Engine::open(dir.path()).await.unwrap();
    let (city, employers, admins) = engine
        .read(|tx| {
            let alice = tx.get(&id("Person:alice"))?.expect("alice persisted");
            let city = Value::Object(alice.data.clone())
                .get_path("address.city")
                .cloned();
            let employers = tx.neighbors(&alice.id, Direction::Outgoing, Some("WORKS_AT"))?;
            let admins = tx.with_label("Admin")?.len();
            Ok((city, employers, admins))
        })
        .await
        .unwrap();
    assert_eq!(city, Some(Value::from("Hue")));
    assert_eq!(employers, vec![id("Company:acme")]);
    assert_eq!(admins, 1);
}

#[tokio::test]
async fn failed_transactions_write_nothing() {
    let engine = Engine::open_in_memory().await.unwrap();
    let result: Result<(), Error> = engine
        .transaction(|tx| {
            tx.create("Person", Some(RecordKey::Integer(1)), vec![], Object::new())?;
            Err(Error::InvalidQuery("abort".into()))
        })
        .await;
    assert!(result.is_err());
    let found = engine.read(|tx| tx.get(&id("Person:1"))).await.unwrap();
    assert!(found.is_none());
}

#[tokio::test]
async fn read_transactions_reject_writes() {
    let engine = Engine::open_in_memory().await.unwrap();
    let err = engine
        .read(|tx| tx.create("Person", None, vec![], Object::new()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::TxReadOnly));
}

#[tokio::test]
async fn databases_are_selected_per_handle() {
    let engine = Engine::open_in_memory().await.unwrap();
    let mut shop = engine.clone();
    shop.use_db("acme", "shop").unwrap();
    shop.transaction(|tx| {
        tx.create(
            "Product",
            Some(RecordKey::Integer(1)),
            vec![],
            Object::new(),
        )
    })
    .await
    .unwrap();
    assert_eq!(shop.database().to_string(), "acme.shop");
    assert!(engine
        .read(|tx| tx.get(&id("Product:1")))
        .await
        .unwrap()
        .is_none());
    assert!(shop
        .read(|tx| tx.get(&id("Product:1")))
        .await
        .unwrap()
        .is_some());
    assert!(shop.clone().use_db("bad name", "x").is_err());
}

#[tokio::test]
async fn cypher_engine_still_works_alongside() {
    let engine = Engine::open_in_memory().await.unwrap();
    engine
        .execute("CREATE (n:Person {name: 'Alice'})", &[])
        .await
        .unwrap();
    let rs = engine
        .execute("MATCH (n:Person) RETURN n.name AS name", &[])
        .await
        .unwrap();
    assert_eq!(rs.row_count(), 1);
}

#[tokio::test]
async fn dropping_the_last_handle_checkpoints_and_releases_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    {
        let engine = Engine::open(dir.path()).await.unwrap();
        let err = Engine::open(dir.path())
            .await
            .err()
            .expect("directory is locked");
        assert!(err.to_string().contains("already open"), "{err}");
        let clone = engine.clone();
        drop(engine);
        clone
            .transaction(|tx| tx.create("Note", Some(RecordKey::Integer(1)), vec![], Object::new()))
            .await
            .unwrap();
        // No explicit checkpoint or shutdown: dropping the last clone persists.
    }
    let engine = Engine::open(dir.path()).await.unwrap();
    assert!(engine
        .read(|tx| tx.get(&id("Note:1")))
        .await
        .unwrap()
        .is_some());
}
