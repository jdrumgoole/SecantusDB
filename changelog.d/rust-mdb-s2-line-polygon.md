### Rust MongoDB server: a line against a polygon is S2's clip

#### Fixed

- **`$geoIntersects` of a line and a polygon is mongod's
  `polygonLineIntersection`** (fixes RM-148): the line clipped to the
  polygon by a transcription of `S2Polygon::IntersectWithPolyline`
  (`InternalClipPolyline`, `ClipEdge`, `AddIntersection`, and
  `S2EdgeUtil::GetIntersection`), in place of a midpoint test. A line
  along lng 180 now meets a polygon with an edge on that meridian, as on
  mongod 8.2.11. A sweep of 152 lines against five polygons, holes and
  the antimeridian included, agrees throughout
  (`tests/backlog/test_rm_geo_residuals.py::test_line_along_a_polygon_edge_on_the_antimeridian`).
