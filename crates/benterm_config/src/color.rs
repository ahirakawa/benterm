use std::fmt;
use std::str::FromStr;

/// A color with 8-bit channels. Alpha 255 is opaque.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// The reason a string is not a color.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorError;

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
}

impl FromStr for Rgba {
    type Err = ColorError;

    /// Accepts `#rrggbb` and `#rrggbbaa`, in either case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let digits = s.strip_prefix('#').ok_or(ColorError)?;
        if !(digits.len() == 6 || digits.len() == 8)
            || !digits.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(ColorError);
        }
        let channel = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).map_err(|_| ColorError);
        Ok(Self {
            r: channel(0)?,
            g: channel(2)?,
            b: channel(4)?,
            a: if digits.len() == 8 { channel(6)? } else { 255 },
        })
    }
}

impl fmt::Display for Rgba {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)?;
        if self.a != 255 {
            write!(f, "{:02x}", self.a)?;
        }
        Ok(())
    }
}
