//! Query executor for attribute filtering

use std::collections::HashSet;

use crate::query::{FilterCondition, FilterExpression, FilterOp};
use crate::segment::{SegmentReader, SegmentWriter};
use crate::storage::StorageBackend;
use crate::types::{AttributeValue, SegmentInfo, Schema};
use crate::{Error, Result};

/// Filter executor for applying attribute filters on segments
pub struct FilterExecutor;

impl FilterExecutor {
    /// Apply filter to segments and return matching document IDs
    pub async fn apply_filter(
        segments: &[SegmentInfo],
        filter: &FilterExpression,
        schema: &Schema,
        storage: &dyn StorageBackend,
    ) -> Result<HashSet<u64>> {
        let mut matching_ids = HashSet::new();

        for segment_info in segments {
            // Load segment data
            let segment_data = storage.get(&segment_info.file_path).await?;

            // Create reader
            let arrow_schema = SegmentWriter::new(schema.clone())?.arrow_schema;
            let reader = SegmentReader::new(arrow_schema);

            // Read all documents from segment
            let documents = reader.read_parquet(segment_data)?;

            // Apply filter to each document
            for doc in documents {
                if Self::evaluate_filter(filter, &doc.attributes)? {
                    matching_ids.insert(doc.id);
                }
            }
        }

        Ok(matching_ids)
    }

    /// Evaluate filter expression on document attributes
    fn evaluate_filter(
        filter: &FilterExpression,
        attributes: &std::collections::HashMap<String, AttributeValue>,
    ) -> Result<bool> {
        match filter {
            FilterExpression::And { conditions } => {
                for condition in conditions {
                    if !Self::evaluate_condition(condition, attributes)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            FilterExpression::Or { conditions } => {
                for condition in conditions {
                    if Self::evaluate_condition(condition, attributes)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    /// Evaluate a single filter condition
    fn evaluate_condition(
        condition: &FilterCondition,
        attributes: &std::collections::HashMap<String, AttributeValue>,
    ) -> Result<bool> {
        let field_value = attributes.get(&condition.field);

        match &condition.op {
            FilterOp::Eq => {
                if let Some(field_value) = field_value {
                    Ok(Self::values_equal(field_value, &condition.value))
                } else {
                    Ok(false)
                }
            }
            FilterOp::Ne => {
                if let Some(field_value) = field_value {
                    Ok(!Self::values_equal(field_value, &condition.value))
                } else {
                    Ok(true)
                }
            }
            FilterOp::Gt => {
                if let Some(field_value) = field_value {
                    Self::compare_values(field_value, &condition.value, |a, b| a > b)
                } else {
                    Ok(false)
                }
            }
            FilterOp::Gte => {
                if let Some(field_value) = field_value {
                    Self::compare_values(field_value, &condition.value, |a, b| a >= b)
                } else {
                    Ok(false)
                }
            }
            FilterOp::Lt => {
                if let Some(field_value) = field_value {
                    Self::compare_values(field_value, &condition.value, |a, b| a < b)
                } else {
                    Ok(false)
                }
            }
            FilterOp::Lte => {
                if let Some(field_value) = field_value {
                    Self::compare_values(field_value, &condition.value, |a, b| a <= b)
                } else {
                    Ok(false)
                }
            }
            FilterOp::Contains => {
                if let Some(AttributeValue::StringArray(arr)) = field_value {
                    if let AttributeValue::String(search) = &condition.value {
                        Ok(arr.contains(search))
                    } else {
                        Ok(false)
                    }
                } else {
                    Ok(false)
                }
            }
            FilterOp::ContainsAny => {
                if let Some(AttributeValue::StringArray(arr)) = field_value {
                    if let AttributeValue::StringArray(search_terms) = &condition.value {
                        Ok(arr.iter().any(|item| search_terms.contains(item)))
                    } else {
                        Ok(false)
                    }
                } else {
                    Ok(false)
                }
            }
        }
    }

    /// Check if two attribute values are equal
    fn values_equal(a: &AttributeValue, b: &AttributeValue) -> bool {
        match (a, b) {
            (AttributeValue::String(a), AttributeValue::String(b)) => a == b,
            (AttributeValue::Integer(a), AttributeValue::Integer(b)) => a == b,
            (AttributeValue::Float(a), AttributeValue::Float(b)) => {
                // Float comparison with epsilon
                (a - b).abs() < f64::EPSILON
            }
            (AttributeValue::Boolean(a), AttributeValue::Boolean(b)) => a == b,
            (AttributeValue::StringArray(a), AttributeValue::StringArray(b)) => a == b,
            _ => false,
        }
    }

    /// Compare two attribute values using a comparison function
    fn compare_values<F>(a: &AttributeValue, b: &AttributeValue, cmp: F) -> Result<bool>
    where
        F: Fn(f64, f64) -> bool,
    {
        match (a, b) {
            (AttributeValue::Integer(a), AttributeValue::Integer(b)) => {
                Ok(cmp(*a as f64, *b as f64))
            }
            (AttributeValue::Float(a), AttributeValue::Float(b)) => Ok(cmp(*a, *b)),
            (AttributeValue::Integer(a), AttributeValue::Float(b)) => Ok(cmp(*a as f64, *b)),
            (AttributeValue::Float(a), AttributeValue::Integer(b)) => Ok(cmp(*a, *b as f64)),
            _ => Err(Error::InvalidRequest(format!(
                "Cannot compare values of different types: {:?} and {:?}",
                a, b
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AttributeSchema, AttributeType, DistanceMetric, Document};
    use crate::storage::local::LocalStorage;
    use tempfile::TempDir;
    use std::collections::HashMap;

    #[test]
    fn test_evaluate_condition_eq() {
        let mut attributes = HashMap::new();
        attributes.insert(
            "category".to_string(),
            AttributeValue::String("tech".to_string()),
        );

        let condition = FilterCondition {
            field: "category".to_string(),
            op: FilterOp::Eq,
            value: AttributeValue::String("tech".to_string()),
        };

        assert!(FilterExecutor::evaluate_condition(&condition, &attributes).unwrap());

        let condition_fail = FilterCondition {
            field: "category".to_string(),
            op: FilterOp::Eq,
            value: AttributeValue::String("sports".to_string()),
        };

        assert!(!FilterExecutor::evaluate_condition(&condition_fail, &attributes).unwrap());
    }

    #[test]
    fn test_evaluate_condition_gte() {
        let mut attributes = HashMap::new();
        attributes.insert("score".to_string(), AttributeValue::Float(4.5));

        let condition = FilterCondition {
            field: "score".to_string(),
            op: FilterOp::Gte,
            value: AttributeValue::Float(4.0),
        };

        assert!(FilterExecutor::evaluate_condition(&condition, &attributes).unwrap());

        let condition_fail = FilterCondition {
            field: "score".to_string(),
            op: FilterOp::Gte,
            value: AttributeValue::Float(5.0),
        };

        assert!(!FilterExecutor::evaluate_condition(&condition_fail, &attributes).unwrap());
    }

    #[test]
    fn test_evaluate_condition_contains() {
        let mut attributes = HashMap::new();
        attributes.insert(
            "tags".to_string(),
            AttributeValue::StringArray(vec!["rust".to_string(), "database".to_string()]),
        );

        let condition = FilterCondition {
            field: "tags".to_string(),
            op: FilterOp::Contains,
            value: AttributeValue::String("rust".to_string()),
        };

        assert!(FilterExecutor::evaluate_condition(&condition, &attributes).unwrap());

        let condition_fail = FilterCondition {
            field: "tags".to_string(),
            op: FilterOp::Contains,
            value: AttributeValue::String("python".to_string()),
        };

        assert!(!FilterExecutor::evaluate_condition(&condition_fail, &attributes).unwrap());
    }

    #[test]
    fn test_evaluate_filter_and() {
        let mut attributes = HashMap::new();
        attributes.insert(
            "category".to_string(),
            AttributeValue::String("tech".to_string()),
        );
        attributes.insert("score".to_string(), AttributeValue::Float(4.5));

        let filter = FilterExpression::And {
            conditions: vec![
                FilterCondition {
                    field: "category".to_string(),
                    op: FilterOp::Eq,
                    value: AttributeValue::String("tech".to_string()),
                },
                FilterCondition {
                    field: "score".to_string(),
                    op: FilterOp::Gte,
                    value: AttributeValue::Float(4.0),
                },
            ],
        };

        assert!(FilterExecutor::evaluate_filter(&filter, &attributes).unwrap());
    }

    #[test]
    fn test_evaluate_filter_or() {
        let mut attributes = HashMap::new();
        attributes.insert(
            "category".to_string(),
            AttributeValue::String("sports".to_string()),
        );
        attributes.insert("score".to_string(), AttributeValue::Float(4.5));

        let filter = FilterExpression::Or {
            conditions: vec![
                FilterCondition {
                    field: "category".to_string(),
                    op: FilterOp::Eq,
                    value: AttributeValue::String("tech".to_string()),
                },
                FilterCondition {
                    field: "score".to_string(),
                    op: FilterOp::Gte,
                    value: AttributeValue::Float(4.0),
                },
            ],
        };

        assert!(FilterExecutor::evaluate_filter(&filter, &attributes).unwrap());
    }
}
