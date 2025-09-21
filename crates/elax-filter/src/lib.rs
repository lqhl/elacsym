//! Filter evaluation primitives for structured predicates.

use std::collections::HashSet;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Structured filter expression accepted by the query planner.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FilterExpr {
    /// Logical conjunction – every operand must evaluate to `true`.
    And(Vec<FilterExpr>),
    /// Logical disjunction – at least one operand must evaluate to `true`.
    Or(Vec<FilterExpr>),
    /// Logical negation of a nested expression.
    Not(Box<FilterExpr>),
    /// Strict equality against a scalar value.
    Eq { field: String, value: Value },
    /// Negated equality against a scalar value.
    NotEq { field: String, value: Value },
    /// Membership against a list of values.
    In { field: String, values: Vec<Value> },
    /// Negated membership against a list of values.
    NotIn { field: String, values: Vec<Value> },
    /// Compare whether a field is greater than the provided scalar.
    Gt { field: String, value: f64 },
    /// Compare whether a field is greater than or equal to the provided scalar.
    Gte { field: String, value: f64 },
    /// Compare whether a field is less than the provided scalar.
    Lt { field: String, value: f64 },
    /// Compare whether a field is less than or equal to the provided scalar.
    Lte { field: String, value: f64 },
    /// Checks whether a field path exists and is not `null`.
    Exists { field: String },
}

/// Bitmap of document identifiers that satisfy a filter expression.
#[derive(Clone, Debug, Default)]
pub struct FilterBitmap {
    members: HashSet<String>,
}

impl FilterBitmap {
    /// Construct an empty bitmap.
    pub fn new() -> Self {
        Self {
            members: HashSet::new(),
        }
    }

    /// Build a bitmap from the provided identifier iterator.
    pub fn from_ids<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut members = HashSet::new();
        for id in ids {
            members.insert(id.into());
        }
        Self { members }
    }

    /// Number of identifiers in the bitmap.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether the bitmap tracks any identifiers.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Returns `true` when the identifier is contained in the bitmap.
    pub fn contains(&self, id: &str) -> bool {
        self.members.contains(id)
    }

    /// In-place intersection with another bitmap.
    pub fn intersect(&mut self, other: &FilterBitmap) {
        self.members.retain(|id| other.members.contains(id));
    }
}

/// Evaluate a filter expression against the provided attributes and identifier.
pub fn evaluate(filter: &FilterExpr, attributes: Option<&Value>, id: Option<&str>) -> Result<bool> {
    match filter {
        FilterExpr::And(exprs) => {
            for expr in exprs {
                if !evaluate(expr, attributes, id)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        FilterExpr::Or(exprs) => {
            for expr in exprs {
                if evaluate(expr, attributes, id)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        FilterExpr::Not(expr) => evaluate(expr, attributes, id).map(|value| !value),
        FilterExpr::Eq { field, value } => Ok(match field.as_str() {
            "id" => match (id, value.as_str()) {
                (Some(actual), Some(expected)) => actual == expected,
                _ => false,
            },
            _ => compare_json(field, value, attributes),
        }),
        FilterExpr::NotEq { field, value } => Ok(match field.as_str() {
            "id" => match (id, value.as_str()) {
                (Some(actual), Some(expected)) => actual != expected,
                _ => true,
            },
            _ => !compare_json(field, value, attributes),
        }),
        FilterExpr::In { field, values } => Ok(match field.as_str() {
            "id" => match id {
                Some(actual) => values.iter().any(|value| value.as_str() == Some(actual)),
                None => false,
            },
            _ => compare_membership(field, values, attributes),
        }),
        FilterExpr::NotIn { field, values } => Ok(match field.as_str() {
            "id" => match id {
                Some(actual) => !values.iter().any(|value| value.as_str() == Some(actual)),
                None => true,
            },
            _ => !compare_membership(field, values, attributes),
        }),
        FilterExpr::Gt { field, value } => {
            compare_numeric(field, *value, attributes, |actual, needle| actual > needle)
        }
        FilterExpr::Gte { field, value } => {
            compare_numeric(field, *value, attributes, |actual, needle| actual >= needle)
        }
        FilterExpr::Lt { field, value } => {
            compare_numeric(field, *value, attributes, |actual, needle| actual < needle)
        }
        FilterExpr::Lte { field, value } => {
            compare_numeric(field, *value, attributes, |actual, needle| actual <= needle)
        }
        FilterExpr::Exists { field } => Ok(match field.as_str() {
            "id" => id.is_some(),
            _ => fetch_field(attributes, field).is_some(),
        }),
    }
}

fn compare_json(field: &str, value: &Value, attributes: Option<&Value>) -> bool {
    match fetch_field(attributes, field) {
        Some(actual) => actual == value,
        None => false,
    }
}

fn compare_membership(field: &str, values: &[Value], attributes: Option<&Value>) -> bool {
    match fetch_field(attributes, field) {
        Some(actual) => values.iter().any(|candidate| candidate == actual),
        None => false,
    }
}

fn compare_numeric<F>(
    field: &str,
    needle: f64,
    attributes: Option<&Value>,
    predicate: F,
) -> Result<bool>
where
    F: Fn(f64, f64) -> bool,
{
    let Some(actual) = fetch_field(attributes, field) else {
        return Ok(false);
    };
    let Some(value) = actual.as_f64() else {
        return Err(anyhow!("field {field} is not a number"));
    };
    Ok(predicate(value, needle))
}

fn fetch_field<'a>(attributes: Option<&'a Value>, field: &str) -> Option<&'a Value> {
    let mut current = attributes?;
    for part in field.split('.') {
        let object = current.as_object()?;
        current = object.get(part)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn equality_matches_strings() {
        let filter = FilterExpr::Eq {
            field: "category".to_string(),
            value: Value::String("news".to_string()),
        };
        let attrs = json!({"category": "news"});
        assert!(evaluate(&filter, Some(&attrs), None).unwrap());
    }

    #[test]
    fn membership_handles_missing_fields() {
        let filter = FilterExpr::In {
            field: "category".to_string(),
            values: vec![Value::String("news".into())],
        };
        let attrs = json!({"kind": "news"});
        assert!(!evaluate(&filter, Some(&attrs), None).unwrap());
    }

    #[test]
    fn numeric_comparisons_fail_for_non_numbers() {
        let filter = FilterExpr::Gt {
            field: "score".to_string(),
            value: 10.0,
        };
        let attrs = json!({"score": "ten"});
        assert!(evaluate(&filter, Some(&attrs), None).is_err());
    }
}
