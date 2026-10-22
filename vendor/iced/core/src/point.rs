use crate::Vector;

use core::fmt;

/// A 2D point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point<T = f32> {
    /// The X coordinate.
    pub x: T,

    /// The Y coordinate.
    pub y: T,
}

impl Point {
    /// The origin (i.e. a [`Point`] at (0, 0)).
    pub const ORIGIN: Self = Self::new(0.0, 0.0);
}

impl<T> Point<T> {
    /// Creates a new [`Point`] with the given coordinates.
    pub const fn new(x: T, y: T) -> Self {
        Self { x, y }
    }
}

impl<T> From<[T; 2]> for Point<T> {
    fn from([x, y]: [T; 2]) -> Self {
        Point { x, y }
    }
}

impl<T> From<(T, T)> for Point<T> {
    fn from((x, y): (T, T)) -> Self {
        Self { x, y }
    }
}

impl<T> From<Point<T>> for [T; 2] {
    fn from(point: Point<T>) -> [T; 2] {
        [point.x, point.y]
    }
}

impl<T> core::ops::Add<Vector<T>> for Point<T>
where
    T: core::ops::Add<Output = T>,
{
    type Output = Self;

    fn add(self, vector: Vector<T>) -> Self {
        Self {
            x: self.x + vector.x,
            y: self.y + vector.y,
        }
    }
}

impl<T> core::ops::AddAssign<Vector<T>> for Point<T>
where
    T: core::ops::AddAssign,
{
    fn add_assign(&mut self, vector: Vector<T>) {
        self.x += vector.x;
        self.y += vector.y;
    }
}

impl<T> core::ops::Sub<Vector<T>> for Point<T>
where
    T: core::ops::Sub<Output = T>,
{
    type Output = Self;

    fn sub(self, vector: Vector<T>) -> Self {
        Self {
            x: self.x - vector.x,
            y: self.y - vector.y,
        }
    }
}

impl<T> core::ops::SubAssign<Vector<T>> for Point<T>
where
    T: core::ops::SubAssign,
{
    fn sub_assign(&mut self, vector: Vector<T>) {
        self.x -= vector.x;
        self.y -= vector.y;
    }
}

impl<T> core::ops::Sub<Point<T>> for Point<T>
where
    T: core::ops::Sub<Output = T>,
{
    type Output = Vector<T>;

    fn sub(self, point: Self) -> Vector<T> {
        Vector::new(self.x - point.x, self.y - point.y)
    }
}

impl<T> fmt::Display for Point<T>
where
    T: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Point {{ x: {}, y: {} }}", self.x, self.y)
    }
}

impl Point<f32> {
    /// Snaps the [`Point`] to __unsigned__ integer coordinates.
    pub fn snap(self) -> Point<u32> {
        Point {
            x: crate::round_to_u32(self.x),
            y: crate::round_to_u32(self.y),
        }
    }
}
