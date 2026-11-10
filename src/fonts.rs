//! Which fonts po draws with: the bundled Geist and Geist Mono are the default and the last
//! resort, so po looks the same whatever is installed.

/// Sizes outside this are a typo, not a wish.
pub const SIZES: std::ops::RangeInclusive<f32> = 8.0..=48.0;

/// The fonts in the binary.
pub const BUNDLED: [&[u8]; 12] = [
    include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../assets/fonts/Geist-Italic.ttf"),
    include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Italic.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
    include_bytes!("../assets/fonts/GeistMono-SemiBold.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Bold.ttf"),
    include_bytes!("../assets/fonts/Literata-Variable.ttf"),
    include_bytes!("../assets/fonts/Literata-Italic-Variable.ttf"),
];
pub const BUNDLED_UI_FAMILY: &str = "Geist";
pub const BUNDLED_MONO_FAMILY: &str = "Geist Mono";
/// The serif markdown is read in.
pub const BUNDLED_BOOK_FAMILY: &str = "Literata";
