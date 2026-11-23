//! The variables a stage hands its sub-pipeline, and a command hands its
//! stages: `$$NOW`, a `let`'s names, `$lookup`'s `let`, `$merge`'s.
//!
//! mongod keeps each as a `Value`, and a `Value` can be EOO -- MISSING -- as
//! well as any BSON one (`ExpressionContext` / `VariablesParseState`). A
//! `$lookup` `let` bound from an absent field reads "missing" in `$type`, not
//! null. A [`bson::Document`] of values has no such case, so the variables
//! travel as a [`Vars`]: a [`Field`] per name, which is `Missing` or a `Value`
//! (RM-763). MISSING is a case of the type, never a reserved key in a
//! document.

use bson::{Bson, Document};

pub use crate::expressions::Field;

impl From<Bson> for Field {
    fn from(v: Bson) -> Field {
        Field::Value(v)
    }
}

macro_rules! field_from {
    ($($t:ty),* $(,)?) => {
        $(impl From<$t> for Field {
            fn from(v: $t) -> Field {
                Field::Value(Bson::from(v))
            }
        })*
    };
}
field_from!(
    Document,
    bool,
    i32,
    i64,
    f64,
    String,
    &str,
    bson::DateTime,
    bson::Timestamp
);

/// Variable names and what each is bound to, in binding order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Vars {
    bound: Vec<(String, Field)>,
}

impl Vars {
    pub fn new() -> Vars {
        Vars::default()
    }

    /// What `name` is bound to: `Some(Field::Missing)` for a variable bound
    /// to MISSING, `None` for one that is not bound at all.
    pub fn get(&self, name: &str) -> Option<&Field> {
        self.bound.iter().find(|(n, _)| n == name).map(|(_, f)| f)
    }

    /// The value `name` is bound to; `None` when it is unbound or MISSING.
    pub fn value(&self, name: &str) -> Option<&Bson> {
        match self.get(name) {
            Some(Field::Value(v)) => Some(v),
            _ => None,
        }
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Bind `name`, replacing an earlier binding in place.
    pub fn insert(&mut self, name: impl Into<String>, to: impl Into<Field>) {
        let (name, to) = (name.into(), to.into());
        match self.bound.iter_mut().find(|(n, _)| *n == name) {
            Some((_, f)) => *f = to,
            None => self.bound.push((name, to)),
        }
    }

    pub fn remove(&mut self, name: &str) -> Option<Field> {
        let at = self.bound.iter().position(|(n, _)| n == name)?;
        Some(self.bound.remove(at).1)
    }

    pub fn len(&self) -> usize {
        self.bound.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bound.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.bound.iter().map(|(n, _)| n)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Field)> {
        self.bound.iter().map(|(n, f)| (n, f))
    }

    /// The variables that hold a value, as a document: a MISSING one has no
    /// BSON form, so it is left out. For the places that hand variables to a
    /// layer that reads a document (an update's `c`, a command's `let`), where
    /// an unbound name and a MISSING one answer alike.
    pub fn values(&self) -> Document {
        let mut d = Document::new();
        for (n, f) in &self.bound {
            if let Field::Value(v) = f {
                d.insert(n.clone(), v.clone());
            }
        }
        d
    }
}

impl From<Document> for Vars {
    fn from(d: Document) -> Vars {
        Vars {
            bound: d.into_iter().map(|(n, v)| (n, Field::Value(v))).collect(),
        }
    }
}

impl From<&Document> for Vars {
    fn from(d: &Document) -> Vars {
        Vars::from(d.clone())
    }
}
