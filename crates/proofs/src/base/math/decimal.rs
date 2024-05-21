//! TODO: add docs
use crate::{
    base::scalar::Scalar,
    sql::parse::{ConversionError, ConversionResult},
};
use num_bigint::{
    BigInt,
    Sign::{self, Minus},
};
use proofs_sql::decimal_unknown::DecimalUnknown;
use serde::{Deserialize, Deserializer, Serialize};
use std::str::FromStr;

#[derive(Eq, PartialEq, Debug, Clone, Hash, Serialize, Copy)]
/// limit-enforced precision
pub struct Precision(u8);
pub(crate) const MAX_SUPPORTED_PRECISION: u8 = 75;

impl Precision {
    /// Constructor for creating a Precision instance
    pub fn new(value: u8) -> Result<Self, String> {
        if value > MAX_SUPPORTED_PRECISION || value == 0 {
            Err("Precision must be larger than zero and less than 76".to_owned())
        } else {
            Ok(Precision(value))
        }
    }

    /// Getter method to access the inner value
    pub fn value(&self) -> u8 {
        self.0
    }
}

// Custom deserializer for precision since we need to limit its value to 75
impl<'de> Deserialize<'de> for Precision {
    fn deserialize<D>(deserializer: D) -> Result<Precision, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Deserialize as a u8
        let value = u8::deserialize(deserializer)?;

        // Use the Precision::new method to ensure the value is within the allowed range
        Precision::new(value).map_err(serde::de::Error::custom)
    }
}

/// A decimal type that is parameterized by the scalar type
#[derive(Eq, PartialEq, Debug, Clone, Hash, Serialize)]
pub struct Decimal<S: Scalar> {
    /// The raw value of the decimal as scalar
    pub value: S,
    /// The precision of the decimal
    pub precision: Precision,
    /// The scale of the decimal
    pub scale: i8,
}

impl<S: Scalar> Decimal<S> {
    /// Constructor for creating a Decimal instance
    pub fn new(value: S, precision: Precision, scale: i8) -> Self {
        Decimal {
            value,
            precision,
            scale,
        }
    }

    /// Scale the decimal to the new scale factor. Negative scaling and overflow error out.
    pub fn with_precision_and_scale(
        &self,
        new_precision: Precision,
        new_scale: i8,
    ) -> ConversionResult<Decimal<S>> {
        let scale_factor = new_scale - self.scale;
        if scale_factor < 0 || new_precision.value() < self.precision.value() + scale_factor as u8 {
            return Err(ConversionError::DecimalRoundingError(
                "Scale factor must be non-negative".to_string(),
            ));
        }
        let scaled_value = scale_scalar(self.value, scale_factor)?;
        Ok(Decimal::new(scaled_value, new_precision, new_scale))
    }

    /// Get a decimal with given precision and scale from an i64
    pub fn from_i64(value: i64, precision: Precision, scale: i8) -> ConversionResult<Self> {
        const MINIMAL_PRECISION: u8 = 19;
        let raw_precision = precision.value();
        if raw_precision < MINIMAL_PRECISION {
            return Err(ConversionError::DecimalRoundingError(
                "Precision must be at least 19".to_string(),
            ));
        }
        if scale < 0 || raw_precision < MINIMAL_PRECISION + scale as u8 {
            return Err(ConversionError::DecimalRoundingError(
                "Can not scale down a decimal".to_string(),
            ));
        }
        let scaled_value = scale_scalar(S::from(&value), scale)?;
        Ok(Decimal::new(scaled_value, precision, scale))
    }

    /// Get a decimal with given precision and scale from an i128
    pub fn from_i128(value: i128, precision: Precision, scale: i8) -> ConversionResult<Self> {
        const MINIMAL_PRECISION: u8 = 39;
        let raw_precision = precision.value();
        if raw_precision < MINIMAL_PRECISION {
            return Err(ConversionError::DecimalRoundingError(
                "Precision must be at least 19".to_string(),
            ));
        }
        if scale < 0 || raw_precision < MINIMAL_PRECISION + scale as u8 {
            return Err(ConversionError::DecimalRoundingError(
                "Can not scale down a decimal".to_string(),
            ));
        }
        let scaled_value = scale_scalar(S::from(&value), scale)?;
        Ok(Decimal::new(scaled_value, precision, scale))
    }
}

/// Tries to pair decimals that are equal in semantic value but
/// are represented with differing scales and precisions. For example:
///
/// Scales a decimal according to
///
/// 100(p = 3, s = 1) => 1.0
/// 100(p = 4, s = 3) => 1.000
///
/// Asummes that adjusting the scale upwards is
/// safe (as long as the resulting precision remains
/// less than the max supported value)
/// because there is no loss of information,
/// as opposed to scaling down which is lossy.
pub(crate) fn match_decimal<S: Scalar>(
    d: &DecimalUnknown,
    scale: i8,
) -> Result<S, ConversionError> {
    // Convert limbs into Scalar and account for sign
    let (limbs, sign) = get_limbs_and_sign(d, scale)?;
    let scalar = S::from(limbs);
    match sign {
        Minus => Ok(-scalar),
        _ => Ok(scalar),
    }
}

// determines how to safely scale an incoming decimal
fn get_limbs_and_sign(d: &DecimalUnknown, scale: i8) -> Result<([u64; 4], Sign), ConversionError> {
    // Check for valid precision
    if d.precision() > MAX_SUPPORTED_PRECISION {
        return Err(ConversionError::PrecisionParseError(
            "Error while attempting decimal match: max precision exceeded".to_owned(),
        ));
    }
    // scaling down is lossy behavior akin to rounding which postgresql does not support
    if d.scale() > scale {
        return Err(ConversionError::DecimalRoundingError(format!(
            "matching decimal would cause precision overflow: incoming scale() = {} is greater than db scale = {}",
            d.scale(),
            scale
        )));
    }
    // check to make sure there is room to scale up
    // to the decimal we wish to match to
    // TODO: account for negative scale
    if d.scale() < scale
        && d.precision().saturating_add(scale as u8 - d.scale() as u8) <= MAX_SUPPORTED_PRECISION
    {
        return Ok(decimal_string_to_scaled_limbs(d, Some(scale)));
    } else if d.precision() + scale as u8 > MAX_SUPPORTED_PRECISION && scale > d.scale {
        // if scaling the value up exceeds supported precision, then error
        return Err(ConversionError::PrecisionParseError(format!(
            "Scaling factor {} exceeds maximum allowed precision",
            d.precision() as i8 + scale
        )));
    }
    // If none of the error conditions are met, proceed with the conversion
    Ok(decimal_string_to_scaled_limbs(d, None))
}

// Uses num-bigint to correctly parse decimal intermediate type into limbs
fn decimal_string_to_scaled_limbs(decimal: &DecimalUnknown, scale: Option<i8>) -> ([u64; 4], Sign) {
    // Parse the decimal string
    let mut value: String = decimal.value();

    // Determine the number of zeros to append: scale - self.scale
    // i.e. to match 1.0 to 1.000 then scale = 3 and self.scale 1
    // so 3 - 1 = 2 zeros appended.
    // If scale is None or zero, no zeros are appended.
    // Limited by the maximum length of 75 characters
    // This is safer and less error-prone than multiplying by 10^scale
    let actual_zeros_to_append = std::cmp::min(
        (scale.unwrap_or(0) as u8).saturating_sub(decimal.scale() as u8),
        75_u8.saturating_sub(value.len().try_into().unwrap()),
    );

    // Extend the string with the determined number of zeros
    value.extend(std::iter::repeat('0').take(actual_zeros_to_append.into()));
    // Convert to bigint
    let integer_result =
        BigInt::from_str(&value).expect("Failed to convert decimal string to BigInt");

    // Convert to limbs, ensuring at least 4 elements, filled with 0 if necessary
    let (sign, integer_parts) = integer_result.to_u64_digits();
    (
        integer_parts
            .into_iter()
            .chain(std::iter::repeat(0)) // fill up with zeros
            .take(4) // ensures that we always have 4 limbs
            .collect::<Vec<_>>() // turn into vec
            .try_into()
            .expect("Error while parsing decimal string into limbs"),
        sign,
    )
}

/// Scale scalar by the given scale factor. Negative scaling is not allowed.
/// Note that we do not check for overflow.
pub(crate) fn scale_scalar<S: Scalar>(s: S, scale: i8) -> ConversionResult<S> {
    if scale < 0 {
        return Err(ConversionError::DecimalRoundingError(
            "Scale factor must be non-negative".to_string(),
        ));
    }
    let ten = S::from(10);
    let mut res = s;
    for _ in 0..scale {
        res *= ten;
    }
    Ok(res)
}

#[cfg(test)]
mod scale_adjust_test {

    use super::*;

    #[test]
    fn we_cannot_scale_past_max_precision() {
        let decimal = DecimalUnknown::new(
            "12345678901234567890123456789012345678901234567890123456789012345678900.0",
        );
        assert_eq!(decimal.scale(), 1);
        let target_scale = 30;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_err());
    }

    #[test]
    fn we_can_match_exact_decimals_from_queries_to_db() {
        let decimal = DecimalUnknown::new("123.45");
        let target_scale = 2;
        let (limbs, sign) = get_limbs_and_sign(&decimal, target_scale).unwrap();
        assert_eq!(limbs, [12345, 0, 0, 0]);
        assert_eq!(sign, Sign::Plus);
    }

    #[test]
    fn we_can_match_decimals_by_scaling_up() {
        let decimal = DecimalUnknown::new("123.45");
        let target_scale = 3;
        let (limbs, sign) = get_limbs_and_sign(&decimal, target_scale).unwrap();
        assert_eq!(limbs, [123450, 0, 0, 0]);
        assert_eq!(sign, Sign::Plus);
    }

    #[test]
    fn we_can_match_integers_by_scaling_up() {
        let decimal = DecimalUnknown::new("12345");
        let target_scale = 2;
        let (limbs, sign) = get_limbs_and_sign(&decimal, target_scale).unwrap();
        assert_eq!(limbs, [1234500, 0, 0, 0]);
        assert_eq!(sign, Sign::Plus);
    }

    #[test]
    fn we_can_match_negative_decimals() {
        let decimal = DecimalUnknown::new("-123.45");
        let target_scale = 2;
        let (limbs, sign) = get_limbs_and_sign(&decimal, target_scale).unwrap();
        assert_eq!(limbs, [12345, 0, 0, 0]);
        assert_eq!(sign, Sign::Minus);
    }

    #[test]
    fn we_cannot_scale_down_to_match_decimals() {
        let decimal = DecimalUnknown::new("361.0004");
        let target_scale = 1;
        // matching down would equate to rounding down which we dont support yet
        assert!(get_limbs_and_sign(&decimal, target_scale).is_err());
    }

    #[test]
    fn we_can_match_decimals_at_extrema() {
        // a big decimal cannot scale up past the supported precision
        let decimal = DecimalUnknown::new(
            "1234567890123456789012345678901234567890123456789012345678901234567890.0",
        );
        let target_scale = 30;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_err());

        // maximum decimal value we can support
        let decimal = DecimalUnknown::new(
            "99999999999999999999999999999999999999999999999999999999999999999999999999.0",
        );
        let target_scale = 1;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_ok());

        // scaling larger than max will fail
        let decimal = DecimalUnknown::new(
            "99999999999999999999999999999999999999999999999999999999999999999999999999.0",
        );
        let target_scale = 2;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_err());

        // smallest possible decimal value we can support (either signed/unsigned)
        let decimal = DecimalUnknown::new(
            "0.00000000000000000000000000000000000000000000000000000000000000000000000001",
        );
        // - 1 because of leading zero counting towards precision
        let target_scale = MAX_SUPPORTED_PRECISION as i8 - 1;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_ok());

        // this scales up to boundary successfully
        let decimal = DecimalUnknown::new(
            "0.0000000000000000000000000000000000000000000000000000000000000000000000001",
        );
        let target_scale = MAX_SUPPORTED_PRECISION as i8 - 1;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_ok());

        // this exceeds supported precision
        let decimal = DecimalUnknown::new(
            "0.000000000000000000000000000000000000000000000000000000000000000000000000001",
        );
        let target_scale = MAX_SUPPORTED_PRECISION as i8 - 1;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_err());

        // this is ok because it can be scaled to 75 precision and trailing
        // zeros do not count towards precision
        let decimal = DecimalUnknown::new(
            "0.000000000000000000000000000000000000000000000000000000000000000000000000010",
        );
        let target_scale = MAX_SUPPORTED_PRECISION as i8 - 1;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_ok());

        // this is ok because of trailing zeros
        let decimal = DecimalUnknown::new(
            "99999999999999999999999999999999999999999999999999999999999999999999999999.00000",
        );
        let target_scale = 1;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_ok());

        // this exceeds max precision
        let decimal = DecimalUnknown::new(
            "999999999999999999999999999999999999999999999999999999999999999999999999999.1",
        );
        let target_scale = 2;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_err());

        // this exceeds max precision
        let decimal = DecimalUnknown::new("1.0");
        let target_scale = 75;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_err());

        // but this is ok
        let decimal = DecimalUnknown::new("1.0");
        let target_scale = 74;
        assert!(get_limbs_and_sign(&decimal, target_scale).is_ok());
    }
}

#[cfg(test)]
pub mod limb_tests {

    use crate::base::{
        math::decimal::decimal_string_to_scaled_limbs,
        scalar::{Curve25519Scalar, Scalar},
    };
    use proofs_sql::decimal_unknown::DecimalUnknown;

    #[test]
    fn we_can_convert_a_large_decimal_to_limbs() {
        let dec_mid = decimal_string_to_scaled_limbs(
            &DecimalUnknown::new(
                "11579208923731619542357098500868790785.3269984665640564039457584007913129639935",
            ),
            None,
        );
        let dec_trailing = decimal_string_to_scaled_limbs(
            &DecimalUnknown::new(
                "115792089237316195423570985008687907853269984665640564039457584007913129639935.0",
            ),
            None,
        );
        let dec_leading = decimal_string_to_scaled_limbs(
            &DecimalUnknown::new(
                "11579208923731619542357098500868790785.3269984665640564039457584007913129639935",
            ),
            None,
        );

        // max-width i256 is ok for limbs
        let expected: [u64; 4] = [
            0xFFFFFFFFFFFFFFFF,
            0xFFFFFFFFFFFFFFFF,
            0xFFFFFFFFFFFFFFFF,
            0xFFFFFFFFFFFFFFFF,
        ];

        assert_eq!(dec_mid.0, expected);
        assert_eq!(dec_trailing.0, expected);
        assert_eq!(dec_leading.0, expected);
    }

    #[test]
    fn we_can_convert_a_small_decimal_to_limbs() {
        let dec = DecimalUnknown::new("123.456");
        let integer_result = decimal_string_to_scaled_limbs(&dec, None);
        let expected: [u64; 4] = [123456, 0, 0, 0];
        assert!(integer_result.0 == expected);
    }

    #[test]
    fn we_can_convert_decimals_correctly_at_curve25519scalar_boundaries() {
        // Test that we parse max signed correctly
        let integer_result = decimal_string_to_scaled_limbs(
            &DecimalUnknown::new(
                "3618502788666131106986593281521497120428558179689953803000975469142727125494",
            ),
            None,
        );

        assert_eq!(
            Curve25519Scalar::from(integer_result.0),
            Curve25519Scalar::MAX_SIGNED
        );

        // Test that we parse min signed +1 properly
        let integer_result = decimal_string_to_scaled_limbs(
            &DecimalUnknown::new(
                "7237005577332262213973186563042994240857116359379907606001950938285454250989",
            ),
            None,
        );

        assert_eq!(
            Curve25519Scalar::from(integer_result.0),
            Curve25519Scalar::ZERO
        );

        // Test that we parse inverses correctly for -1 = p -1
        let integer_result = decimal_string_to_scaled_limbs(
            &DecimalUnknown::new(
                /* curve order Fr, min signed value */
                "7237005577332262213973186563042994240857116359379907606001950938285454250988",
            ),
            None,
        );
        assert_eq!(
            (Curve25519Scalar::ZERO - Curve25519Scalar::ONE),
            Curve25519Scalar::from(integer_result.0)
        );

        // Test that Fr + 1 is correct
        let integer_result = decimal_string_to_scaled_limbs(
            &DecimalUnknown::new(
                /* curve order Fr */
                "7237005577332262213973186563042994240857116359379907606001950938285454250988",
            ),
            None,
        );

        // Test that curve order + 1 = 0
        assert_eq!(
            (Curve25519Scalar::ZERO),
            Curve25519Scalar::from(integer_result.0) + Curve25519Scalar::ONE
        );
    }
}

#[cfg(test)]
mod conversion_tests {
    use super::*;
    use crate::base::scalar::Curve25519Scalar;
    macro_rules! precision {
        ($value:expr) => {
            Precision::new($value).unwrap()
        };
    }

    #[test]
    fn we_can_scale_scalars_up() {
        let s = Curve25519Scalar::from(1);
        assert_eq!(scale_scalar(s, 0).unwrap(), s);
        assert_eq!(scale_scalar(s, 1).unwrap(), Curve25519Scalar::from(10));
        assert_eq!(scale_scalar(s, 2).unwrap(), Curve25519Scalar::from(100));
        assert_eq!(scale_scalar(s, 3).unwrap(), Curve25519Scalar::from(1000));
        let s = Curve25519Scalar::from(-1);
        assert_eq!(scale_scalar(s, 0).unwrap(), s);
        assert_eq!(scale_scalar(s, 1).unwrap(), Curve25519Scalar::from(-10));
        assert_eq!(scale_scalar(s, 2).unwrap(), Curve25519Scalar::from(-100));
        assert_eq!(scale_scalar(s, 3).unwrap(), Curve25519Scalar::from(-1000));
    }

    #[test]
    fn we_can_not_scale_scalars_down() {
        let s = Curve25519Scalar::from(1);
        matches!(
            scale_scalar(s, -1),
            Err(ConversionError::DecimalRoundingError(_))
        );
    }

    #[test]
    fn we_can_not_reduce_decimal_scale() {
        let s = Curve25519Scalar::from(100);
        let d = Decimal::new(s, precision!(19), 4);
        matches!(
            d.with_precision_and_scale(precision!(18), 3),
            Err(ConversionError::DecimalRoundingError(_))
        );
    }

    #[test]
    fn we_can_not_inadequately_increase_decimal_precision_by_scaling() {
        let s = Curve25519Scalar::from(100);
        let d = Decimal::new(s, precision!(19), 4);
        matches!(
            d.with_precision_and_scale(precision!(21), 7),
            Err(ConversionError::DecimalRoundingError(_))
        );
    }

    #[test]
    fn we_can_increase_decimal_precision_by_scaling() {
        let s = Curve25519Scalar::from(-134);
        let d = Decimal::new(s, precision!(19), 4);
        let expected = Decimal::new(Curve25519Scalar::from(-13400000), precision!(25), 9);
        let actual = d.with_precision_and_scale(precision!(25), 9).unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn we_can_scale_decimals_to_almost_limit() {
        let s = Curve25519Scalar::from(-134);
        let d = Decimal::new(s, precision!(3), 0);
        let expected = Decimal::new(
            Curve25519Scalar::from(-13400)
                * Curve25519Scalar::from(10_000_000_000_i64)
                * Curve25519Scalar::from(10_000_000_000_i64)
                * Curve25519Scalar::from(10_000_000_000_i64)
                * Curve25519Scalar::from(10_000_000_000_i64)
                * Curve25519Scalar::from(10_000_000_000_i64)
                * Curve25519Scalar::from(10_000_000_000_i64)
                * Curve25519Scalar::from(10_000_000_000_i64),
            precision!(75),
            72,
        );
        println!("{:?}", expected);
        let actual = d.with_precision_and_scale(precision!(75), 72).unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn we_can_convert_i64_to_decimal() {
        let decimal: Decimal<Curve25519Scalar> =
            Decimal::from_i64(-91_i64, precision!(21), 2_i8).unwrap();
        assert_eq!(decimal.value, Curve25519Scalar::from(-9100));
    }

    #[test]
    fn we_can_not_scale_i64_if_rounding_required() {
        let res_decimal: ConversionResult<Decimal<Curve25519Scalar>> =
            Decimal::from_i64(1_i64, precision!(65), 57_i8);
        matches!(res_decimal, Err(ConversionError::DecimalRoundingError(_)));
    }

    #[test]
    fn we_can_convert_i128_to_decimal() {
        let decimal: Decimal<Curve25519Scalar> =
            Decimal::from_i128(12_i128, precision!(43), 4_i8).unwrap();
        assert_eq!(decimal.value, Curve25519Scalar::from(120000));
    }

    #[test]
    fn we_can_not_scale_i128_if_rounding_required() {
        let res_decimal: ConversionResult<Decimal<Curve25519Scalar>> =
            Decimal::from_i128(1_i128, precision!(43), 27_i8);
        matches!(res_decimal, Err(ConversionError::DecimalRoundingError(_)));
    }
}
