pub trait Shape {
    fn area(&self) -> u32;
    fn twice(&self) -> u32 {
        self.area() * 2
    }
}
pub struct Sq(pub u32);
impl Shape for Sq {
    fn area(&self) -> u32 {
        self.0
    }
}
pub fn method_call() -> u32 {
    Sq(1).area()
}
pub fn qualified_call() -> u32 {
    <Sq as Shape>::area(&Sq(2))
}
pub fn default_method() -> u32 {
    Sq(3).twice()
}
