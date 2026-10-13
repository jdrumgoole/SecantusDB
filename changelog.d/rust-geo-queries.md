### Rust MongoDB server: geo queries checked against mongod

The Rust MongoDB server tested every geo shape as if coordinates were a
plane, and treated `$near` as a filter. mongod tests `$geometry` on the
sphere, where an edge is the great-circle arc between its ends, and returns
a `$near` nearest first from a geo index it requires.

Found by sending 1,278 geo commands to the server and to mongod 8.2.11 and
comparing each reply. 726 differed; 137 do now, about 100 of them the order
of documents under a geo index (`tasks/backlog.md` 7.081).

#### Fixed

- `$geoWithin` and `$geoIntersects` with `$geometry` are tested on the
  sphere. `[1, 1]` is not on the line from `[0, 0]` to `[2, 2]`, and a
  polygon across the antimeridian is the small one.
- `$box`, `$polygon` and `$center` return points only. They returned a
  line or a polygon that lay inside them.
- A value holding several locations (`[[1, 1], [30, 30]]`) matches through
  any of them, and is found through a `2d` index.
- `$near` and `$nearSphere` return documents nearest first, measure a line
  or a polygon by its nearest point, and need a geo index on the field
  that the form of the query can read. They are refused inside `$or`,
  `$nor`, `$not` and `$match`.
- `$geoNear` takes its units from `near`: metres for GeoJSON, radians for
  a legacy point with `spherical: true`. It no longer needs
  `distanceField`, refuses `limit` and `num`, and must be the first stage.
- A `2dsphere` index refuses a value that is not a geometry, and a `2d`
  index a value that is not a legacy pair: on insert, on update, and when
  the index is built over such a document.
- `$within` is read as `$geoWithin`.
- About ninety argument errors have mongod's code and text.

#### Changed

- A `$geoNear` with a `maxDistance` reads every document of the collection.
  It used to fetch candidates through the index.
