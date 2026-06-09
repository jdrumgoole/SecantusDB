use geo::{Geometry, Point, Polygon, LineString, Coord};
use geo::relate::Relate;
fn main() {
    let poly: Geometry<f64> = Geometry::Polygon(Polygon::new(
        LineString(vec![Coord{x:0.0,y:0.0},Coord{x:10.0,y:0.0},Coord{x:10.0,y:10.0},Coord{x:0.0,y:10.0},Coord{x:0.0,y:0.0}]),
        vec![],
    ));
    let inside: Geometry<f64> = Geometry::Point(Point::new(5.0, 5.0));
    let outside: Geometry<f64> = Geometry::Point(Point::new(50.0, 50.0));
    let im = inside.relate(&poly);
    println!("inside within={} intersects={}", im.is_within(), im.is_intersects());
    let im2 = outside.relate(&poly);
    println!("outside within={} intersects={}", im2.is_within(), im2.is_intersects());
}
