use wren_macros::foreign_static_method;

#[foreign_static_method]
fn foreign_test<V, L: Location>(context: Context<V, L>, b: f64, c: f64) -> f64 {
    a + b + c
}

fn main() {}
