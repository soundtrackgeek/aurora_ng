//! Country flags in colour, from flag-icons (MIT), compiled into the binary.

include!(concat!(env!("OUT_DIR"), "/flags.rs"));

/// The flag for a lowercase ISO country code like "gb", if there is one.
pub fn svg(code: &str) -> Option<&'static [u8]> {
    FLAGS
        .binary_search_by(|(flag, _)| (*flag).cmp(code))
        .ok()
        .map(|ix| FLAGS[ix].1)
}

/// The asset path of a country's flag, like "flags/gb.svg", for codes as
/// MusicBrainz gives them ("GB"). None for countries without a flag, such
/// as the Soviet Union ("SU") or Yugoslavia ("YU").
pub fn path(code: &str) -> Option<String> {
    let code = match code.trim().to_ascii_lowercase() {
        code if code == "uk" => "gb".to_owned(),
        code => code,
    };
    svg(&code).map(|_| format!("flags/{code}.svg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_flags_by_country_code() {
        assert_eq!(path("GB").as_deref(), Some("flags/gb.svg"));
        assert_eq!(path(" no ").as_deref(), Some("flags/no.svg"));
        assert_eq!(path("UK").as_deref(), Some("flags/gb.svg"));
        assert_eq!(path("SU"), None);
        assert_eq!(path(""), None);
        assert!(FLAGS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }
}
