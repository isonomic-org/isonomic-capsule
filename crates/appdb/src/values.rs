// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Wire values to and from the database's binary format.
//!
//! A parameter takes the type the database inferred for its placeholder;
//! a column reports the database's own type name. Types without a natural
//! wire value (numeric, intervals, arrays, …) are refused with a message
//! that says to cast them in SQL (`$1::text`, `amount::text`).

use std::error::Error;

use bytes::BytesMut;
use isonomic_proto::{Bytes, SqlValue};
use tokio_postgres::types::{to_sql_checked, FromSql, IsNull, ToSql, Type};

type BoxError = Box<dyn Error + Sync + Send>;

fn unsupported(what: &str, ty: &Type) -> BoxError {
    format!("{what} of type {ty} is not supported; cast it to text in SQL").into()
}

fn is_text(ty: &Type) -> bool {
    [
        Type::TEXT,
        Type::VARCHAR,
        Type::BPCHAR,
        Type::NAME,
        Type::UNKNOWN,
        Type::JSON,
    ]
    .contains(ty)
}

/// A parameter, encoded for the type its placeholder has.
#[derive(Debug)]
pub(crate) struct Param<'a>(pub &'a SqlValue);

impl ToSql for Param<'_> {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, BoxError> {
        match (self.0, ty) {
            (SqlValue::Null, _) => Ok(IsNull::Yes),
            (SqlValue::Bool(v), &Type::BOOL) => v.to_sql(ty, out),
            (SqlValue::Int(v), &Type::INT8) => v.to_sql(ty, out),
            (SqlValue::Int(v), &Type::INT4) => i32::try_from(*v)?.to_sql(ty, out),
            (SqlValue::Int(v), &Type::INT2) => i16::try_from(*v)?.to_sql(ty, out),
            (SqlValue::Int(v), &Type::OID) => u32::try_from(*v)?.to_sql(ty, out),
            (SqlValue::Int(v), &Type::FLOAT8) => (*v as f64).to_sql(ty, out),
            (SqlValue::Int(v), &Type::FLOAT4) => (*v as f32).to_sql(ty, out),
            (SqlValue::Float(v), &Type::FLOAT8) => v.to_sql(ty, out),
            (SqlValue::Float(v), &Type::FLOAT4) => (*v as f32).to_sql(ty, out),
            (SqlValue::Text(v), ty) if is_text(ty) => {
                out.extend_from_slice(v.as_bytes());
                Ok(IsNull::No)
            }
            (SqlValue::Text(v), &Type::JSONB) => {
                out.extend_from_slice(&[1]);
                out.extend_from_slice(v.as_bytes());
                Ok(IsNull::No)
            }
            (SqlValue::Bytes { bytes }, &Type::BYTEA) => bytes.0.as_slice().to_sql(ty, out),
            (value, ty) => Err(format!(
                "a {} value cannot be a parameter of type {ty}; cast the placeholder in SQL",
                kind(value)
            )
            .into()),
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

fn kind(value: &SqlValue) -> &'static str {
    match value {
        SqlValue::Null => "null",
        SqlValue::Bool(_) => "boolean",
        SqlValue::Int(_) => "integer",
        SqlValue::Float(_) => "float",
        SqlValue::Text(_) => "text",
        SqlValue::Bytes { .. } => "bytes",
    }
}

/// A result value, decoded from its column's type.
pub(crate) struct Cell(pub SqlValue);

impl<'a> FromSql<'a> for Cell {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, BoxError> {
        let value = match *ty {
            Type::BOOL => SqlValue::Bool(bool::from_sql(ty, raw)?),
            Type::INT2 => SqlValue::Int(i16::from_sql(ty, raw)?.into()),
            Type::INT4 => SqlValue::Int(i32::from_sql(ty, raw)?.into()),
            Type::INT8 => SqlValue::Int(i64::from_sql(ty, raw)?),
            Type::OID => SqlValue::Int(u32::from_sql(ty, raw)?.into()),
            Type::FLOAT4 => SqlValue::Float(f32::from_sql(ty, raw)?.into()),
            Type::FLOAT8 => SqlValue::Float(f64::from_sql(ty, raw)?),
            Type::BYTEA => SqlValue::Bytes {
                bytes: Bytes(raw.to_vec()),
            },
            Type::JSONB => match raw.split_first() {
                Some((1, rest)) => SqlValue::Text(std::str::from_utf8(rest)?.to_string()),
                _ => return Err("unknown jsonb version".into()),
            },
            Type::UUID => SqlValue::Text(uuid(raw)?),
            Type::TIMESTAMP => SqlValue::Text(timestamp(i64::from_sql(ty, raw)?, "")),
            Type::TIMESTAMPTZ => SqlValue::Text(timestamp(i64::from_sql(ty, raw)?, "Z")),
            Type::DATE => SqlValue::Text(date(i32::from_sql(ty, raw)?.into())),
            ref ty if is_text(ty) => SqlValue::Text(std::str::from_utf8(raw)?.to_string()),
            ref ty => return Err(unsupported("a column", ty)),
        };
        Ok(Cell(value))
    }

    fn from_sql_null(_: &Type) -> Result<Self, BoxError> {
        Ok(Cell(SqlValue::Null))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

fn uuid(raw: &[u8]) -> Result<String, BoxError> {
    if raw.len() != 16 {
        return Err("a uuid is 16 bytes".into());
    }
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// Days since 2000-01-01, the database's epoch, as `YYYY-MM-DD`.
fn date(days: i64) -> String {
    // Days from 1970-01-01 to 2000-01-01.
    let (y, m, d) = civil(days + 10_957);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Microseconds since 2000-01-01 as an ISO 8601 timestamp.
fn timestamp(micros: i64, zone: &str) -> String {
    let days = micros.div_euclid(86_400_000_000);
    let rest = micros.rem_euclid(86_400_000_000);
    let (h, rest) = (rest / 3_600_000_000, rest % 3_600_000_000);
    let (min, rest) = (rest / 60_000_000, rest % 60_000_000);
    let (s, us) = (rest / 1_000_000, rest % 1_000_000);
    format!("{}T{h:02}:{min:02}:{s:02}.{us:06}{zone}", date(days))
}

/// The civil date of a day count from 1970-01-01 (proleptic Gregorian).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_times_count_from_the_databases_epoch() {
        assert_eq!(date(0), "2000-01-01");
        assert_eq!(date(-1), "1999-12-31");
        assert_eq!(date(9815), "2026-11-15");
        assert_eq!(date(60), "2000-03-01");
        let micros = (9815 * 86_400 + 21 * 3600 + 5 * 60 + 9) * 1_000_000 + 42;
        assert_eq!(timestamp(micros, "Z"), "2026-11-15T21:05:09.000042Z");
        assert_eq!(timestamp(-1, ""), "1999-12-31T23:59:59.999999");
    }

    #[test]
    fn values_encode_for_their_placeholder_and_refuse_a_mismatch() {
        let mut out = BytesMut::new();
        let int = SqlValue::Int(7);
        assert!(Param(&int).to_sql(&Type::INT4, &mut out).is_ok());
        assert_eq!(&out[..], &[0, 0, 0, 7]);
        let big = SqlValue::Int(1 << 40);
        assert!(Param(&big)
            .to_sql(&Type::INT4, &mut BytesMut::new())
            .is_err());
        let text = SqlValue::Text("x".into());
        assert!(Param(&text)
            .to_sql(&Type::INT8, &mut BytesMut::new())
            .is_err());
        assert!(matches!(
            Param(&SqlValue::Null).to_sql(&Type::NUMERIC, &mut BytesMut::new()),
            Ok(IsNull::Yes)
        ));
    }

    #[test]
    fn cells_decode_by_column_type() {
        let Cell(v) = Cell::from_sql(&Type::INT8, &5i64.to_be_bytes()).unwrap();
        assert_eq!(v, SqlValue::Int(5));
        let Cell(v) = Cell::from_sql(&Type::JSONB, b"\x01{\"a\":1}").unwrap();
        assert_eq!(v, SqlValue::Text("{\"a\":1}".into()));
        let raw = [0x12u8; 16];
        let Cell(v) = Cell::from_sql(&Type::UUID, &raw).unwrap();
        assert_eq!(
            v,
            SqlValue::Text("12121212-1212-1212-1212-121212121212".into())
        );
        assert!(Cell::from_sql(&Type::NUMERIC, &[0; 8]).is_err());
    }
}
