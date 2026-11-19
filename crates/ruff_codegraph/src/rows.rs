//! The structural lane: a [`ModelGraph`] as graded relation rows.
//!
//! Rows are a set. They are sorted and de-duplicated, so they say what the
//! source declares and nothing about the order it declared it in; that order
//! is the event lane's job (`crate::events`).

use std::collections::BTreeMap;

use ruff_spo_triplet::{
    AssocKind, AttrKind, ConcernKind, GemKind, Model, ModelGraph, ScopeKind, ValidationKind,
};

use crate::cell;
use crate::error::Error;

/// A fact a declaration states outright.
pub const GRADE_STRUCTURE: &str = "SOURCE_STRUCTURE";
/// A fact derived from names alone, so possible and never confirmed.
pub const GRADE_POSSIBLE: &str = "SOURCE_STRUCTURE+POSSIBLE";

/// Which declaration family a row accounts for. Kept in memory so the
/// conservation check can count rows without parsing subjects; never written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Other,
    Function,
    Field,
    Association,
    Callback,
    Validation,
    Call,
    Import,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub predicate: &'static str,
    pub subject: String,
    pub object: String,
    pub grade: &'static str,
    pub family: Family,
}

/// Why the harvest could not see something. The writer only ever produces the
/// two reasons the Ruby frontend can state; the reader's set is larger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    CalleeNotNamed,
    ReferenceNotNamed,
}

impl Reason {
    pub const ALL: [Self; 2] = [Self::CalleeNotNamed, Self::ReferenceNotNamed];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CalleeNotNamed => "CALLEE_NOT_NAMED",
            Self::ReferenceNotNamed => "REFERENCE_NOT_NAMED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Residual {
    pub reason: Reason,
    pub subject: String,
}

/// The object cell of a row: a name is refused when unusable, free text is
/// escaped.
#[derive(Debug, Clone, Copy)]
pub enum Obj<'a> {
    Name(&'a str),
    Text(&'a str),
}

/// Sort key: `CRATE` first, then subject, predicate, object, grade.
type Key = (bool, String, &'static str, String, &'static str);

/// A row set under construction.
#[derive(Debug, Default)]
pub struct Builder {
    rows: BTreeMap<Key, Row>,
    residuals: Vec<Residual>,
}

/// The finished, sorted rows and residuals.
#[derive(Debug, Clone, Default)]
pub struct Built {
    pub rows: Vec<Row>,
    pub residuals: Vec<Residual>,
}

impl Builder {
    /// A builder holding the `CRATE` row of `namespace`. The harvest skips
    /// files it cannot parse without saying so, so the crate is never
    /// `COMPLETE`.
    pub fn new(namespace: &str) -> Result<Self, Error> {
        let mut builder = Self::default();
        builder.put(
            "CRATE",
            namespace,
            Obj::Name("PARTIAL"),
            GRADE_STRUCTURE,
            Family::Other,
        )?;
        Ok(builder)
    }

    /// Add a row; `Ok(false)` when an identical row was already there.
    pub fn put(
        &mut self,
        predicate: &'static str,
        subject: &str,
        object: Obj<'_>,
        grade: &'static str,
        family: Family,
    ) -> Result<bool, Error> {
        let subject = cell::name("subject", subject)?.to_owned();
        let object = match object {
            Obj::Name(n) => cell::name("object", n)?.to_owned(),
            Obj::Text(t) => cell::text(t),
        };
        let key = (
            predicate != "CRATE",
            subject.clone(),
            predicate,
            object.clone(),
            grade,
        );
        if self.rows.contains_key(&key) {
            return Ok(false);
        }
        self.rows.insert(
            key,
            Row {
                predicate,
                subject,
                object,
                grade,
                family,
            },
        );
        Ok(true)
    }

    pub fn residual(&mut self, reason: Reason, subject: &str) {
        self.residuals.push(Residual {
            reason,
            subject: subject.to_owned(),
        });
    }

    #[must_use]
    pub fn finish(self) -> Built {
        let mut residuals = self.residuals;
        residuals.sort();
        Built {
            rows: self.rows.into_values().collect(),
            residuals,
        }
    }
}

/// `base` + `sep` + `part`, refusing an unusable `part`.
fn member(base: &str, sep: &str, part: &str) -> Result<String, Error> {
    Ok(format!("{base}{sep}{}", cell::name("name", part)?))
}

/// A constant as the store spells it: `<ns>::<Const>`, without the leading
/// `::` a top-level reference carries.
fn qualified(ns: &str, constant: &str) -> Result<String, Error> {
    member(ns, "::", constant.trim_start_matches("::"))
}

/// Project every model of `graph` into `builder`.
pub fn structural(graph: &ModelGraph, builder: &mut Builder) -> Result<(), Error> {
    for model in &graph.models {
        project_model(&graph.namespace, model, builder)?;
    }
    Ok(())
}

fn project_model(ns: &str, model: &Model, b: &mut Builder) -> Result<(), Error> {
    let m = member(ns, "::", &model.name)?;
    b.put("DEFINES", &m, Obj::Name(&m), GRADE_STRUCTURE, Family::Other)?;

    let parents = model
        .sti
        .iter()
        .filter_map(|sti| sti.inherits_from.as_deref())
        .chain(model.inherits.iter().map(String::as_str));
    for parent in parents {
        b.put(
            "INHERITS",
            &m,
            Obj::Name(&qualified(ns, parent)?),
            GRADE_STRUCTURE,
            Family::Other,
        )?;
    }

    for concern in &model.concerns {
        let (predicate, object) = match concern.kind {
            ConcernKind::Include => ("INCLUDES", qualified(ns, &concern.module)?),
            ConcernKind::Extend => ("EXTENDS", qualified(ns, &concern.module)?),
            ConcernKind::Prepend => ("PREPENDS", qualified(ns, &concern.module)?),
            ConcernKind::ClassMethodsBlock => ("CONCERN_BLOCK", "class_methods".to_owned()),
            ConcernKind::IncludedBlock => ("CONCERN_BLOCK", "included".to_owned()),
        };
        b.put(
            predicate,
            &m,
            Obj::Name(&object),
            GRADE_STRUCTURE,
            Family::Other,
        )?;
    }

    for assoc in &model.associations {
        let node = member(&m, ".", &assoc.name)?;
        b.put(
            "ASSOCIATION",
            &m,
            Obj::Name(&node),
            GRADE_STRUCTURE,
            Family::Association,
        )?;
        b.put(
            "ASSOCIATION_KIND",
            &node,
            Obj::Name(assoc_macro(assoc.kind)),
            GRADE_STRUCTURE,
            Family::Other,
        )?;
        for (key, value) in &assoc.options {
            b.put(
                "ASSOCIATION_OPTION",
                &node,
                Obj::Text(&format!("{key}={value}")),
                GRADE_STRUCTURE,
                Family::Other,
            )?;
        }
    }

    for validation in &model.validations {
        let fact = format!(
            "{}:{}",
            validation_macro(validation.kind),
            validation.target
        );
        b.put(
            "VALIDATES",
            &m,
            Obj::Text(&fact),
            GRADE_STRUCTURE,
            Family::Validation,
        )?;
        for (key, value) in &validation.options {
            b.put(
                "VALIDATES_OPTION",
                &m,
                Obj::Text(&format!("{fact}:{key}={value}")),
                GRADE_STRUCTURE,
                Family::Other,
            )?;
        }
    }

    for scope in &model.scopes {
        let fact = format!("{}:{}", scope_macro(scope.kind), scope.name);
        b.put(
            "SCOPE",
            &m,
            Obj::Text(&fact),
            GRADE_STRUCTURE,
            Family::Other,
        )?;
    }

    for delegation in &model.delegations {
        for method in &delegation.methods {
            let fact = format!("{method}:{}", delegation.to);
            b.put(
                "DELEGATES",
                &m,
                Obj::Text(&fact),
                GRADE_STRUCTURE,
                Family::Other,
            )?;
        }
    }

    for dsl in &model.dsl_calls {
        let fact = format!("{}({})", dsl.name, dsl.args);
        b.put(
            "DSL_CALL",
            &m,
            Obj::Text(&fact),
            GRADE_STRUCTURE,
            Family::Other,
        )?;
    }

    for field in &model.fields {
        let node = member(&m, ".", &field.name)?;
        b.put(
            "DEFINES",
            &node,
            Obj::Name(&node),
            GRADE_STRUCTURE,
            Family::Field,
        )?;
        if let Some(kind) = &field.field_type {
            b.put(
                "FIELD_TYPE",
                &node,
                Obj::Text(kind),
                GRADE_STRUCTURE,
                Family::Other,
            )?;
        }
    }
    for attribute in model
        .attributes
        .iter()
        .filter(|a| a.kind == AttrKind::Attribute)
    {
        let node = member(&m, ".", &attribute.name)?;
        b.put(
            "DEFINES",
            &node,
            Obj::Name(&node),
            GRADE_STRUCTURE,
            Family::Field,
        )?;
        if let Some((_, kind)) = attribute.options.iter().find(|(key, _)| key == "type") {
            b.put(
                "FIELD_TYPE",
                &node,
                Obj::Text(kind),
                GRADE_STRUCTURE,
                Family::Other,
            )?;
        }
    }

    let mut function_defs: BTreeMap<&str, usize> = BTreeMap::new();
    for f in model.functions.iter().chain(&model.helpers) {
        *function_defs.entry(f.name.as_str()).or_default() += 1;
    }

    for callback in &model.callbacks {
        let phase = member(&m, "::", &format!("<{}>", callback.phase))?;
        b.put(
            "DEFINES",
            &phase,
            Obj::Name(&phase),
            GRADE_STRUCTURE,
            Family::Other,
        )?;
        b.put(
            "SYNTHETIC",
            &phase,
            Obj::Name(&phase),
            GRADE_STRUCTURE,
            Family::Other,
        )?;
        let fresh = b.put(
            "CALLBACK",
            &phase,
            Obj::Text(&callback.target),
            GRADE_STRUCTURE,
            Family::Callback,
        )?;
        for (key, value) in &callback.options {
            b.put(
                "CALLBACK_OPTION",
                &phase,
                Obj::Text(&format!("{}:{key}={value}", callback.target)),
                GRADE_STRUCTURE,
                Family::Other,
            )?;
        }
        if fresh && !function_defs.contains_key(callback.target.as_str()) {
            b.residual(Reason::ReferenceNotNamed, &phase);
        }
    }

    for (public, function) in model
        .functions
        .iter()
        .map(|f| (true, f))
        .chain(model.helpers.iter().map(|f| (false, f)))
    {
        let f = member(&m, "::", &function.name)?;
        b.put(
            "DEFINES",
            &f,
            Obj::Name(&f),
            GRADE_STRUCTURE,
            Family::Function,
        )?;
        if public {
            b.put("PUBLIC", &f, Obj::Name(&f), GRADE_STRUCTURE, Family::Other)?;
        }
        let possible = |b: &mut Builder, predicate: &'static str, object: Obj<'_>| {
            b.put(predicate, &f, object, GRADE_POSSIBLE, Family::Other)
        };
        for (predicate, names) in [
            ("READS", &function.reads),
            ("WRITES", &function.writes),
            ("GUARDED_WRITE", &function.guarded_writes),
            ("TRAVERSES", &function.traverses),
        ] {
            for name in names {
                possible(b, predicate, Obj::Name(&member(&m, ".", name)?))?;
            }
        }
        for exception in &function.raises {
            possible(b, "RAISES", Obj::Text(exception))?;
        }
        for call in &function.calls {
            let sibling = call
                .rsplit_once('.')
                .filter(|(receiver, _)| *receiver == "self")
                .map(|(_, method)| method)
                .filter(|method| function_defs.get(method) == Some(&1));
            if let Some(method) = sibling {
                b.put(
                    "CALLS_NAMED",
                    &f,
                    Obj::Name(&member(&m, "::", method)?),
                    GRADE_POSSIBLE,
                    Family::Call,
                )?;
            } else if b.put(
                "UNRESOLVED_CALL",
                &f,
                Obj::Text(call),
                GRADE_POSSIBLE,
                Family::Call,
            )? {
                b.residual(Reason::CalleeNotNamed, &f);
            }
        }
    }
    Ok(())
}

pub(crate) fn assoc_macro(kind: AssocKind) -> &'static str {
    match kind {
        AssocKind::BelongsTo => "belongs_to",
        AssocKind::HasMany => "has_many",
        AssocKind::HasOne => "has_one",
        AssocKind::HasAndBelongsToMany => "has_and_belongs_to_many",
        AssocKind::AcceptsNestedAttributesFor => "accepts_nested_attributes_for",
    }
}

pub(crate) fn validation_macro(kind: ValidationKind) -> &'static str {
    match kind {
        ValidationKind::Validates => "validates",
        ValidationKind::Validate => "validate",
        ValidationKind::Normalizes => "normalizes",
        ValidationKind::ValidatesAssociated => "validates_associated",
        ValidationKind::ValidatesEach => "validates_each",
        ValidationKind::ValidatesWith => "validates_with",
    }
}

pub(crate) fn scope_macro(kind: ScopeKind) -> &'static str {
    match kind {
        ScopeKind::Scope => "scope",
        ScopeKind::DefaultScope => "default_scope",
        ScopeKind::Scopes => "scopes",
    }
}

pub(crate) fn concern_macro(kind: ConcernKind) -> &'static str {
    match kind {
        ConcernKind::Include => "include",
        ConcernKind::Extend => "extend",
        ConcernKind::Prepend => "prepend",
        ConcernKind::ClassMethodsBlock => "class_methods",
        ConcernKind::IncludedBlock => "included",
    }
}

pub(crate) fn attr_macro(kind: AttrKind) -> &'static str {
    match kind {
        AttrKind::Attribute => "attribute",
        AttrKind::AttrAccessor => "attr_accessor",
        AttrKind::AttrReader => "attr_reader",
        AttrKind::AttrReadonly => "attr_readonly",
        AttrKind::AliasAttribute => "alias_attribute",
        AttrKind::AliasMethod => "alias_method",
        AttrKind::Alias => "alias",
        AttrKind::UndefMethod => "undef_method",
        AttrKind::Serialize => "serialize",
        AttrKind::Enum => "enum",
        AttrKind::StoreAttribute => "store_attribute",
        AttrKind::StoreAccessor => "store_accessor",
        AttrKind::DefineAttributeMethod => "define_attribute_method",
    }
}

pub(crate) fn gem_macro(kind: GemKind) -> &'static str {
    match kind {
        GemKind::MountUploader => "mount_uploader",
        GemKind::HasPaperTrail => "has_paper_trail",
        GemKind::HasClosureTree => "has_closure_tree",
        GemKind::CounterCulture => "counter_culture",
        GemKind::AutoStripAttributes => "auto_strip_attributes",
    }
}
