use crate::Color;

/// The background of an element. This guest port supports solid sRGB colors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Background {
    /// A solid color.
    Color(Color),
}

impl Background {
    /// Scales the alpha channel by the given factor.
    pub fn scale_alpha(self, factor: f32) -> Self {
        match self {
            Self::Color(color) => Self::Color(color.scale_alpha(factor)),
        }
    }
}

impl From<Color> for Background {
    fn from(color: Color) -> Self {
        Self::Color(color)
    }
}
