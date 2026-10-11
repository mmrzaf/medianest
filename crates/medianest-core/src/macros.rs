/// Declares an enum persisted as an integer code in the catalog.
macro_rules! int_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $value:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$vmeta])* $variant = $value),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            #[must_use]
            pub const fn code(self) -> i64 {
                self as i64
            }

            pub fn from_code(value: i64) -> Result<Self, $crate::UnknownValue> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    other => Err($crate::UnknownValue { kind: stringify!($name), value: other.to_string() }),
                }
            }
        }
    };
}
