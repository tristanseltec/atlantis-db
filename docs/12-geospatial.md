# 12 — Geospatial

> **Status:** Planned — Phase 3 (the `Geometry` value type arrives in Phase 2)
> **Crates:** `atlantis-geo` (planned), `atlantis-core`, `atlantis-kv` (planned), `atlantis-cypher`

Geospatial support stores locations and shapes as values, answers questions such as
"which places are within 5 km" or "which parcels intersect this polygon", and indexes
points for fast spatial lookups. There is no geospatial code today: `Value` in
`crates/atlantis-core/src/value.rs` has no geometry variant. Everything below is
target design.

## Geometry Types (Planned — Phase 2)

`Geometry` is a variant of the target `Value` type (see [03 — Data Model](03-data-model.md)).
All coordinates are **WGS84** longitude/latitude in degrees, the coordinate system of
GPS and GeoJSON.

| Kind | Description | GeoJSON `type` |
|---|---|---|
| `Point` | One position | `Point` |
| `LineString` | Ordered positions forming a path | `LineString` |
| `Polygon` | One outer ring plus optional holes; rings are closed | `Polygon` |
| `MultiPoint` | Set of points | `MultiPoint` |
| `MultiLineString` | Set of paths | `MultiLineString` |
| `MultiPolygon` | Set of polygons | `MultiPolygon` |
| `Collection` | Mixed set of geometries | `GeometryCollection` |

Field types in DDL ([07 — Schema & DDL](07-schema-ddl.md)):

```cypher
CREATE TABLE Place SCHEMAFULL;
CREATE FIELD name ON Place TYPE string;
CREATE FIELD location ON Place TYPE point;
CREATE FIELD footprint ON Place TYPE option<geometry>;
```

`point` accepts only points. `geometry` accepts any kind.

Validation on write: longitude must be in `[-180, 180]`, latitude in `[-90, 90]`,
polygon rings must be closed and have at least four positions, and an outer ring must
not self-intersect. Invalid input fails the write with a validation error.
Altitude and Cartesian (planar x/y) coordinates are not supported.

## Constructing Geometries (Planned — Phase 3)

```cypher
// Points: Neo4j-compatible map form
RETURN point({longitude: 107.5909, latitude: 16.4637}) AS hue;

// Polygons from rings of [longitude, latitude] pairs
RETURN geo.polygon([[107.55, 16.44], [107.62, 16.44], [107.62, 16.49],
                    [107.55, 16.49], [107.55, 16.44]]) AS old_city;

// Accessors
WITH point({longitude: 107.5909, latitude: 16.4637}) AS p
RETURN p.longitude, p.latitude;
```

`geo.polygon` also accepts a list of rings (`[outer, hole1, ...]`), where the first ring
is the outer boundary.

### GeoJSON Input and Output

Any geometry kind can be built from a GeoJSON geometry object and converted back:

```cypher
CREATE (:Place {
  name: 'Imperial City',
  location: point({longitude: 107.5790, latitude: 16.4698}),
  footprint: geo.fromGeoJSON({type: 'Polygon',
    coordinates: [[[107.574, 16.465], [107.585, 16.465], [107.585, 16.475],
                   [107.574, 16.475], [107.574, 16.465]]]})
});

MATCH (p:Place) RETURN p.name, geo.toGeoJSON(p.footprint) AS geojson;
```

`geo.fromGeoJSON(map)` accepts the geometry object only (not `Feature` or
`FeatureCollection`). `geo.toGeoJSON(geometry)` returns a map that serializes to valid
GeoJSON. Server and SDK responses encode geometry values as GeoJSON maps
(see [17 — Server & Protocols](17-server-protocols.md)).

## Spatial Functions (Planned — Phase 3)

| Function | Returns | Description |
|---|---|---|
| `point({longitude, latitude})` | point | Construct a WGS84 point |
| `point.distance(a, b)` | float | Geodesic distance in metres between two points |
| `point.withinBBox(p, lowerLeft, upperRight)` | bool | Point inside a longitude/latitude box (Neo4j 5 form) |
| `geo.polygon(rings)` | geometry | Construct a polygon |
| `geo.within(a, b)` | bool | `a` lies entirely inside `b` |
| `geo.intersects(a, b)` | bool | `a` and `b` share at least one point |
| `geo.fromGeoJSON(map)` | geometry | Parse a GeoJSON geometry object |
| `geo.toGeoJSON(geometry)` | map | Encode as GeoJSON |

Notes:

- `point.distance` uses the haversine formula on a sphere with radius 6,371,008.8 m.
  The error is below 0.5% compared to the ellipsoid, which is enough for proximity
  search. It returns `null` if either argument is `null` or not a point.
- `geo.within` and `geo.intersects` treat edges between positions as great-circle
  segments. A polygon boundary counts as inside for `geo.within`.
- Polygons that cross the antimeridian (180° longitude) must be split into a
  `MultiPolygon`.

## Query Examples (Planned — Phase 3)

Places within 2 km of a point, nearest first:

```cypher
WITH point({longitude: 107.5909, latitude: 16.4637}) AS here
MATCH (p:Place)
WHERE point.distance(p.location, here) < 2000
RETURN p.name, round(point.distance(p.location, here)) AS metres
ORDER BY metres
LIMIT 10;
```

Points inside a drawn area:

```cypher
MATCH (s:Store)
WHERE geo.within(s.location, $area)      // $area: a polygon parameter
RETURN s;
```

Shapes overlapping another shape, combined with a graph hop:

```cypher
MATCH (z:FloodZone {level: 'high'})
MATCH (p:Parcel)-[:OWNED_BY]->(o:Owner)
WHERE geo.intersects(p.footprint, z.area)
RETURN o.name, collect(p.id) AS parcels;
```

Combined with vector search:

```cypher
CALL db.vector.search('review_embedding', $vec, 50) YIELD node AS r, score
MATCH (r)-[:ABOUT]->(p:Place)
WHERE point.distance(p.location, $here) < 5000
RETURN p.name, max(score) AS relevance
ORDER BY relevance DESC LIMIT 10;
```

## Point Index (Planned — Phase 3)

```cypher
CREATE POINT INDEX place_location FOR (p:Place) ON (p.location);
DROP INDEX place_location;
```

A point index covers one `point` field of one table. Records whose field is `null` are
not indexed. Like other indexes, it is built in the background on existing data and
then updated in the record's transaction.

### Cell Design

The index maps each point to a **cell ID** on a hierarchical, S2-style grid:

1. Project the sphere onto the six faces of a cube.
2. Subdivide each face recursively into four children, up to level 30
   (cells of about 1 cm² at the finest level).
3. Number cells along a Hilbert curve, so cells that are close in space usually have
   close IDs, and every descendant of a cell falls inside one contiguous ID range.

Each point is stored as one index entry at the finest level, using the `ix/` key family
from [04 — Storage Engine](04-storage-engine.md):

```
.../tb/{tb}/ix/{index}/{cellId: u64 big-endian}/{key}   -> (empty)
```

Because a coarse cell is a contiguous range of fine cell IDs, any region can be
answered with a few KV range scans.

### Query Execution

| Predicate | Index plan |
|---|---|
| `point.distance(x.loc, $c) < r` | Cover the circle with a small set of cells (coarse in the middle, fine at the edge), range-scan each, then check the exact distance |
| `geo.within(x.loc, $polygon)` | Cover the polygon with cells, range-scan, then run the exact point-in-polygon test |
| `point.withinBBox(x.loc, $ll, $ur)` | Cover the box, range-scan, exact check |
| `ORDER BY point.distance(x.loc, $c) LIMIT k` | Expanding search: scan rings of cells outward from `$c` until `k` results are confirmed closer than the next unscanned ring |

The cell covering always contains the region, so the exact check only removes false
positives and never misses a record. The planner picks the index when the predicate
compares the indexed field with a constant or parameter
(see [15 — Query Planner & Executor](15-query-planner-executor.md)).

Only `Point` values are indexed in Phase 3. `geo.intersects` over polygon fields runs
as a scan with an exact filter. Indexing non-point geometries through multi-cell
coverings is a possible later extension and is not scheduled.

## The `atlantis-geo` Crate (Planned — Phase 3)

| Responsibility | Notes |
|---|---|
| Geometry operations | Validation, distance, containment, intersection, GeoJSON conversion |
| Cell math | Point → cell ID, region → cell covering, neighbor rings |
| Point index | Writes and reads `ix/` entries through `atlantis-kv` |
| Cypher functions | Registers `point.*` and `geo.*` with the executor's function registry |

The `Geometry` type itself lives in `atlantis-core`, so every crate can carry geometry
values without depending on `atlantis-geo`.

## Design Decisions

**Why WGS84 only?** It is what GPS devices, map tiles and GeoJSON use. Supporting
several reference systems adds conversion errors and API surface that most
applications never need.

**Why cells over the KV store instead of an R-tree?** Cell IDs turn spatial lookups into
ordered range scans, which the transactional KV layer already does well. The index then
gets transactions, MVCC, backup and cluster distribution with no extra structure. An
R-tree would need its own page management and concurrency control.

**Why a Hilbert curve?** It preserves locality better than simpler curves such as Z-order,
so a region needs fewer and longer range scans.

**Why haversine instead of ellipsoidal distance?** It is fast and accurate enough for
search and ranking. Surveying-grade accuracy is out of scope.

## What's Next

- [03 — Data Model](03-data-model.md) — the `Geometry` value and type names.
- [07 — Schema & DDL](07-schema-ddl.md) — field types and index lifecycle.
- [08 — Document & Relational](08-document-relational.md) — secondary indexes in general.
- [23 — Query Cookbook](23-query-cookbook.md) — more recipes across models.
