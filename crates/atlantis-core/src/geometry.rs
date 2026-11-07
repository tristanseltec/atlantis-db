//! Geometry values in WGS84 longitude/latitude.
//!
//! This module only defines the value type and its validation. Geo
//! functions and the spatial index belong to the planned `atlantis-geo`
//! crate.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::AtlantisError;

/// A WGS84 position: longitude then latitude, in degrees.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Point {
    pub lon: f64,
    pub lat: f64,
}

impl Point {
    /// Creates a point, rejecting coordinates outside WGS84 bounds.
    pub fn new(lon: f64, lat: f64) -> Result<Self, AtlantisError> {
        let p = Point { lon, lat };
        p.validate()?;
        Ok(p)
    }

    fn validate(&self) -> Result<(), AtlantisError> {
        if !(-180.0..=180.0).contains(&self.lon) || !(-90.0..=90.0).contains(&self.lat) {
            return Err(AtlantisError::InvalidValue(format!(
                "point out of WGS84 bounds: lon {}, lat {}",
                self.lon, self.lat
            )));
        }
        Ok(())
    }
}

/// A geometry following the GeoJSON / OGC simple-features model.
#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum Geometry {
    Point(Point),
    /// At least two positions.
    LineString(Vec<Point>),
    /// One exterior ring followed by optional holes. Each ring is closed
    /// (first position equals last) and has at least four positions.
    Polygon(Vec<Vec<Point>>),
    MultiPoint(Vec<Point>),
    MultiLineString(Vec<Vec<Point>>),
    MultiPolygon(Vec<Vec<Vec<Point>>>),
    Collection(Vec<Geometry>),
}

fn invalid(msg: &str) -> AtlantisError {
    AtlantisError::InvalidValue(format!("invalid geometry: {msg}"))
}

fn validate_line(points: &[Point]) -> Result<(), AtlantisError> {
    if points.len() < 2 {
        return Err(invalid("a line string needs at least 2 positions"));
    }
    points.iter().try_for_each(Point::validate)
}

fn validate_polygon(rings: &[Vec<Point>]) -> Result<(), AtlantisError> {
    if rings.is_empty() {
        return Err(invalid("a polygon needs an exterior ring"));
    }
    for ring in rings {
        if ring.len() < 4 || ring.first() != ring.last() {
            return Err(invalid(
                "polygon rings must be closed and have at least 4 positions",
            ));
        }
        ring.iter().try_for_each(Point::validate)?;
    }
    Ok(())
}

impl Geometry {
    /// Checks coordinate bounds and structural rules.
    pub fn validate(&self) -> Result<(), AtlantisError> {
        match self {
            Geometry::Point(p) => p.validate(),
            Geometry::LineString(line) => validate_line(line),
            Geometry::Polygon(rings) => validate_polygon(rings),
            Geometry::MultiPoint(points) => points.iter().try_for_each(Point::validate),
            Geometry::MultiLineString(lines) => lines.iter().try_for_each(|l| validate_line(l)),
            Geometry::MultiPolygon(polys) => polys.iter().try_for_each(|p| validate_polygon(p)),
            Geometry::Collection(items) => items.iter().try_for_each(Geometry::validate),
        }
    }

    /// The geometry kind, matching GeoJSON type names.
    pub fn kind(&self) -> &'static str {
        match self {
            Geometry::Point(_) => "Point",
            Geometry::LineString(_) => "LineString",
            Geometry::Polygon(_) => "Polygon",
            Geometry::MultiPoint(_) => "MultiPoint",
            Geometry::MultiLineString(_) => "MultiLineString",
            Geometry::MultiPolygon(_) => "MultiPolygon",
            Geometry::Collection(_) => "GeometryCollection",
        }
    }
}

fn write_points(f: &mut fmt::Formatter<'_>, points: &[Point]) -> fmt::Result {
    f.write_str("(")?;
    for (i, p) in points.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{} {}", p.lon, p.lat)?;
    }
    f.write_str(")")
}

fn write_rings(f: &mut fmt::Formatter<'_>, rings: &[Vec<Point>]) -> fmt::Result {
    f.write_str("(")?;
    for (i, ring) in rings.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write_points(f, ring)?;
    }
    f.write_str(")")
}

impl fmt::Display for Geometry {
    /// Formats as Well-Known Text, e.g. `POINT(107.59 16.46)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Geometry::Point(p) => write!(f, "POINT({} {})", p.lon, p.lat),
            Geometry::LineString(line) => {
                f.write_str("LINESTRING")?;
                write_points(f, line)
            }
            Geometry::Polygon(rings) => {
                f.write_str("POLYGON")?;
                write_rings(f, rings)
            }
            Geometry::MultiPoint(points) => {
                f.write_str("MULTIPOINT")?;
                write_points(f, points)
            }
            Geometry::MultiLineString(lines) => {
                f.write_str("MULTILINESTRING")?;
                write_rings(f, lines)
            }
            Geometry::MultiPolygon(polys) => {
                f.write_str("MULTIPOLYGON(")?;
                for (i, poly) in polys.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write_rings(f, poly)?;
                }
                f.write_str(")")
            }
            Geometry::Collection(items) => {
                f.write_str("GEOMETRYCOLLECTION(")?;
                for (i, g) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{g}")?;
                }
                f.write_str(")")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(lon: f64, lat: f64) -> Point {
        Point { lon, lat }
    }

    #[test]
    fn point_bounds() {
        assert!(Point::new(107.59, 16.46).is_ok());
        assert!(Point::new(181.0, 0.0).is_err());
        assert!(Point::new(0.0, -91.0).is_err());
    }

    #[test]
    fn polygon_rules() {
        let square = vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 0.0)];
        assert!(Geometry::Polygon(vec![square.clone()]).validate().is_ok());
        let open = vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.5, 0.5)];
        assert!(Geometry::Polygon(vec![open]).validate().is_err());
        assert!(Geometry::Polygon(vec![]).validate().is_err());
        assert!(Geometry::LineString(vec![p(0.0, 0.0)]).validate().is_err());
        let nested = Geometry::Collection(vec![Geometry::Point(p(200.0, 0.0))]);
        assert!(nested.validate().is_err());
    }

    #[test]
    fn wkt_display() {
        assert_eq!(
            Geometry::Point(p(107.59, 16.46)).to_string(),
            "POINT(107.59 16.46)"
        );
        let line = Geometry::LineString(vec![p(0.0, 0.0), p(1.0, 1.0)]);
        assert_eq!(line.to_string(), "LINESTRING(0 0, 1 1)");
        assert_eq!(line.kind(), "LineString");
    }
}
