//! Recursive metadata filter AST. `/v1` sends it JSON-stringified; `/v1` sends
//! it as a JSON object — [`MetadataFilter::from_value`] accepts both. Includes an
//! in-memory evaluator (used for tests and non-SQL filtering); the DB layer
//! compiles the same AST to parameterized SQL.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FilterType {
    StringEqual,
    StringContains,
    Numeric,
    ArrayContains,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum NumericOp {
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "=")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FilterLeaf {
    pub key: String,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter_type: Option<FilterType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_operator: Option<NumericOp>,
    #[serde(default)]
    pub negate: bool,
    #[serde(default)]
    pub ignore_case: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum MetadataFilter {
    And {
        #[serde(rename = "AND")]
        and: Vec<MetadataFilter>,
    },
    Or {
        #[serde(rename = "OR")]
        or: Vec<MetadataFilter>,
    },
    Leaf(FilterLeaf),
}

impl MetadataFilter {
    /// Accept either a JSON object or a JSON string containing JSON.
    pub fn from_value(v: &Value) -> Option<MetadataFilter> {
        match v {
            Value::String(s) => serde_json::from_str(s).ok(),
            Value::Null => None,
            other => MetadataFilter::deserialize(other).ok(),
        }
    }

    /// Evaluate this filter against a flat metadata object.
    pub fn matches(&self, metadata: &Value) -> bool {
        match self {
            MetadataFilter::And { and } => and.iter().all(|f| f.matches(metadata)),
            MetadataFilter::Or { or } => or.iter().any(|f| f.matches(metadata)),
            MetadataFilter::Leaf(leaf) => leaf.matches(metadata),
        }
    }
}

impl FilterLeaf {
    fn matches(&self, metadata: &Value) -> bool {
        let field = metadata.get(&self.key);
        let raw = self.eval(field);
        if self.negate {
            !raw
        } else {
            raw
        }
    }

    fn eval(&self, field: Option<&Value>) -> bool {
        let ft = self.filter_type.clone().unwrap_or(FilterType::StringEqual);
        let field = match field {
            Some(f) => f,
            None => return false,
        };
        match ft {
            FilterType::StringEqual => {
                let a = value_to_string(field);
                let b = value_to_string(&self.value);
                if self.ignore_case {
                    a.eq_ignore_ascii_case(&b)
                } else {
                    a == b
                }
            }
            FilterType::StringContains => {
                let a = value_to_string(field);
                let b = value_to_string(&self.value);
                if self.ignore_case {
                    a.to_lowercase().contains(&b.to_lowercase())
                } else {
                    a.contains(&*b)
                }
            }
            FilterType::Numeric => {
                let a = field.as_f64();
                let b = self.value.as_f64();
                match (a, b) {
                    (Some(a), Some(b)) => {
                        let op = self.numeric_operator.unwrap_or(NumericOp::Eq);
                        match op {
                            NumericOp::Lt => a < b,
                            NumericOp::Le => a <= b,
                            NumericOp::Gt => a > b,
                            NumericOp::Ge => a >= b,
                            NumericOp::Eq => a == b,
                            NumericOp::Ne => a != b,
                        }
                    }
                    _ => false,
                }
            }
            FilterType::ArrayContains => {
                if let Value::Array(arr) = field {
                    let needle = value_to_string(&self.value);
                    arr.iter().any(|el| value_to_string(el) == needle)
                } else {
                    false
                }
            }
        }
    }
}

fn value_to_string(v: &Value) -> std::borrow::Cow<'_, str> {
    match v {
        Value::String(s) => std::borrow::Cow::Borrowed(s),
        Value::Null => std::borrow::Cow::Borrowed(""),
        other => std::borrow::Cow::Owned(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_nested_group() {
        let raw = json!({
            "AND": [
                {"key": "topic", "value": "rust", "filterType": "string_equal"},
                {"OR": [
                    {"key": "score", "value": 5, "filterType": "numeric", "numericOperator": ">="},
                    {"key": "tags", "value": "fav", "filterType": "array_contains"}
                ]}
            ]
        });
        let f = MetadataFilter::from_value(&raw).expect("parse");
        let md = json!({"topic": "rust", "score": 7, "tags": ["a", "b"]});
        assert!(f.matches(&md));
    }

    #[test]
    fn numeric_flips_under_negate() {
        let leaf = json!({"key": "n", "value": 10, "filterType": "numeric", "numericOperator": ">", "negate": true});
        let f = MetadataFilter::from_value(&leaf).unwrap();
        // n=20 -> 20 > 10 is true, negated -> false
        assert!(!f.matches(&json!({"n": 20})));
        // n=5 -> 5 > 10 is false, negated -> true
        assert!(f.matches(&json!({"n": 5})));
    }

    #[test]
    fn accepts_json_stringified_form() {
        let s = Value::String(r#"{"key":"a","value":"b","filterType":"string_equal"}"#.into());
        let f = MetadataFilter::from_value(&s).unwrap();
        assert!(f.matches(&json!({"a": "b"})));
        assert!(!f.matches(&json!({"a": "c"})));
    }
}
