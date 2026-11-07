use atlantis_core::Value;
use proptest::prelude::*;

use super::{bin, expr, int, parse_err, prop, string, var};
use crate::ast::{BinaryOp::*, Expression, UnaryOp};

fn lit(v: Value) -> Expression {
    Expression::Literal(v)
}

fn unary(op: UnaryOp, e: Expression) -> Expression {
    Expression::Unary(op, Box::new(e))
}

fn func(name: &str, distinct: bool, args: Vec<Expression>) -> Expression {
    Expression::Function {
        name: name.into(),
        distinct,
        args,
    }
}

#[test]
fn literals() {
    assert_eq!(expr("42"), int(42));
    assert_eq!(expr("1e3"), lit(Value::Float(1000.0)));
    assert_eq!(expr(".5"), lit(Value::Float(0.5)));
    assert_eq!(expr("-2.5"), lit(Value::Float(-2.5)));
    assert_eq!(expr("-7"), int(-7));
    assert_eq!(expr("-9223372036854775808"), int(i64::MIN));
    assert_eq!(expr("'it\\'s'"), string("it's"));
    assert_eq!(expr("\"a\\u0041\""), string("aA"));
    assert_eq!(expr("TRUE"), lit(Value::Bool(true)));
    assert_eq!(expr("false"), lit(Value::Bool(false)));
    assert_eq!(expr("Null"), lit(Value::Null));
    assert_eq!(expr("$param"), Expression::Parameter("param".into()));
}

#[test]
fn integer_overflow_is_an_error() {
    assert!(parse_err("RETURN 9223372036854775808").contains("integer literal out of range"));
    assert!(parse_err("RETURN 99999999999999999999").contains("integer literal out of range"));
}

#[test]
fn lists_and_maps() {
    assert_eq!(expr("[]"), Expression::List(vec![]));
    assert_eq!(
        expr("[1, 'a', [2]]"),
        Expression::List(vec![int(1), string("a"), Expression::List(vec![int(2)])])
    );
    assert_eq!(expr("{}"), Expression::Map(vec![]));
    assert_eq!(
        expr("{name: 'x', type: 2, `odd key`: n}"),
        Expression::Map(vec![
            ("name".into(), string("x")),
            ("type".into(), int(2)),
            ("odd key".into(), var("n"))
        ])
    );
}

#[test]
fn property_index_and_slice() {
    assert_eq!(expr("n.a.b"), prop(prop(var("n"), "a"), "b"));
    assert_eq!(expr("n.order"), prop(var("n"), "order"));
    assert_eq!(expr("n.type"), prop(var("n"), "type"));
    assert_eq!(
        expr("l[0]"),
        Expression::Index(Box::new(var("l")), Box::new(int(0)))
    );
    assert_eq!(
        expr("l[-1]"),
        Expression::Index(Box::new(var("l")), Box::new(int(-1)))
    );
    let slice = |from: Option<i64>, to: Option<i64>| {
        Expression::Slice(
            Box::new(var("l")),
            from.map(|n| Box::new(int(n))),
            to.map(|n| Box::new(int(n))),
        )
    };
    assert_eq!(expr("l[1..3]"), slice(Some(1), Some(3)));
    assert_eq!(expr("l[..3]"), slice(None, Some(3)));
    assert_eq!(expr("l[2..]"), slice(Some(2), None));
    assert_eq!(expr("l[..]"), slice(None, None));
    assert_eq!(
        expr("m['k'].x"),
        prop(
            Expression::Index(Box::new(var("m")), Box::new(string("k"))),
            "x"
        )
    );
}

#[test]
fn variables_and_quoted_identifiers() {
    assert_eq!(expr("`weird name`"), var("weird name"));
    assert_eq!(expr("order"), var("order"));
    assert_eq!(expr("`WHERE`"), var("WHERE"));
    assert!(parse_err("RETURN WHERE").contains("expected an expression"));
}

#[test]
fn function_calls() {
    assert_eq!(expr("toUpper(x)"), func("toUpper", false, vec![var("x")]));
    assert_eq!(expr("rand()"), func("rand", false, vec![]));
    assert_eq!(
        expr("db.vector.similarity(a, b)"),
        func("db.vector.similarity", false, vec![var("a"), var("b")])
    );
    assert_eq!(
        expr("count(DISTINCT x)"),
        func("count", true, vec![var("x")])
    );
    assert_eq!(expr("count(*)"), Expression::CountStar);
    assert_eq!(expr("COUNT( * )"), Expression::CountStar);
    assert!(expr("sum(n.x) + 1").contains_aggregate());
    assert!(parse_err("RETURN f(1, 2").contains("expected ',' or ')'"));
}

#[test]
fn case_expressions() {
    assert_eq!(
        expr("CASE WHEN a THEN 1 WHEN b THEN 2 ELSE 3 END"),
        Expression::Case {
            operand: None,
            whens: vec![(var("a"), int(1)), (var("b"), int(2))],
            default: Some(Box::new(int(3))),
        }
    );
    assert_eq!(
        expr("case n.x when 1 then 'one' end"),
        Expression::Case {
            operand: Some(Box::new(prop(var("n"), "x"))),
            whens: vec![(int(1), string("one"))],
            default: None,
        }
    );
    assert!(parse_err("RETURN CASE ELSE 1 END").contains("expected WHEN"));
    assert!(parse_err("RETURN CASE WHEN a THEN 1").contains("expected END"));
}

#[test]
fn list_comprehension() {
    assert_eq!(
        expr("[x IN range(1, 10) WHERE x % 2 = 0 | x * x]"),
        Expression::ListComprehension {
            variable: "x".into(),
            list: Box::new(func("range", false, vec![int(1), int(10)])),
            predicate: Some(Box::new(bin(bin(var("x"), Mod, int(2)), Eq, int(0)))),
            map: Some(Box::new(bin(var("x"), Mul, var("x")))),
        }
    );
    assert_eq!(
        expr("[x IN l]"),
        Expression::ListComprehension {
            variable: "x".into(),
            list: Box::new(var("l")),
            predicate: None,
            map: None
        }
    );
    assert!(matches!(
        expr("[x IN l | x.name]"),
        Expression::ListComprehension {
            predicate: None,
            map: Some(_),
            ..
        }
    ));
}

#[test]
fn comparison_operators() {
    for (src, op) in [
        ("a = b", Eq),
        ("a <> b", NotEq),
        ("a != b", NotEq),
        ("a < b", Lt),
        ("a <= b", LtEq),
        ("a > b", Gt),
        ("a >= b", GtEq),
        ("a =~ b", RegexMatch),
        ("a IN b", In),
        ("a STARTS WITH b", StartsWith),
        ("a ends with b", EndsWith),
        ("a CONTAINS b", Contains),
    ] {
        assert_eq!(expr(src), bin(var("a"), op, var("b")), "{src}");
    }
    assert_eq!(
        expr("a IS NULL"),
        Expression::IsNull(Box::new(var("a")), false)
    );
    assert_eq!(
        expr("a.x IS NOT NULL"),
        Expression::IsNull(Box::new(prop(var("a"), "x")), true)
    );
    assert_eq!(
        expr("n:Person:Admin"),
        Expression::HasLabels(Box::new(var("n")), vec!["Person".into(), "Admin".into()])
    );
    assert!(parse_err("RETURN a STARTS b").contains("expected WITH"));
    assert!(parse_err("RETURN a IS 1").contains("expected NULL"));
}

#[test]
fn boolean_precedence() {
    // OR < XOR < AND < NOT < comparison
    assert_eq!(
        expr("a OR b XOR c AND NOT d = e"),
        bin(
            var("a"),
            Or,
            bin(
                var("b"),
                Xor,
                bin(
                    var("c"),
                    And,
                    unary(UnaryOp::Not, bin(var("d"), Eq, var("e")))
                )
            )
        )
    );
    assert_eq!(
        expr("a AND b AND c"),
        bin(bin(var("a"), And, var("b")), And, var("c"))
    );
    assert_eq!(
        expr("NOT NOT a"),
        unary(UnaryOp::Not, unary(UnaryOp::Not, var("a")))
    );
    assert_eq!(
        expr("NOT n:A AND m:B"),
        bin(
            unary(
                UnaryOp::Not,
                Expression::HasLabels(Box::new(var("n")), vec!["A".into()])
            ),
            And,
            Expression::HasLabels(Box::new(var("m")), vec!["B".into()])
        )
    );
}

#[test]
fn arithmetic_precedence() {
    assert_eq!(
        expr("1 + 2 * 3"),
        bin(int(1), Add, bin(int(2), Mul, int(3)))
    );
    assert_eq!(
        expr("1 - 2 - 3"),
        bin(bin(int(1), Sub, int(2)), Sub, int(3))
    );
    assert_eq!(
        expr("a * b / c % d"),
        bin(
            bin(bin(var("a"), Mul, var("b")), Div, var("c")),
            Mod,
            var("d")
        )
    );
    assert_eq!(
        expr("2 * 3 ^ 2"),
        bin(int(2), Mul, bin(int(3), Pow, int(2)))
    );
    assert_eq!(
        expr("2 ^ 3 ^ 2"),
        bin(bin(int(2), Pow, int(3)), Pow, int(2))
    );
    assert_eq!(
        expr("-x ^ 2"),
        bin(unary(UnaryOp::Neg, var("x")), Pow, int(2))
    );
    assert_eq!(expr("-n.x"), unary(UnaryOp::Neg, prop(var("n"), "x")));
    assert_eq!(expr("+x"), unary(UnaryOp::Plus, var("x")));
    assert_eq!(
        expr("(1 + 2) * 3"),
        bin(bin(int(1), Add, int(2)), Mul, int(3))
    );
    assert_eq!(
        expr("a + 1 < b * 2"),
        bin(bin(var("a"), Add, int(1)), Lt, bin(var("b"), Mul, int(2)))
    );
    assert_eq!(expr("1--1"), bin(int(1), Sub, int(-1)));
}

#[test]
fn expression_errors() {
    assert!(parse_err("RETURN (1 + 2").contains("expected ')'"));
    assert!(parse_err("RETURN [1, 2").contains("expected ',' or ']'"));
    assert!(parse_err("RETURN {a 1}").contains("expected ':'"));
    assert!(parse_err("RETURN 1 +").contains("unexpected end of input, expected an expression"));
    assert!(parse_err("RETURN n.").contains("expected a property key"));
}

/// Escapes `s` as a single-quoted Cypher string literal using every escape
/// form the lexer understands.
fn quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() && (c as u32) <= 0xFFFF => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

proptest! {
    #[test]
    fn integer_literal_round_trip(n in any::<i64>()) {
        prop_assert_eq!(expr(&n.to_string()), int(n));
    }

    #[test]
    fn string_literal_round_trip(s in any::<String>()) {
        prop_assert_eq!(expr(&quote(&s)), string(&s));
    }

    #[test]
    fn float_literal_round_trip(x in -1.0e300f64..1.0e300) {
        prop_assert_eq!(expr(&format!("{x:?}")), lit(Value::Float(x)));
    }
}
