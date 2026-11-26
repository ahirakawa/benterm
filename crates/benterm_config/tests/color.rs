use benterm_config::Rgba;
use proptest::prelude::*;

#[test]
fn parses_six_digit_hex_as_opaque() {
    assert_eq!("#1e1f22".parse(), Ok(Rgba::rgb(0x1e, 0x1f, 0x22)));
}

#[test]
fn parses_eight_digit_hex_with_alpha() {
    assert_eq!(
        "#6aa1ff4d".parse(),
        Ok(Rgba {
            r: 0x6a,
            g: 0xa1,
            b: 0xff,
            a: 0x4d
        })
    );
}

#[test]
fn accepts_upper_case_digits() {
    assert_eq!("#6AA1FF".parse(), Ok(Rgba::rgb(0x6a, 0xa1, 0xff)));
}

#[test]
fn rejects_malformed_strings() {
    for s in [
        "",
        "#",
        "1e1f22",
        "#1e1f2",
        "#1e1f222",
        "#1e1f22a",
        "#gggggg",
        "#1e1f22 ",
        " #1e1f22",
        "#+1e1f2",
        "#１e1f22",
        "red",
    ] {
        assert!(s.parse::<Rgba>().is_err(), "{s:?} should be rejected");
    }
}

#[test]
fn displays_opaque_colors_without_alpha() {
    assert_eq!(Rgba::rgb(0x1e, 0x1f, 0x22).to_string(), "#1e1f22");
}

#[test]
fn displays_translucent_colors_with_alpha() {
    let c = Rgba {
        r: 1,
        g: 2,
        b: 3,
        a: 4,
    };
    assert_eq!(c.to_string(), "#01020304");
}

proptest! {
    #[test]
    fn display_round_trips(r: u8, g: u8, b: u8, a: u8) {
        let c = Rgba { r, g, b, a };
        prop_assert_eq!(c.to_string().parse::<Rgba>(), Ok(c));
    }

    #[test]
    fn parsing_any_string_does_not_panic(s in "\\PC*") {
        let _ = s.parse::<Rgba>();
    }
}
