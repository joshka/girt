//! Git stores signed decimal HHMM despite the published minute-unit description.
//! Keep the public minute unit and use checked conversion at this wire boundary.

use super::Error;

pub(super) fn encode(minutes: i16) -> Result<[u8; 2], Error> {
    let minutes = i32::from(minutes);
    let hhmm = minutes / 60 * 100 + minutes % 60;
    let stored = i16::try_from(hhmm)
        .map_err(|_| Error::Unsupported("reflog timezone exceeds signed HHMM field"))?;
    Ok(stored.to_be_bytes())
}

pub(super) fn decode(bytes: [u8; 2]) -> i16 {
    let hhmm = i16::from_be_bytes(bytes);
    hhmm / 100 * 60 + hhmm % 100
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::utc(0, 0)]
    #[case::west(-420, -700)]
    #[case::east_half_hour(330, 530)]
    #[case::west_half_hour(-210, -330)]
    #[case::east_quarter_hour(15, 15)]
    #[case::west_quarter_hour(-15, -15)]
    #[case::positive_day_boundary(1439, 2359)]
    #[case::negative_day_boundary(-1439, -2359)]
    #[case::positive_wire_boundary(19679, 32759)]
    #[case::negative_wire_boundary(-19679, -32759)]
    fn converts_semantic_minutes_and_signed_hhmm(#[case] minutes: i16, #[case] hhmm: i16) {
        assert_eq!(encode(minutes).unwrap(), hhmm.to_be_bytes());
        assert_eq!(decode(hhmm.to_be_bytes()), minutes);
    }

    #[rstest]
    #[case::positive_noncanonical(480, 320)]
    #[case::negative_noncanonical(-480, -320)]
    #[case::largest_signed_field(i16::MAX, 19687)]
    #[case::smallest_signed_field(i16::MIN, -19688)]
    fn interprets_noncanonical_minute_digits_arithmetically(
        #[case] hhmm: i16,
        #[case] minutes: i16,
    ) {
        assert_eq!(decode(hhmm.to_be_bytes()), minutes);
    }

    #[rstest]
    #[case::positive_wire_overflow(19680)]
    #[case::negative_wire_overflow(-19680)]
    #[case::largest_minutes(i16::MAX)]
    #[case::smallest_minutes(i16::MIN)]
    fn refuses_unrepresentable_offsets(#[case] minutes: i16) {
        assert!(matches!(encode(minutes), Err(Error::Unsupported(_))));
    }
}
