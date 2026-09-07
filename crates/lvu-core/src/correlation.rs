use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_EXACT_FIELD_BYTES: usize = 64;
pub const MAX_EXACT_SCALAR_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExactFieldError {
    #[error("field name is empty")]
    EmptyField,
    #[error("field name exceeds {MAX_EXACT_FIELD_BYTES} UTF-8 bytes")]
    FieldTooLarge,
    #[error("field name {0:?} is reserved")]
    ReservedField(String),
    #[error("scalar exceeds {MAX_EXACT_SCALAR_BYTES} UTF-8 bytes")]
    ScalarTooLarge,
    #[error("non-finite floats cannot be correlated")]
    NonFiniteFloat,
    #[error("no source was mapped to a field carrying this value")]
    NoMappedSource,
    #[error("a correlation spans at most {MAX_CORRELATION_SOURCES} sources")]
    TooManySources,
    #[error("source identity {0:?} is invalid")]
    InvalidSource(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExactScalar {
    Null,
    Bool(bool),
    SignedInteger(i64),
    UnsignedInteger(u64),
    /// Exact IEEE-754 binary64 representation. This keeps float provenance and
    /// avoids decimal rendering or integer promotion changing equality.
    FloatBits(u64),
    String(String),
}

#[derive(Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum ExactScalarWire {
    Null,
    Bool(bool),
    SignedInteger(i64),
    UnsignedInteger(u64),
    FloatBits(u64),
    String(String),
}

impl<'de> Deserialize<'de> for ExactScalar {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = match ExactScalarWire::deserialize(deserializer)? {
            ExactScalarWire::Null => Self::Null,
            ExactScalarWire::Bool(value) => Self::Bool(value),
            ExactScalarWire::SignedInteger(value) => Self::SignedInteger(value),
            ExactScalarWire::UnsignedInteger(value) => Self::UnsignedInteger(value),
            ExactScalarWire::FloatBits(value) => Self::FloatBits(value),
            ExactScalarWire::String(value) => Self::String(value),
        };
        value.validate().map_err(serde::de::Error::custom)?;
        Ok(value)
    }
}

impl ExactScalar {
    pub fn finite_float(value: f64) -> Result<Self, ExactFieldError> {
        if value.is_finite() {
            Ok(Self::FloatBits(value.to_bits()))
        } else {
            Err(ExactFieldError::NonFiniteFloat)
        }
    }

    pub fn string(value: impl Into<String>) -> Result<Self, ExactFieldError> {
        let value = value.into();
        if value.len() > MAX_EXACT_SCALAR_BYTES {
            Err(ExactFieldError::ScalarTooLarge)
        } else {
            Ok(Self::String(value))
        }
    }

    pub fn validate(&self) -> Result<(), ExactFieldError> {
        match self {
            Self::String(value) if value.len() > MAX_EXACT_SCALAR_BYTES => {
                Err(ExactFieldError::ScalarTooLarge)
            }
            Self::FloatBits(bits) if !f64::from_bits(*bits).is_finite() => {
                Err(ExactFieldError::NonFiniteFloat)
            }
            _ => Ok(()),
        }
    }

    pub fn exact_token(&self) -> Result<String, ExactFieldError> {
        self.validate()?;
        Ok(match self {
            Self::Null => "n".into(),
            Self::Bool(value) => format!("b:{value}"),
            Self::SignedInteger(value) => format!("i:{value}"),
            Self::UnsignedInteger(value) => format!("u:{value}"),
            Self::FloatBits(value) => format!("f:{value:016x}"),
            Self::String(value) => format!("s:{value}"),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExactFieldConstraint {
    field: String,
    value: ExactScalar,
}

impl<'de> Deserialize<'de> for ExactFieldConstraint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            field: String,
            value: ExactScalar,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.field, wire.value).map_err(serde::de::Error::custom)
    }
}

impl ExactFieldConstraint {
    pub fn new(field: impl Into<String>, value: ExactScalar) -> Result<Self, ExactFieldError> {
        let candidate = Self {
            field: field.into(),
            value,
        };
        candidate.validate()?;
        Ok(candidate)
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub fn value(&self) -> &ExactScalar {
        &self.value
    }

    pub fn validate(&self) -> Result<(), ExactFieldError> {
        if self.field.is_empty() {
            return Err(ExactFieldError::EmptyField);
        }
        if self.field.len() > MAX_EXACT_FIELD_BYTES {
            return Err(ExactFieldError::FieldTooLarge);
        }
        if self.field == "raw" || self.field.starts_with("_lvu_") {
            return Err(ExactFieldError::ReservedField(self.field.clone()));
        }
        self.value.validate()
    }
}

/// Sources a single correlation may span. Matches the view source cap so an
/// accepted correlation can always be installed as one merged view.
pub const MAX_CORRELATION_SOURCES: usize = 32;

/// One accepted cross-source correlation: a single typed value, and the field
/// that carries it *in each source*. Sources use different key names for the
/// same identity (`request_id` here, `req` there), so the mapping is explicit
/// and per source. A source absent from `sources` contributes no records; a
/// name is never inferred for it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FieldCorrelation {
    /// The field the user correlated on, in the source the record came from.
    /// Used for labels and for pinning; it is not a fallback for unmapped
    /// sources.
    origin_field: String,
    value: ExactScalar,
    /// Source id → the field name that carries this identity in that source.
    sources: std::collections::BTreeMap<String, String>,
}

impl<'de> Deserialize<'de> for FieldCorrelation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            origin_field: String,
            value: ExactScalar,
            sources: std::collections::BTreeMap<String, String>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.origin_field, wire.value, wire.sources).map_err(serde::de::Error::custom)
    }
}

impl FieldCorrelation {
    pub fn new(
        origin_field: impl Into<String>,
        value: ExactScalar,
        sources: std::collections::BTreeMap<String, String>,
    ) -> Result<Self, ExactFieldError> {
        let candidate = Self {
            origin_field: origin_field.into(),
            value,
            sources,
        };
        candidate.validate()?;
        Ok(candidate)
    }

    pub fn origin_field(&self) -> &str {
        &self.origin_field
    }

    pub fn value(&self) -> &ExactScalar {
        &self.value
    }

    /// Source ids this correlation covers, in a stable order.
    pub fn source_ids(&self) -> impl Iterator<Item = &str> {
        self.sources.keys().map(String::as_str)
    }

    /// Distinct mapped field names, in a stable order. These are the columns
    /// worth pinning so the correlating value stays visible in every source.
    pub fn fields(&self) -> Vec<String> {
        let mut names: Vec<String> = self.sources.values().cloned().collect();
        names.sort();
        names.dedup();
        names
    }

    /// The exact predicate for one source, or `None` when the user did not map
    /// that source. `None` means "no records", never "try the origin name".
    pub fn constraint_for(&self, source_id: &str) -> Option<ExactFieldConstraint> {
        let field = self.sources.get(source_id)?;
        ExactFieldConstraint::new(field.clone(), self.value.clone()).ok()
    }

    pub fn validate(&self) -> Result<(), ExactFieldError> {
        ExactFieldConstraint::new(self.origin_field.clone(), self.value.clone())?;
        if self.sources.is_empty() {
            return Err(ExactFieldError::NoMappedSource);
        }
        if self.sources.len() > MAX_CORRELATION_SOURCES {
            return Err(ExactFieldError::TooManySources);
        }
        for (source_id, field) in &self.sources {
            if source_id.is_empty() || source_id.len() > 128 {
                return Err(ExactFieldError::InvalidSource(source_id.clone()));
            }
            ExactFieldConstraint::new(field.clone(), self.value.clone())?;
        }
        Ok(())
    }
}
