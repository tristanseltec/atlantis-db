//! Tests for the record and edge store.

use std::sync::Arc;

use atlantis_core::{
    AtlantisError, DatabaseRef, Direction, DistanceMetricKind, Object, RecordId, RecordKey, Value,
    DEFAULT_TABLE,
};
use atlantis_kv::{Isolation, MemStore, TxMode};

use super::{RecordStore, RecordTx, TableKind, VectorIndexDef};

fn store() -> RecordStore {
    RecordStore::new(Arc::new(MemStore::new()))
}

fn tx(store: &RecordStore) -> RecordTx {
    store
        .begin(
            DatabaseRef::default(),
            TxMode::ReadWrite,
            Isolation::Snapshot,
        )
        .unwrap()
}

fn obj(pairs: &[(&str, Value)]) -> Object {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn id(s: &str) -> RecordId {
    s.parse().unwrap()
}

#[test]
fn create_get_update_merge_delete() {
    let s = store();
    let mut t = tx(&s);
    let rec = t
        .create(
            "Person",
            None,
            vec![],
            obj(&[("id", "alice".into()), ("age", 30i64.into())]),
        )
        .unwrap();
    assert_eq!(rec.id, id("Person:alice"));
    assert!(
        !rec.data.contains_key("id"),
        "the id lives in the record id, not the document"
    );
    assert!(matches!(
        t.create(
            "Person",
            Some(RecordKey::String("alice".into())),
            vec![],
            Object::new()
        ),
        Err(AtlantisError::RecordExists(_))
    ));

    let merged = t
        .merge(
            &rec.id,
            obj(&[("city", "Hue".into()), ("age", Value::Null)]),
        )
        .unwrap();
    assert_eq!(merged.get("city"), Some(&Value::from("Hue")));
    assert_eq!(merged.get("age"), None);
    let replaced = t.update(&rec.id, obj(&[("name", "Alice".into())])).unwrap();
    assert_eq!(replaced.data.len(), 1);
    assert!(t
        .update(&rec.id, obj(&[("id", Value::RecordId(id("Person:bob")))]))
        .is_err());
    t.commit().unwrap();

    let mut t = tx(&s);
    assert_eq!(
        t.get(&rec.id).unwrap().unwrap().get("name"),
        Some(&Value::from("Alice"))
    );
    assert!(t.delete(&rec.id, false).unwrap());
    assert!(!t.delete(&rec.id, false).unwrap());
    assert!(matches!(
        t.merge(&rec.id, Object::new()),
        Err(AtlantisError::RecordNotFound(_))
    ));
}

#[test]
fn generated_keys_and_default_table() {
    let s = store();
    let mut t = tx(&s);
    let a = t.create_node(&[], Object::new()).unwrap();
    assert_eq!(a.id.table, DEFAULT_TABLE);
    assert!(matches!(a.id.key, RecordKey::Ulid(_)));
    assert!(
        t.create("_secret", None, vec![], Object::new()).is_err(),
        "reserved table names"
    );
    let b = t.create_node(&[], Object::new()).unwrap();
    let ids: Vec<RecordId> = t
        .scan(DEFAULT_TABLE, None)
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, vec![a.id, b.id], "ULID keys keep creation order");
}

#[test]
fn nodes_labels_and_edges() {
    let s = store();
    let mut t = tx(&s);
    let alice = t
        .create_node(&["Person", "Admin"], obj(&[("id", "alice".into())]))
        .unwrap();
    let bob = t
        .create_node(&["Person"], obj(&[("id", "bob".into())]))
        .unwrap();
    let acme = t
        .create_node(&["Company"], obj(&[("id", "acme".into())]))
        .unwrap();
    assert_eq!(alice.labels, vec!["Admin".to_string()]);
    assert_eq!(t.with_label("Admin").unwrap().len(), 1);
    assert_eq!(t.with_label("Person").unwrap().len(), 2);

    let knows = t
        .relate(
            &alice.id,
            "KNOWS",
            &bob.id,
            obj(&[("since", 2020i64.into())]),
        )
        .unwrap();
    t.relate(&alice.id, "WORKS_AT", &acme.id, Object::new())
        .unwrap();
    t.commit().unwrap();

    let mut t = tx(&s);
    assert_eq!(
        t.neighbors(&alice.id, Direction::Outgoing, None)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        t.neighbors(&alice.id, Direction::Outgoing, Some("KNOWS"))
            .unwrap(),
        vec![bob.id.clone()]
    );
    assert_eq!(
        t.neighbors(&bob.id, Direction::Incoming, None).unwrap(),
        vec![alice.id.clone()]
    );
    assert!(t
        .neighbors(&bob.id, Direction::Outgoing, None)
        .unwrap()
        .is_empty());
    let as_record = t.get(&knows.id).unwrap().unwrap();
    assert_eq!(
        as_record.get("in"),
        Some(&Value::RecordId(alice.id.clone()))
    );
    assert_eq!(as_record.get("since"), Some(&Value::Integer(2020)));
    assert!(t.table("KNOWS").unwrap().unwrap().is_edge());
    assert!(
        t.create("KNOWS", None, vec![], Object::new()).is_err(),
        "edge tables hold only edges"
    );
}

#[test]
fn delete_requires_detach_when_edges_exist() {
    let s = store();
    let mut t = tx(&s);
    let a = t.create_node(&["P"], Object::new()).unwrap();
    let b = t.create_node(&["P"], Object::new()).unwrap();
    let e = t.relate(&a.id, "R", &b.id, Object::new()).unwrap();
    t.relate(&a.id, "R", &a.id, Object::new()).unwrap(); // self-loop
    assert_eq!(
        t.edges(&a.id, Direction::Both, None).unwrap().len(),
        2,
        "self-loop counted once"
    );
    assert!(matches!(
        t.delete(&a.id, false),
        Err(AtlantisError::SchemaViolation(_))
    ));
    assert!(t.delete(&a.id, true).unwrap());
    assert!(t.get_edge(&e.id).unwrap().is_none());
    assert!(
        t.edges(&b.id, Direction::Both, None).unwrap().is_empty(),
        "no dangling adjacency"
    );
}

#[test]
fn edge_endpoint_rules() {
    let s = store();
    let mut t = tx(&s);
    t.define_table(
        "WORKS_AT",
        TableKind::Edge {
            from: vec!["Person".into()],
            to: vec!["Company".into()],
        },
    )
    .unwrap();
    let p = t.create_node(&["Person"], Object::new()).unwrap();
    let c = t.create_node(&["Company"], Object::new()).unwrap();
    assert!(t.relate(&p.id, "WORKS_AT", &c.id, Object::new()).is_ok());
    assert!(matches!(
        t.relate(&c.id, "WORKS_AT", &p.id, Object::new()),
        Err(AtlantisError::SchemaViolation(_))
    ));
    assert!(matches!(
        t.relate(&p.id, "WORKS_AT", &id("Company:ghost"), Object::new()),
        Err(AtlantisError::RecordNotFound(_))
    ));
    assert!(t.define_table("WORKS_AT", TableKind::Normal).is_err());
    let names: Vec<String> = t.tables().unwrap().into_iter().map(|d| d.name).collect();
    assert_eq!(names, vec!["Company", "Person", "WORKS_AT"]);
}

#[test]
fn databases_are_isolated() {
    let s = store();
    let mut t = tx(&s);
    t.create("Person", Some(RecordKey::Integer(1)), vec![], Object::new())
        .unwrap();
    t.commit().unwrap();
    let other = DatabaseRef::new("acme", "shop").unwrap();
    let mut t = s
        .begin(other, TxMode::ReadOnly, Isolation::Snapshot)
        .unwrap();
    assert!(t.get(&id("Person:1")).unwrap().is_none());
    assert!(t.tables().unwrap().is_empty());
}

#[test]
fn composite_keys_support_time_range_scans() {
    let s = store();
    let mut t = tx(&s);
    for (sensor, hour) in [("s1", 1i64), ("s1", 2), ("s1", 3), ("s2", 1)] {
        let key = RecordKey::Array(vec![sensor.into(), hour.into()]);
        t.create("Reading", Some(key), vec![], obj(&[("v", hour.into())]))
            .unwrap();
    }
    let from = RecordKey::Array(vec!["s1".into(), 2i64.into()]);
    let to = RecordKey::Array(vec!["s1".into(), 99i64.into()]);
    let hits = t.scan_range("Reading", &from, &to, None).unwrap();
    let hours: Vec<&Value> = hits.iter().map(|r| r.get("v").unwrap()).collect();
    assert_eq!(hours, vec![&Value::Integer(2), &Value::Integer(3)]);
}

#[test]
fn concurrent_creates_of_the_same_id_conflict() {
    let s = store();
    let mut a = tx(&s);
    let mut b = tx(&s);
    a.create("Person", Some(RecordKey::Integer(1)), vec![], Object::new())
        .unwrap();
    b.create("Person", Some(RecordKey::Integer(1)), vec![], Object::new())
        .unwrap();
    a.commit().unwrap();
    assert!(b.commit().unwrap_err().is_retryable());
}

#[test]
fn concurrent_relate_and_delete_cannot_leave_a_dangling_edge() {
    let s = store();
    let mut setup = tx(&s);
    let a = setup
        .create_node(&["P"], obj(&[("id", "a".into())]))
        .unwrap();
    let b = setup
        .create_node(&["P"], obj(&[("id", "b".into())]))
        .unwrap();
    setup.commit().unwrap();

    let mut relate = tx(&s);
    let mut delete = tx(&s);
    relate.relate(&a.id, "R", &b.id, Object::new()).unwrap();
    assert!(
        delete.delete(&a.id, false).unwrap(),
        "sees no edges in its snapshot"
    );
    relate.commit().unwrap();
    assert!(delete.commit().unwrap_err().is_retryable());

    let mut check = tx(&s);
    assert!(check.get(&a.id).unwrap().is_some());
    assert_eq!(
        check.neighbors(&b.id, Direction::Incoming, None).unwrap(),
        vec![a.id]
    );
}

#[test]
fn edges_cannot_be_endpoints() {
    let s = store();
    let mut t = tx(&s);
    let x = t.create_node(&["Q"], Object::new()).unwrap();
    let y = t.create_node(&["Q"], Object::new()).unwrap();
    let e = t.relate(&x.id, "S", &y.id, Object::new()).unwrap();
    assert!(matches!(
        t.relate(&e.id, "T", &y.id, Object::new()),
        Err(AtlantisError::SchemaViolation(_))
    ));
    assert!(matches!(
        t.relate(&y.id, "T", &e.id, Object::new()),
        Err(AtlantisError::SchemaViolation(_))
    ));
}

#[test]
fn labels_can_be_added_and_removed() {
    let s = store();
    let mut t = tx(&s);
    let n = t.create_node(&["Person"], Object::new()).unwrap();
    let n = t
        .set_labels(&n.id, &["Admin".into(), "Person".into()], &[])
        .unwrap();
    assert_eq!(n.labels, vec!["Admin".to_string()]);
    assert_eq!(t.with_label("Admin").unwrap().len(), 1);
    t.set_labels(&n.id, &[], &["Admin".into()]).unwrap();
    assert!(
        t.with_label("Admin").unwrap().is_empty(),
        "label index entry removed"
    );
    assert!(
        t.set_labels(&n.id, &[], &["Person".into()]).is_err(),
        "table label is fixed"
    );
}

#[test]
fn vector_index_definitions_live_in_the_catalog() {
    let s = store();
    let mut t = tx(&s);
    let def = VectorIndexDef {
        name: "chunk_embedding".into(),
        label: "Chunk".into(),
        field: "embedding".into(),
        dimension: 3,
        metric: DistanceMetricKind::Cosine,
        m: 16,
        ef_construction: 200,
    };
    t.define_vector_index(def.clone()).unwrap();
    assert!(t.define_vector_index(def.clone()).is_err());
    assert_eq!(
        t.vector_index("chunk_embedding").unwrap(),
        Some(def.clone())
    );
    assert_eq!(t.vector_indexes().unwrap(), vec![def]);
    assert!(
        t.tables().unwrap().is_empty(),
        "index definitions are not tables"
    );
    assert!(t.drop_index("chunk_embedding").unwrap());
    assert!(!t.drop_index("chunk_embedding").unwrap());
}

#[test]
fn commit_reports_changed_records_once() {
    let s = store();
    let mut t = tx(&s);
    let a = t.create_node(&["P"], Object::new()).unwrap();
    t.merge(&a.id, obj(&[("x", 1i64.into())])).unwrap();
    let b = t.create_node(&["P"], Object::new()).unwrap();
    t.delete(&b.id, false).unwrap();
    let info = t.commit().unwrap();
    assert_eq!(info.changed.len(), 2);
    assert!(info.changed.contains(&a.id) && info.changed.contains(&b.id));
}
