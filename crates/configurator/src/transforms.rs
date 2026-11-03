//! Named pure value transforms, shared by schema `exportTransform`s and the
//! `transform` migration op. Names are an author-facing contract
//! (`docs/DEVELOPER_MIGRATION.md` §2): add freely, never rename.

use serde_json::{Value, json};

use crate::error::{ConfiguratorError, Result};

pub const BUILTIN_TRANSFORMS: &[&str] = &[
    "mhzToKhzInt",
    "boolToInt",
    "enumToIndex",
    "degToRad",
    "feetToMeters",
    "metersToFeet",
];

pub fn apply_transform(name: &str, value: &Value) -> Result<Value> {
    let fail = |msg: &str| ConfiguratorError::TransformFailed {
        transform: name.to_owned(),
        message: msg.to_owned(),
    };
    let number = || value.as_f64().ok_or_else(|| fail("expected number"));

    match name {
        // 122.4 MHz -> 122400. Rounding (not truncation) keeps 118.025 from
        // landing on 118024 through float error.
        "mhzToKhzInt" => Ok(json!((number()? * 1000.0).round() as i64)),
        "boolToInt" => {
            let b = value.as_bool().ok_or_else(|| fail("expected bool"))?;
            Ok(json!(i64::from(b)))
        }
        // The option lookup happens at the schema layer; here a resolved
        // index passes through so chained transforms still work.
        "enumToIndex" => value.as_i64().map(|n| json!(n)).ok_or_else(|| {
            fail("enumToIndex expects a pre-resolved option index; resolve at schema layer first")
        }),
        "degToRad" => Ok(json!(number()?.to_radians())),
        "feetToMeters" => Ok(json!(number()? * 0.3048)),
        "metersToFeet" => Ok(json!(number()? / 0.3048)),
        other => Err(ConfiguratorError::UnknownTransform(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mhz_to_khz_rounds() {
        assert_eq!(
            apply_transform("mhzToKhzInt", &json!(122.4)).unwrap(),
            json!(122_400)
        );
        assert_eq!(
            apply_transform("mhzToKhzInt", &json!(118.025)).unwrap(),
            json!(118_025)
        );
    }

    #[test]
    fn bool_to_int() {
        assert_eq!(
            apply_transform("boolToInt", &json!(true)).unwrap(),
            json!(1)
        );
        assert_eq!(
            apply_transform("boolToInt", &json!(false)).unwrap(),
            json!(0)
        );
        assert!(apply_transform("boolToInt", &json!(1)).is_err());
    }

    #[test]
    fn enum_to_index_needs_an_index() {
        assert_eq!(apply_transform("enumToIndex", &json!(2)).unwrap(), json!(2));
        assert!(apply_transform("enumToIndex", &json!("ILS")).is_err());
    }

    #[test]
    fn unit_conversions() {
        let m = apply_transform("feetToMeters", &json!(100.0)).unwrap();
        assert!((m.as_f64().unwrap() - 30.48).abs() < 1e-9);
        let ft = apply_transform("metersToFeet", &json!(30.48)).unwrap();
        assert!((ft.as_f64().unwrap() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn every_builtin_is_known() {
        for name in BUILTIN_TRANSFORMS {
            assert!(!matches!(
                apply_transform(name, &json!(1)),
                Err(ConfiguratorError::UnknownTransform(_))
            ));
        }
        assert!(matches!(
            apply_transform("nope", &json!(0)),
            Err(ConfiguratorError::UnknownTransform(_))
        ));
    }
}
