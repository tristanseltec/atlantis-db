//! Edge cases of expression evaluation that the evaluator handles by moving
//! values out of temporaries instead of cloning them.

use atlantis_db::{Engine, Value};

async fn one(e: &Engine, q: &str) -> Value {
    let rs = e
        .execute(q, &[])
        .await
        .unwrap_or_else(|err| panic!("{q}: {err}"));
    let v = rs.rows().next().and_then(|r| r.get("v").cloned());
    v.unwrap_or(Value::Null)
}

fn ints(xs: &[i64]) -> Value {
    Value::Array(xs.iter().map(|x| Value::Integer(*x)).collect())
}

#[tokio::test]
async fn list_comprehension_index_slice_and_properties() {
    let e = Engine::open_in_memory().await.unwrap();
    assert_eq!(
        one(&e, "RETURN [x IN [1, 2, 3, 4] WHERE x > 1] AS v").await,
        ints(&[2, 3, 4])
    );
    assert_eq!(
        one(&e, "RETURN [x IN [1, 2, 3] | x * 10] AS v").await,
        ints(&[10, 20, 30])
    );
    // The comprehension variable shadows an outer one, which survives.
    assert_eq!(
        one(
            &e,
            "WITH 7 AS x RETURN [x IN [1, 2] WHERE x = 2] + [x] AS v"
        )
        .await,
        ints(&[2, 7])
    );
    assert_eq!(one(&e, "RETURN [1, 2, 3][0] AS v").await, Value::Integer(1));
    assert_eq!(
        one(&e, "RETURN [1, 2, 3][-1] AS v").await,
        Value::Integer(3)
    );
    assert_eq!(one(&e, "RETURN [1, 2, 3][5] AS v").await, Value::Null);
    assert_eq!(
        one(&e, "WITH [1, 2, 3] AS l RETURN [l[0], l[2], l[1]] AS v").await,
        ints(&[1, 3, 2])
    );
    assert_eq!(
        one(&e, "RETURN [1, 2, 3, 4, 5][1..3] AS v").await,
        ints(&[2, 3])
    );
    assert_eq!(
        one(&e, "RETURN [1, 2, 3, 4, 5][-2..] AS v").await,
        ints(&[4, 5])
    );
    assert_eq!(one(&e, "RETURN [1, 2, 3][..-1] AS v").await, ints(&[1, 2]));
    assert_eq!(one(&e, "RETURN 'héllo'[1] AS v").await, Value::from("é"));
    assert_eq!(one(&e, "RETURN 'abc'[-1] AS v").await, Value::from("c"));
    assert_eq!(
        one(&e, "WITH {a: {b: 5}} AS m RETURN m.a.b AS v").await,
        Value::Integer(5)
    );
    assert_eq!(
        one(&e, "WITH {a: 1} AS m RETURN [m.a, m['a'], m.a] AS v").await,
        ints(&[1, 1, 1])
    );
    e.execute("CREATE (:P {name: 'n', tags: ['a', 'b']})", &[])
        .await
        .unwrap();
    assert_eq!(
        one(&e, "MATCH (p:P) RETURN [p.tags[1], p.name, p.tags[0]] AS v").await,
        Value::Array(vec![Value::from("b"), Value::from("n"), Value::from("a")])
    );
    // Two output columns with the same source.
    let rs = e
        .execute("WITH 1 AS n RETURN *, n AS m", &[])
        .await
        .unwrap();
    let row = rs.rows().next().unwrap();
    assert_eq!(row.get("n"), Some(&Value::Integer(1)));
    assert_eq!(row.get("m"), Some(&Value::Integer(1)));
}
