//! Tests for property indexes and constraints.

use std::sync::Arc;

use atlantis_core::{AtlantisError, DatabaseRef, DistanceMetricKind, Object, RecordId, Value};
use atlantis_kv::{Isolation, MemStore, TxMode};

use super::{ConstraintDef, ConstraintKind, IndexInfo, IndexLookup, PropertyIndexDef};
use crate::records::{RecordStore, RecordTx, VectorIndexDef};

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

fn path(p: &str) -> Vec<String> {
    p.split('.').map(String::from).collect()
}

fn index(name: &str, label: &str, fields: &[&str]) -> PropertyIndexDef {
    PropertyIndexDef {
        name: name.into(),
        label: label.into(),
        fields: fields.iter().map(|f| path(f)).collect(),
    }
}

fn constraint(name: &str, label: &str, kind: ConstraintKind, fields: &[&str]) -> ConstraintDef {
    ConstraintDef {
        name: name.into(),
        label: label.into(),
        kind,
        fields: fields.iter().map(|f| path(f)).collect(),
    }
}

fn info(t: &mut RecordTx, name: &str) -> IndexInfo {
    t.schema_def(name).unwrap().unwrap().index_info().unwrap()
}

/// Keys of the records an equality lookup returns, sorted.
fn eq(t: &mut RecordTx, name: &str, values: &[Value]) -> Vec<String> {
    let ix = info(t, name);
    let mut ids: Vec<String> = t
        .index_lookup(&ix, &IndexLookup::Eq(values.to_vec()))
        .unwrap()
        .expect("lookup should be answerable")
        .iter()
        .map(|id| id.to_string())
        .collect();
    ids.sort();
    ids
}

fn node(t: &mut RecordTx, key: &str, data: &[(&str, Value)]) -> RecordId {
    let mut data = obj(data);
    data.insert("id".into(), Value::from(key));
    t.create_node(&["Person"], data).unwrap().id
}

fn is_violation<T: std::fmt::Debug>(r: Result<T, AtlantisError>) -> bool {
    matches!(r, Err(AtlantisError::ConstraintViolation(_)))
}

#[test]
fn entries_follow_creates_updates_deletes_and_labels() {
    let s = store();
    let mut t = tx(&s);
    t.define_property_index(index("by_email", "Person", &["email"]))
        .unwrap();
    let ana = node(&mut t, "ana", &[("email", "a@x".into())]);
    node(&mut t, "bo", &[("email", "b@x".into())]);
    assert_eq!(eq(&mut t, "by_email", &["a@x".into()]), ["Person:ana"]);

    t.merge(&ana, obj(&[("email", "new@x".into())])).unwrap();
    assert!(eq(&mut t, "by_email", &["a@x".into()]).is_empty());
    assert_eq!(eq(&mut t, "by_email", &["new@x".into()]), ["Person:ana"]);

    t.merge(&ana, obj(&[("email", Value::Null)])).unwrap();
    assert!(eq(&mut t, "by_email", &["new@x".into()]).is_empty());

    t.delete(&"Person:bo".parse().unwrap(), false).unwrap();
    assert!(eq(&mut t, "by_email", &["b@x".into()]).is_empty());

    // A secondary label brings a node into an index on that label.
    t.define_property_index(index("admin_email", "Admin", &["email"]))
        .unwrap();
    let cy = t
        .create_node(&["User"], obj(&[("email", "c@x".into())]))
        .unwrap()
        .id;
    assert!(eq(&mut t, "admin_email", &["c@x".into()]).is_empty());
    t.set_labels(&cy, &["Admin".into()], &[]).unwrap();
    assert_eq!(eq(&mut t, "admin_email", &["c@x".into()]), [cy.to_string()]);
    t.set_labels(&cy, &[], &["Admin".into()]).unwrap();
    assert!(eq(&mut t, "admin_email", &["c@x".into()]).is_empty());
    t.commit().unwrap();
}

#[test]
fn an_index_covers_records_written_before_it() {
    let s = store();
    let mut t = tx(&s);
    node(&mut t, "ana", &[("age", 30.into())]);
    t.commit().unwrap();
    let mut t = tx(&s);
    node(&mut t, "bo", &[("age", 30.into())]);
    t.define_property_index(index("by_age", "Person", &["age"]))
        .unwrap();
    assert_eq!(
        eq(&mut t, "by_age", &[30.into()]),
        ["Person:ana", "Person:bo"]
    );
}

#[test]
fn numbers_match_across_integer_and_float() {
    let s = store();
    let mut t = tx(&s);
    t.define_property_index(index("by_n", "Person", &["n"]))
        .unwrap();
    node(&mut t, "int", &[("n", 1.into())]);
    node(&mut t, "float", &[("n", Value::Float(1.0))]);
    node(&mut t, "other", &[("n", Value::Float(1.5))]);
    assert_eq!(
        eq(&mut t, "by_n", &[Value::Float(1.0)]),
        ["Person:float", "Person:int"]
    );
    assert_eq!(
        eq(&mut t, "by_n", &[1.into()]),
        ["Person:float", "Person:int"]
    );
    // Beyond 2^53 an equality lookup cannot be exact: the caller scans.
    let ix = info(&mut t, "by_n");
    let big = IndexLookup::Eq(vec![Value::Integer((1 << 53) + 1)]);
    assert_eq!(t.index_lookup(&ix, &big).unwrap(), None);
    // `= null` never matches.
    let null = IndexLookup::Eq(vec![Value::Null]);
    assert_eq!(t.index_lookup(&ix, &null).unwrap(), Some(vec![]));
}

#[test]
fn single_field_indexes_are_multikey() {
    let s = store();
    let mut t = tx(&s);
    t.define_property_index(index("by_tag", "Person", &["tags"]))
        .unwrap();
    let tags = |xs: &[&str]| Value::Array(xs.iter().map(|x| Value::from(*x)).collect());
    let ana = node(&mut t, "ana", &[("tags", tags(&["vip", "beta", "vip"]))]);
    node(&mut t, "bo", &[("tags", "vip".into())]);
    assert_eq!(
        eq(&mut t, "by_tag", &["vip".into()]),
        ["Person:ana", "Person:bo"]
    );
    assert_eq!(eq(&mut t, "by_tag", &["beta".into()]), ["Person:ana"]);
    t.merge(&ana, obj(&[("tags", tags(&["vip"]))])).unwrap();
    assert!(eq(&mut t, "by_tag", &["beta".into()]).is_empty());
    // Whole arrays are not entries of a multikey index.
    let ix = info(&mut t, "by_tag");
    let whole = IndexLookup::Eq(vec![tags(&["vip"])]);
    assert_eq!(t.index_lookup(&ix, &whole).unwrap(), None);
}

#[test]
fn composite_and_nested_indexes() {
    let s = store();
    let mut t = tx(&s);
    t.define_property_index(index("name", "Person", &["last", "first"]))
        .unwrap();
    t.define_property_index(index("city", "Person", &["address.city"]))
        .unwrap();
    let addr = |c: &str| Value::Object(obj(&[("city", c.into())]));
    node(
        &mut t,
        "ana",
        &[
            ("last", "Le".into()),
            ("first", "Ana".into()),
            ("address", addr("Hue")),
        ],
    );
    node(
        &mut t,
        "bo",
        &[("last", "Le".into()), ("first", "Bo".into())],
    );
    // Missing a field of a composite index: not indexed.
    node(&mut t, "cy", &[("last", "Le".into())]);
    assert_eq!(
        eq(&mut t, "name", &["Le".into(), "Ana".into()]),
        ["Person:ana"]
    );
    assert_eq!(
        eq(&mut t, "name", &["Le".into()]),
        ["Person:ana", "Person:bo"]
    );
    assert_eq!(eq(&mut t, "city", &["Hue".into()]), ["Person:ana"]);
}

#[test]
fn range_and_prefix_lookups() {
    let s = store();
    let mut t = tx(&s);
    t.define_property_index(index("by_age", "Person", &["age"]))
        .unwrap();
    t.define_property_index(index("by_name", "Person", &["name"]))
        .unwrap();
    for (k, age, name) in [
        ("a", Value::from(17), "Ana"),
        ("b", Value::Float(18.5), "Anh"),
        ("c", Value::from(40), "Bao"),
        ("d", Value::from("old"), "Binh"),
    ] {
        node(&mut t, k, &[("age", age), ("name", name.into())]);
    }
    let ids = |t: &mut RecordTx, ix: &str, l: IndexLookup| {
        let ix = info(t, ix);
        let mut v: Vec<String> = t
            .index_lookup(&ix, &l)
            .unwrap()
            .unwrap()
            .iter()
            .map(|i| i.key.to_string())
            .collect();
        v.sort();
        v
    };
    let range = |lo: Option<Value>, hi: Option<Value>| IndexLookup::Range {
        lower: lo,
        upper: hi,
    };
    assert_eq!(
        ids(&mut t, "by_age", range(Some(18.into()), None)),
        ["b", "c"]
    );
    assert_eq!(
        ids(&mut t, "by_age", range(Some(17.into()), Some(18.into()))),
        ["a"]
    );
    // A number range never returns strings.
    assert_eq!(
        ids(&mut t, "by_age", range(None, Some(100.into()))),
        ["a", "b", "c"]
    );
    assert_eq!(
        ids(&mut t, "by_name", IndexLookup::Prefix("An".into())),
        ["a", "b"]
    );
    assert_eq!(
        ids(&mut t, "by_name", range(Some("B".into()), None)),
        ["c", "d"]
    );
}

#[test]
fn unique_constraints_reject_duplicates_and_leave_no_partial_writes() {
    let s = store();
    let mut t = tx(&s);
    t.define_constraint(constraint(
        "email_uq",
        "Person",
        ConstraintKind::Unique,
        &["email"],
    ))
    .unwrap();
    let ana = node(&mut t, "ana", &[("email", "a@x".into())]);
    let mut dup = obj(&[("email", "a@x".into()), ("id", "bo".into())]);
    assert!(is_violation(t.create_node(&["Person"], dup.clone())));
    assert!(t.get(&"Person:bo".parse().unwrap()).unwrap().is_none());
    // Null and missing values are not unique-checked.
    node(&mut t, "cy", &[]);
    node(&mut t, "dan", &[("email", Value::Null)]);

    let bo = node(&mut t, "bo", &[("email", "b@x".into())]);
    assert!(is_violation(t.merge(&bo, obj(&[("email", "a@x".into())]))));
    // Freeing a value lets another node take it.
    t.merge(&ana, obj(&[("email", "ana@x".into())])).unwrap();
    t.merge(&bo, obj(&[("email", "a@x".into())])).unwrap();
    t.delete(&bo, false).unwrap();
    dup.insert("id".into(), "eve".into());
    t.create_node(&["Person"], dup).unwrap();

    // The constraint index answers lookups.
    assert_eq!(eq(&mut t, "email_uq", &["a@x".into()]), ["Person:eve"]);
    // A value the constraint cannot check is refused.
    let blob = Value::Object(obj(&[("x", 1.into())]));
    let mut bad = obj(&[("email", blob)]);
    bad.insert("id".into(), "fay".into());
    assert!(is_violation(t.create_node(&["Person"], bad)));
    t.commit().unwrap();
}

#[test]
fn constraints_check_existing_data_when_created() {
    let s = store();
    let mut t = tx(&s);
    node(&mut t, "ana", &[("email", "a@x".into())]);
    node(&mut t, "bo", &[("email", "a@x".into())]);
    node(&mut t, "cy", &[]);
    let uq = constraint("uq", "Person", ConstraintKind::Unique, &["email"]);
    assert!(is_violation(t.define_constraint(uq)));
    let nn = constraint("nn", "Person", ConstraintKind::NotNull, &["email"]);
    assert!(is_violation(t.define_constraint(nn)));
    assert!(t.schema().unwrap().is_empty());
}

#[test]
fn not_null_and_node_key() {
    let s = store();
    let mut t = tx(&s);
    t.define_constraint(constraint(
        "name_nn",
        "Person",
        ConstraintKind::NotNull,
        &["name"],
    ))
    .unwrap();
    t.define_constraint(constraint(
        "key",
        "Person",
        ConstraintKind::NodeKey,
        &["org", "no"],
    ))
    .unwrap();
    let full = [
        ("name", Value::from("Ana")),
        ("org", "acme".into()),
        ("no", 1.into()),
    ];
    let ana = node(&mut t, "ana", &full);
    assert!(is_violation(t.create_node(&["Person"], obj(&full[1..]))));
    assert!(is_violation(t.create_node(&["Person"], obj(&full[..2]))));
    assert!(is_violation(t.create_node(&["Person"], obj(&full))));
    assert!(is_violation(t.merge(&ana, obj(&[("name", Value::Null)]))));
    // Other labels are not constrained.
    t.create_node(&["Robot"], Object::new()).unwrap();
}

#[test]
fn dropping_removes_definitions_and_entries() {
    let s = store();
    let mut t = tx(&s);
    t.define_constraint(constraint(
        "uq",
        "Person",
        ConstraintKind::Unique,
        &["email"],
    ))
    .unwrap();
    t.define_property_index(index("ix", "Person", &["email"]))
        .unwrap();
    node(&mut t, "ana", &[("email", "a@x".into())]);
    assert!(t.drop_schema("uq").unwrap().is_some());
    assert!(t.drop_schema("ix").unwrap().is_some());
    assert!(t.drop_schema("ix").unwrap().is_none());
    node(&mut t, "bo", &[("email", "a@x".into())]);
    // Re-creating under the same names starts from the current data.
    t.define_property_index(index("ix", "Person", &["email"]))
        .unwrap();
    assert_eq!(
        eq(&mut t, "ix", &["a@x".into()]),
        ["Person:ana", "Person:bo"]
    );
    let uq = constraint("uq", "Person", ConstraintKind::Unique, &["email"]);
    assert!(is_violation(t.define_constraint(uq)));
}

#[test]
fn names_and_definitions_must_be_distinct() {
    let s = store();
    let mut t = tx(&s);
    t.define_property_index(index("ix", "Person", &["email"]))
        .unwrap();
    assert!(t
        .define_property_index(index("ix", "Person", &["name"]))
        .is_err());
    assert!(t
        .define_property_index(index("other", "Person", &["email"]))
        .is_err());
    assert!(t
        .define_constraint(constraint("ix", "Person", ConstraintKind::Unique, &["x"]))
        .is_err());
    let vector = VectorIndexDef {
        name: "ix".into(),
        label: "Person".into(),
        field: "v".into(),
        dimension: 3,
        metric: DistanceMetricKind::Cosine,
        m: 16,
        ef_construction: 100,
    };
    assert!(t.define_vector_index(vector).is_err());
    assert!(t
        .define_property_index(index("bad", "Person", &["id"]))
        .is_err());
    assert!(t
        .define_constraint(constraint(
            "nn2",
            "Person",
            ConstraintKind::NotNull,
            &["a", "b"]
        ))
        .is_err());
}

#[test]
fn concurrent_claims_of_a_unique_value_conflict() {
    let s = store();
    let mut setup = tx(&s);
    setup
        .define_constraint(constraint(
            "uq",
            "Person",
            ConstraintKind::Unique,
            &["email"],
        ))
        .unwrap();
    setup.commit().unwrap();
    let mut a = tx(&s);
    let mut b = tx(&s);
    node(&mut a, "ana", &[("email", "x@x".into())]);
    node(&mut b, "bo", &[("email", "x@x".into())]);
    a.commit().unwrap();
    assert!(matches!(b.commit(), Err(AtlantisError::TxConflict(_))));
}

#[test]
fn plain_writers_do_not_conflict_over_the_schema_version() {
    let s = store();
    let mut a = tx(&s);
    let mut b = tx(&s);
    node(&mut a, "ana", &[]);
    node(&mut b, "bo", &[]);
    a.commit().unwrap();
    b.commit().unwrap();
}

#[test]
fn a_writer_cannot_miss_an_index_created_concurrently() {
    // Index first, then the writer whose snapshot predates it.
    let s = store();
    let mut writer = tx(&s);
    let mut ddl = tx(&s);
    ddl.define_property_index(index("ix", "Person", &["email"]))
        .unwrap();
    ddl.commit().unwrap();
    node(&mut writer, "ana", &[("email", "a@x".into())]);
    assert!(matches!(writer.commit(), Err(AtlantisError::TxConflict(_))));

    // Writer first, then an index built from a snapshot that misses it.
    let mut ddl = tx(&s);
    let mut writer = tx(&s);
    node(&mut writer, "bo", &[("nick", "b".into())]);
    ddl.define_property_index(index("ix2", "Person", &["nick"]))
        .unwrap();
    writer.commit().unwrap();
    assert!(matches!(ddl.commit(), Err(AtlantisError::TxConflict(_))));

    // A retry sees everything.
    let mut t = tx(&s);
    t.define_property_index(index("ix2", "Person", &["nick"]))
        .unwrap();
    assert_eq!(eq(&mut t, "ix2", &["b".into()]), ["Person:bo"]);
}

#[test]
fn read_only_transactions_can_read_the_schema() {
    let s = store();
    let mut t = s
        .begin(
            DatabaseRef::default(),
            TxMode::ReadOnly,
            Isolation::Snapshot,
        )
        .unwrap();
    assert!(t.indexes_for("Person").unwrap().is_empty());
    assert!(t.scan("Person", None).unwrap().is_empty());
}
