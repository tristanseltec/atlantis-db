//! Spatial functions: `point()` and great-circle `distance()`.

use atlantis_core::{AtlantisError, Geometry, Point, Value};

use super::{arity, first, invalid};

fn haversine_m(a: &Point, b: &Point) -> f64 {
    let r = 6_371_008.8;
    let (la1, la2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = la2 - la1;
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().asin()
}

/// `point({longitude, latitude})` (or `{x, y}`).
pub(super) fn point(name: &str, args: Vec<Value>) -> Result<Value, AtlantisError> {
    arity(name, &args, 1)?;
    let Value::Object(map) = first(args) else {
        return Ok(Value::Null);
    };
    let coord = |a: &str, b: &str| map.get(a).or_else(|| map.get(b)).and_then(Value::as_float);
    match (coord("longitude", "x"), coord("latitude", "y")) {
        (Some(lon), Some(lat)) => Ok(Value::Geometry(Geometry::Point(Point::new(lon, lat)?))),
        _ => Err(invalid("point() needs longitude/latitude (or x/y)")),
    }
}

/// `distance(a, b)` between two points, in metres.
pub(super) fn distance(name: &str, args: Vec<Value>) -> Result<Value, AtlantisError> {
    arity(name, &args, 2)?;
    Ok(match (&args[0], &args[1]) {
        (Value::Geometry(Geometry::Point(a)), Value::Geometry(Geometry::Point(b))) => {
            Value::Float(haversine_m(a, b))
        }
        _ => Value::Null,
    })
}
