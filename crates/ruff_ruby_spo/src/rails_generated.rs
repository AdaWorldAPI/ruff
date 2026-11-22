//! Callbacks that `ActiveRecord` generates for association macros.
//!
//! A class body such as
//!
//! ```ruby
//! has_many :journals
//! belongs_to :project
//! ```
//!
//! registers callbacks the application never wrote:
//! `autosave_associated_records_for_journals`, `validate_associated_records_for_journals`,
//! a presence validator for `project`, and so on. They are generic Rails
//! semantics, so they are derived here by ONE rule set read from Rails, never
//! from a per-application table.
//!
//! The input is the ordered declaration stream
//! [`crate::events::ClassOre::declarations`]; the output lists the generated
//! callbacks in the order Rails registers them. Source of truth is Rails
//! 8.1.4.1 as `OpenProject` vendors it (`activerecord-8.1.4.1/lib/active_record`);
//! every [`GeneratedCallback::rule`] names the file and lines of the rule it
//! applies.
//!
//! # Rules implemented
//!
//! Per association macro, in registration order (`builder/association.rb:32-36`
//! runs callbacks, then validations):
//!
//! | macro | generated | Rails |
//! |---|---|---|
//! | `has_many`, `has_and_belongs_to_many` | `around_save :around_save_collection_association` | `autosave_association.rb:193` |
//! | | `after_create :autosave_associated_records_for_<name>` | `:197` |
//! | | `after_update :autosave_associated_records_for_<name>` | `:198` |
//! | `has_one` | `after_create` / `after_update` the same save method | `:209-210` |
//! | `belongs_to` | `before_save :autosave_associated_records_for_<name>` | `:213` |
//! | any of the three | `validate :validate_associated_records_for_<name>` and `after_validation :_ensure_no_duplicate_errors`, only when `validate?` holds and the validation method is not yet defined | `:219-233` |
//! | `belongs_to` | presence validator on `<name>` (`message: :required`) when required | `builder/belongs_to.rb:131-166` |
//! | `has_one` with `required:` | presence validator on `<name>` | `builder/has_one.rb:30-35` |
//! | `accepts_nested_attributes_for` | sets `autosave = true` on the named association and runs the validation step again | `nested_attributes.rb:361-362` |
//!
//! `validate?` is `reflection.rb:717-719`: an explicit non-nil `validate:`
//! wins; otherwise `autosave: true` or a collection association validates.
//! So `autosave: false` does NOT suppress the validation callback of a
//! `has_many`, and no `autosave:` value suppresses the save callbacks: those
//! are registered unconditionally (`autosave_association.rb:189-217`).
//!
//! `belongs_to` is required when `required:` is truthy, when `optional:` is
//! false, or — with neither given — when the application's
//! `belongs_to_required_by_default` is on. That setting is application
//! configuration, so it comes from [`RailsDefaults`]; unset, the outcome is
//! [`Generated::Undecided`] rather than a guess. The same holds for
//! `belongs_to_required_validates_foreign_key`, which decides whether the
//! presence validator carries an `if:` condition (`belongs_to.rb:144-165`).
//!
//! `has_and_belongs_to_many` also registers the collection callbacks of an
//! internal middle `has_many` first (`associations.rb:2020`), whose name is
//! `[<class>.downcase.pluralize, <assoc>].sort.join("_")`
//! (`builder/has_and_belongs_to_many.rb:59-61`). That needs `ActiveSupport`'s
//! inflector, so the middle association's callbacks are [`Generated::Undecided`]
//! and the visible `has_many` follows with `autosave:` and `validate:`
//! forwarded as Rails does (`associations.rb:2038-2042`).
//!
//! # Undecided, never guessed
//!
//! An option that is not the literal `true`, `false` or `nil` (the harvest
//! renders every other value as `<expr>`, a symbol as `:x`, …) and that a rule
//! needs yields [`Generated::Undecided`] naming the rule and the reason.
//!
//! # Keyword splats
//!
//! `has_many :x, **opts` may carry any option, so a splat is never read as "no
//! options". The harvest records it in
//! [`ruff_spo_triplet::AssocDecl::option_splats`], with the number of explicit
//! options before it. An option is known only when an explicit key for it
//! appears AFTER the last splat (a later key wins in a Ruby hash literal); an
//! explicit key before a splat, or none, is unknown. Per rule:
//!
//! | rule | reads | with a splat |
//! |---|---|---|
//! | save callbacks (`autosave_association.rb:193-213`) | nothing | derived |
//! | `validate?` step (`reflection.rb:717-719`, `autosave_association.rb:219-233`) | `validate`, and `autosave` for a singular association | `Undecided` unless decided after the splat |
//! | `belongs_to` presence (`builder/belongs_to.rb:131-166`) | `required`, `optional` | `Undecided` unless decided after the splat |
//! | `has_one` presence (`builder/has_one.rb:32-34`) | `required` | `Undecided` unless decided after the splat |
//! | `accepts_nested_attributes_for` (`nested_attributes.rb:361-362`) | `validate` of the named association | `Undecided` unless decided after the splat |
//! | `has_and_belongs_to_many` middle association (`associations.rb:2020`) | nothing | `Undecided` as before |
//!
//! # Not covered
//!
//! - `dependent:` (`builder/association.rb:144-154`), `touch:`
//!   (`belongs_to.rb:89-110`, `has_one.rb:46-56`), `counter_cache:`
//!   (`belongs_to.rb:28-43`) and `default:` (`belongs_to.rb:113-117`) register anonymous lambdas, so
//!   there is no filter name to report, and their callback sets depend on
//!   further options (`through:`, `counter_cache_column`).
//! - The `before_add`/`after_add`/`before_remove`/`after_remove` hooks of
//!   collections (`builder/collection_association.rb:12-18`, `:30-54`) are class attributes,
//!   not chain callbacks.
//! - A name declared twice. Rails de-duplicates a repeated symbol filter in a
//!   chain (the later registration replaces the earlier one), which moves it
//!   in the order; only the validation step's own
//!   `!method_defined?(validation_method)` guard is modelled.
//! - Associations inherited from a superclass or a concern: an
//!   `accepts_nested_attributes_for` naming one is [`Generated::Undecided`].
//! - The associations Rails builds on the join model of a
//!   `has_and_belongs_to_many` (`builder/has_and_belongs_to_many.rb:34-43`), which
//!   live on another class.

use std::collections::{HashMap, HashSet};

use ruff_spo_triplet::{AssocDecl, AssocKind, OptionSplat};

use crate::Declaration;
use crate::events::RailsEvent;

const AUTOSAVE: &str = "activerecord-8.1.4.1/lib/active_record/autosave_association.rb";
const BELONGS_TO: &str =
    "activerecord-8.1.4.1/lib/active_record/associations/builder/belongs_to.rb";
const HAS_ONE: &str = "activerecord-8.1.4.1/lib/active_record/associations/builder/has_one.rb";
const NESTED: &str = "activerecord-8.1.4.1/lib/active_record/nested_attributes.rb";
const REFLECTION: &str = "activerecord-8.1.4.1/lib/active_record/reflection.rb";
const ASSOCIATIONS: &str = "activerecord-8.1.4.1/lib/active_record/associations.rb";

/// The callback chain a generated callback is registered on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    Save,
    Create,
    Update,
    /// The `validate` chain (`validate :method`, `validates_presence_of`).
    Validate,
    /// The `validation` chain (`before_validation` / `after_validation`).
    Validation,
}

/// Whether the callback runs before, after or around the chain's action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Before,
    After,
    Around,
}

/// What the callback dispatches to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// A method, named exactly as Rails names it.
    Method(String),
    /// `validates_presence_of <attribute>`: a validator object, not a method.
    PresenceValidator { attribute: String },
}

/// Where a generated callback comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// [`RailsEvent::seq`] of the declaration that caused it.
    pub declaration_seq: u32,
    /// [`RailsEvent::anchor`]: byte offset of that macro call.
    pub declaration_anchor: u32,
    /// Rails source `file:line` of the rule, relative to the gem's `lib/`
    /// parent directory.
    pub rule: String,
}

/// One callback Rails generates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedCallback {
    /// The association the callback belongs to.
    pub association: String,
    pub chain: Chain,
    pub kind: Kind,
    pub filter: Filter,
    /// Options that condition it, as Rails passes them (`message: :required`,
    /// an `if:` lambda described in words).
    pub options: Vec<(String, String)>,
    pub origin: Origin,
}

/// A rule that applies but cannot be decided from source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undecided {
    pub association: String,
    pub declaration_seq: u32,
    pub declaration_anchor: u32,
    /// Rails source `file:line` of the rule that could not be decided.
    pub rule: String,
    pub reason: String,
}

/// One entry of the derived stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Generated {
    Callback(GeneratedCallback),
    Undecided(Undecided),
}

/// Application configuration the rules read. `None` is "not known", and a rule
/// that needs it yields [`Generated::Undecided`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RailsDefaults {
    /// `ActiveRecord::Base.belongs_to_required_by_default` (`core.rb:88`).
    pub belongs_to_required_by_default: Option<bool>,
    /// `ActiveRecord.belongs_to_required_validates_foreign_key`
    /// (`builder/belongs_to.rb:145`).
    pub belongs_to_required_validates_foreign_key: Option<bool>,
}

/// The line (1-based) of a byte offset in `src`, for resolving
/// [`Origin::declaration_anchor`].
#[must_use]
pub fn line_of(src: &str, anchor: u32) -> u32 {
    let end = usize::try_from(anchor).unwrap_or(usize::MAX).min(src.len());
    let lines = src.as_bytes()[..end].split(|b| *b == b'\n').count();
    u32::try_from(lines).unwrap_or(u32::MAX)
}

/// Derive the generated callbacks of a class body, in registration order.
#[must_use]
pub fn derive(events: &[RailsEvent], defaults: RailsDefaults) -> Vec<Generated> {
    let mut walk = Derive {
        defaults,
        out: Vec::new(),
        validated: HashSet::new(),
        seen: HashMap::new(),
    };
    for event in events {
        if let Declaration::Association(decl) = &event.decl {
            walk.declaration(event, decl);
        }
    }
    walk.out
}

/// The three-valued reading of an option's source text.
enum Lit {
    True,
    False,
    Nil,
    Other(String),
}

/// The value of `key` on `decl`, last occurrence winning as in a Ruby hash.
///
/// `Ok(None)` is "absent". With a `**splat` in the hash, an occurrence only
/// counts when it comes after the last splat; otherwise the splat may supply
/// or override the key and the result is `Err` naming the splat.
fn option(decl: &AssocDecl, key: &str) -> Result<Option<Lit>, String> {
    let last = decl
        .options
        .iter()
        .enumerate()
        .rev()
        .find(|(_, (k, _))| k == key);
    let after_splats = decl.option_splats.iter().map(|s| s.before).max();
    match (last, after_splats) {
        (Some((i, (_, v))), tail) if tail.is_none_or(|t| i >= t) => Ok(Some(match v.as_str() {
            "true" => Lit::True,
            "false" => Lit::False,
            "nil" => Lit::Nil,
            other => Lit::Other(other.to_string()),
        })),
        (None, None) => Ok(None),
        _ => {
            let splats: Vec<&str> = decl
                .option_splats
                .iter()
                .map(|s| s.source.as_str())
                .collect();
            Err(format!(
                "`{key}:` may be supplied by the keyword splat `{}`, which cannot be read \
                 (an explicit `{key}:` after the splat would decide it)",
                splats.join("`, `")
            ))
        }
    }
}

fn not_literal(key: &str, value: &str) -> String {
    format!("`{key}:` is `{value}`, not a literal true/false/nil")
}

struct Derive {
    defaults: RailsDefaults,
    out: Vec<Generated>,
    /// Names whose `validate_associated_records_for_<name>` is defined
    /// (`autosave_association.rb:221`).
    validated: HashSet<String>,
    /// The latest declaration per association name, for
    /// `accepts_nested_attributes_for`.
    seen: HashMap<String, AssocDecl>,
}

impl Derive {
    fn declaration(&mut self, event: &RailsEvent, decl: &AssocDecl) {
        match decl.kind {
            AssocKind::BelongsTo | AssocKind::HasOne | AssocKind::HasMany => {
                self.seen.insert(decl.name.clone(), decl.clone());
                self.association(event, decl, decl.kind);
            }
            AssocKind::HasAndBelongsToMany => {
                self.undecided(
                    event,
                    &decl.name,
                    format!("{ASSOCIATIONS}:2020"),
                    "the internal middle has_many registers the collection callbacks first; \
                     its name needs ActiveSupport pluralize of the class name"
                        .to_string(),
                );
                // Only `autosave` and `validate` reach the visible has_many
                // that matters here (`associations.rb:2038-2042`).
                let forwarded: Vec<(String, String)> = decl
                    .options
                    .iter()
                    .filter(|(k, _)| k == "autosave" || k == "validate")
                    .cloned()
                    .collect();
                // A splat is kept: it may carry either forwarded key. Its
                // position moves with the options that were filtered out.
                let option_splats = decl
                    .option_splats
                    .iter()
                    .map(|splat| OptionSplat {
                        source: splat.source.clone(),
                        before: decl.options[..splat.before]
                            .iter()
                            .filter(|(k, _)| k == "autosave" || k == "validate")
                            .count(),
                    })
                    .collect();
                let forwarded = AssocDecl {
                    kind: AssocKind::HasMany,
                    name: decl.name.clone(),
                    options: forwarded,
                    option_splats,
                };
                self.seen.insert(decl.name.clone(), forwarded.clone());
                self.association(event, &forwarded, AssocKind::HasMany);
            }
            AssocKind::AcceptsNestedAttributesFor => self.nested(event, decl),
        }
    }

    fn association(&mut self, event: &RailsEvent, decl: &AssocDecl, kind: AssocKind) {
        let name = &decl.name;
        let method = format!("autosave_associated_records_for_{name}");
        match kind {
            AssocKind::HasMany => {
                self.callback(
                    event,
                    name,
                    Chain::Save,
                    Kind::Around,
                    "around_save_collection_association",
                    format!("{AUTOSAVE}:193"),
                );
                self.callback(
                    event,
                    name,
                    Chain::Create,
                    Kind::After,
                    &method,
                    format!("{AUTOSAVE}:197"),
                );
                self.callback(
                    event,
                    name,
                    Chain::Update,
                    Kind::After,
                    &method,
                    format!("{AUTOSAVE}:198"),
                );
            }
            AssocKind::HasOne => {
                self.callback(
                    event,
                    name,
                    Chain::Create,
                    Kind::After,
                    &method,
                    format!("{AUTOSAVE}:209"),
                );
                self.callback(
                    event,
                    name,
                    Chain::Update,
                    Kind::After,
                    &method,
                    format!("{AUTOSAVE}:210"),
                );
            }
            _ => {
                self.callback(
                    event,
                    name,
                    Chain::Save,
                    Kind::Before,
                    &method,
                    format!("{AUTOSAVE}:213"),
                );
            }
        }

        match validate_predicate(kind == AssocKind::HasMany, decl) {
            Ok(true) => self.validation_step(event, name),
            Ok(false) => {}
            Err(reason) => self.undecided(event, name, format!("{REFLECTION}:717-719"), reason),
        }

        match kind {
            AssocKind::BelongsTo => self.belongs_to_presence(event, decl),
            AssocKind::HasOne => self.has_one_presence(event, decl),
            _ => {}
        }
    }

    /// `define_autosave_validation_callbacks`'s body once `validate?` holds.
    fn validation_step(&mut self, event: &RailsEvent, name: &str) {
        if !self.validated.insert(name.to_string()) {
            return;
        }
        self.callback(
            event,
            name,
            Chain::Validate,
            Kind::Before,
            &format!("validate_associated_records_for_{name}"),
            format!("{AUTOSAVE}:231"),
        );
        self.callback(
            event,
            name,
            Chain::Validation,
            Kind::After,
            "_ensure_no_duplicate_errors",
            format!("{AUTOSAVE}:232"),
        );
    }

    fn belongs_to_presence(&mut self, event: &RailsEvent, decl: &AssocDecl) {
        let name = &decl.name;
        let rule = format!("{BELONGS_TO}:132-140");
        let required = self.belongs_to_required(decl);
        match required {
            Err(reason) => self.undecided(event, name, rule, reason),
            Ok(false) => {}
            Ok(true) => match self.defaults.belongs_to_required_validates_foreign_key {
                Some(true) => {
                    self.presence(event, name, Vec::new(), format!("{BELONGS_TO}:145-146"));
                }
                Some(false) => self.presence(
                    event,
                    name,
                    vec![(
                        "if".to_string(),
                        "foreign key (or polymorphic type) is nil or changed".to_string(),
                    )],
                    format!("{BELONGS_TO}:147-163"),
                ),
                None => self.undecided(
                    event,
                    name,
                    format!("{BELONGS_TO}:144-165"),
                    "`belongs_to_required_validates_foreign_key` decides whether the presence \
                     validator is conditional and is application configuration"
                        .to_string(),
                ),
            },
        }
    }

    /// Whether `belongs_to` is required (`builder/belongs_to.rb:132-140`).
    /// `required:` is rewritten to `optional: !required` and wins over a
    /// given `optional:`, so a splat that may supply `required:` hides
    /// `optional:` too.
    fn belongs_to_required(&self, decl: &AssocDecl) -> Result<bool, String> {
        if let Some(required) = option(decl, "required")? {
            return match required {
                Lit::True => Ok(true),
                Lit::False | Lit::Nil => Ok(false),
                Lit::Other(v) => Err(not_literal("required", &v)),
            };
        }
        match option(decl, "optional")? {
            Some(Lit::True) => Ok(false),
            Some(Lit::False) => Ok(true),
            Some(Lit::Other(v)) => Err(not_literal("optional", &v)),
            Some(Lit::Nil) | None => {
                self.defaults.belongs_to_required_by_default.ok_or_else(|| {
                    "neither `optional:` nor `required:` given, and \
                 `belongs_to_required_by_default` is application configuration"
                        .to_string()
                })
            }
        }
    }

    fn has_one_presence(&mut self, event: &RailsEvent, decl: &AssocDecl) {
        let required = match option(decl, "required") {
            Ok(required) => required,
            Err(reason) => {
                self.undecided(event, &decl.name, format!("{HAS_ONE}:32"), reason);
                return;
            }
        };
        match required {
            Some(Lit::True) => {
                self.presence(event, &decl.name, Vec::new(), format!("{HAS_ONE}:32-34"));
            }
            Some(Lit::Other(v)) => self.undecided(
                event,
                &decl.name,
                format!("{HAS_ONE}:32"),
                not_literal("required", &v),
            ),
            Some(Lit::False | Lit::Nil) | None => {}
        }
    }

    fn nested(&mut self, event: &RailsEvent, decl: &AssocDecl) {
        let name = &decl.name;
        let rule = format!("{NESTED}:361-362");
        let Some(target) = self.seen.get(name) else {
            self.undecided(
                event,
                name,
                rule,
                "no association of that name earlier in this class body; it may be inherited \
                 (Rails raises ArgumentError when there is none)"
                    .to_string(),
            );
            return;
        };
        // `reflection.autosave = true` makes `validate?` true unless an
        // explicit `validate:` says otherwise.
        let holds = match option(target, "validate") {
            Ok(Some(Lit::False)) => Ok(false),
            Ok(Some(Lit::True | Lit::Nil) | None) => Ok(true),
            Ok(Some(Lit::Other(v))) => Err(not_literal("validate", &v)),
            Err(reason) => Err(reason),
        };
        match holds {
            Ok(true) => self.validation_step(event, name),
            Ok(false) => {}
            Err(reason) => self.undecided(event, name, format!("{REFLECTION}:717-719"), reason),
        }
    }

    fn callback(
        &mut self,
        event: &RailsEvent,
        association: &str,
        chain: Chain,
        kind: Kind,
        method: &str,
        rule: String,
    ) {
        self.push_callback(
            event,
            association,
            chain,
            kind,
            Filter::Method(method.to_string()),
            Vec::new(),
            rule,
        );
    }

    fn presence(
        &mut self,
        event: &RailsEvent,
        association: &str,
        mut extra: Vec<(String, String)>,
        rule: String,
    ) {
        let mut options = vec![("message".to_string(), ":required".to_string())];
        options.append(&mut extra);
        self.push_callback(
            event,
            association,
            Chain::Validate,
            Kind::Before,
            Filter::PresenceValidator {
                attribute: association.to_string(),
            },
            options,
            rule,
        );
    }

    #[expect(clippy::too_many_arguments, reason = "one record's fields, passed 1:1")]
    fn push_callback(
        &mut self,
        event: &RailsEvent,
        association: &str,
        chain: Chain,
        kind: Kind,
        filter: Filter,
        options: Vec<(String, String)>,
        rule: String,
    ) {
        self.out.push(Generated::Callback(GeneratedCallback {
            association: association.to_string(),
            chain,
            kind,
            filter,
            options,
            origin: Origin {
                declaration_seq: event.seq,
                declaration_anchor: event.anchor,
                rule,
            },
        }));
    }

    fn undecided(&mut self, event: &RailsEvent, association: &str, rule: String, reason: String) {
        self.out.push(Generated::Undecided(Undecided {
            association: association.to_string(),
            declaration_seq: event.seq,
            declaration_anchor: event.anchor,
            rule,
            reason,
        }));
    }
}

/// `reflection.validate?` (`reflection.rb:717-719`).
fn validate_predicate(collection: bool, decl: &AssocDecl) -> Result<bool, String> {
    match option(decl, "validate")? {
        Some(Lit::True) => return Ok(true),
        Some(Lit::False) => return Ok(false),
        Some(Lit::Other(v)) => return Err(not_literal("validate", &v)),
        Some(Lit::Nil) | None => {}
    }
    if collection {
        return Ok(true);
    }
    match option(decl, "autosave")? {
        Some(Lit::True) => Ok(true),
        Some(Lit::False | Lit::Nil) | None => Ok(false),
        Some(Lit::Other(v)) => Err(not_literal("autosave", &v)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Symbols, source_ore};

    const ALL: RailsDefaults = RailsDefaults {
        belongs_to_required_by_default: Some(true),
        belongs_to_required_validates_foreign_key: Some(true),
    };

    fn run(body: &str) -> Vec<Generated> {
        run_with(body, ALL)
    }

    fn run_with(body: &str, defaults: RailsDefaults) -> Vec<Generated> {
        let src = format!("class M < ApplicationRecord\n{body}\nend\n");
        let mut syms = Symbols::default();
        let classes = source_ore(&src, "op", &mut syms);
        derive(&classes[0].declarations, defaults)
    }

    /// `chain/kind filter` lines, the shape a reader checks against Rails.
    fn lines(generated: &[Generated]) -> Vec<String> {
        generated
            .iter()
            .map(|g| match g {
                Generated::Callback(c) => {
                    let filter = match &c.filter {
                        Filter::Method(m) => m.clone(),
                        Filter::PresenceValidator { attribute } => {
                            format!("presence({attribute})")
                        }
                    };
                    format!("{:?}/{:?} {filter}", c.chain, c.kind)
                }
                Generated::Undecided(u) => format!("undecided({}): {}", u.association, u.reason),
            })
            .collect()
    }

    fn collection_set(name: &str) -> Vec<String> {
        vec![
            "Save/Around around_save_collection_association".to_string(),
            format!("Create/After autosave_associated_records_for_{name}"),
            format!("Update/After autosave_associated_records_for_{name}"),
            format!("Validate/Before validate_associated_records_for_{name}"),
            "Validation/After _ensure_no_duplicate_errors".to_string(),
        ]
    }

    #[test]
    fn has_many_generates_the_same_callbacks_for_autosave_absent_true_and_false() {
        for opts in ["", ", autosave: true", ", autosave: false"] {
            let got = lines(&run(&format!("has_many :journals{opts}")));
            assert_eq!(got, collection_set("journals"), "options `{opts}`");
        }
    }

    #[test]
    fn has_one_validates_only_when_autosave_is_true() {
        let save = |name: &str| {
            vec![
                format!("Create/After autosave_associated_records_for_{name}"),
                format!("Update/After autosave_associated_records_for_{name}"),
            ]
        };
        let validation = vec![
            "Validate/Before validate_associated_records_for_author".to_string(),
            "Validation/After _ensure_no_duplicate_errors".to_string(),
        ];
        assert_eq!(lines(&run("has_one :author")), save("author"));
        assert_eq!(
            lines(&run("has_one :author, autosave: false")),
            save("author")
        );
        let mut with = save("author");
        with.extend(validation);
        assert_eq!(lines(&run("has_one :author, autosave: true")), with);
    }

    #[test]
    fn belongs_to_saves_before_save_and_validates_only_when_autosave_is_true() {
        let optional = "optional: true";
        let save = vec!["Save/Before autosave_associated_records_for_project".to_string()];
        assert_eq!(
            lines(&run(&format!("belongs_to :project, {optional}"))),
            save
        );
        assert_eq!(
            lines(&run(&format!(
                "belongs_to :project, {optional}, autosave: false"
            ))),
            save
        );
        let mut with = save;
        with.push("Validate/Before validate_associated_records_for_project".to_string());
        with.push("Validation/After _ensure_no_duplicate_errors".to_string());
        assert_eq!(
            lines(&run(&format!(
                "belongs_to :project, {optional}, autosave: true"
            ))),
            with
        );
    }

    #[test]
    fn habtm_lists_the_unnameable_middle_association_then_the_visible_has_many() {
        let got = lines(&run("has_and_belongs_to_many :roles"));
        assert!(got[0].starts_with("undecided(roles): the internal middle has_many"));
        assert_eq!(got[1..], collection_set("roles"));
        // Only `validate` and `autosave` are forwarded to the visible has_many.
        let got = lines(&run("has_and_belongs_to_many :roles, validate: false"));
        assert_eq!(got.len(), 4, "{got:?}");
        assert!(got.iter().all(|l| !l.contains("validate_associated")));
    }

    #[test]
    fn validate_false_suppresses_the_validation_callback_even_on_a_collection() {
        let got = lines(&run("has_many :journals, validate: false"));
        assert_eq!(got, collection_set("journals")[..3].to_vec());
        // …and `validate: true` forces it on a singular association.
        let got = lines(&run("has_one :author, validate: true"));
        assert!(
            got.iter()
                .any(|l| l.contains("validate_associated_records_for_author"))
        );
    }

    #[test]
    fn belongs_to_optional_true_has_no_presence_validator_and_the_default_has() {
        let optional = lines(&run("belongs_to :project, optional: true"));
        assert!(
            !optional.iter().any(|l| l.contains("presence")),
            "{optional:?}"
        );

        let default = lines(&run("belongs_to :project"));
        assert_eq!(
            default,
            vec![
                "Save/Before autosave_associated_records_for_project".to_string(),
                "Validate/Before presence(project)".to_string(),
            ]
        );

        // `required: false` is `optional: true`; `optional: false` requires.
        let off = lines(&run("belongs_to :project, required: false"));
        assert!(!off.iter().any(|l| l.contains("presence")));
        let on = lines(&run("belongs_to :project, optional: false"));
        assert!(on.iter().any(|l| l.contains("presence(project)")));
    }

    #[test]
    fn belongs_to_default_follows_the_application_configuration() {
        let off = RailsDefaults {
            belongs_to_required_by_default: Some(false),
            ..ALL
        };
        let got = lines(&run_with("belongs_to :project", off));
        assert!(!got.iter().any(|l| l.contains("presence")));
        // Unknown configuration is not guessed.
        let got = lines(&run_with("belongs_to :project", RailsDefaults::default()));
        assert!(
            got.iter().any(|l| l.starts_with("undecided(project)")),
            "{got:?}"
        );
        // `optional:` decides without the configuration.
        let got = lines(&run_with(
            "belongs_to :project, optional: true",
            RailsDefaults::default(),
        ));
        assert!(!got.iter().any(|l| l.starts_with("undecided")));
    }

    #[test]
    fn presence_validator_carries_a_condition_unless_the_foreign_key_is_validated() {
        let conditional = RailsDefaults {
            belongs_to_required_validates_foreign_key: Some(false),
            ..ALL
        };
        let got = run_with("belongs_to :project", conditional);
        let Generated::Callback(c) = &got[1] else {
            panic!("expected the presence validator, got {got:?}");
        };
        assert!(c.options.iter().any(|(k, _)| k == "if"));
        assert!(c.origin.rule.ends_with("belongs_to.rb:147-163"));
        let got = run("belongs_to :project");
        let Generated::Callback(c) = &got[1] else {
            panic!("expected the presence validator, got {got:?}");
        };
        assert_eq!(
            c.options,
            vec![("message".to_string(), ":required".to_string())]
        );
    }

    #[test]
    fn has_one_required_adds_a_presence_validator() {
        let got = lines(&run("has_one :author, required: true"));
        assert_eq!(
            got.last().map(String::as_str),
            Some("Validate/Before presence(author)")
        );
        let got = lines(&run("has_one :author"));
        assert!(!got.iter().any(|l| l.contains("presence")));
    }

    #[test]
    fn accepts_nested_attributes_adds_the_validation_a_singular_association_lacked() {
        let got = lines(&run(
            "has_one :author\naccepts_nested_attributes_for :author",
        ));
        assert_eq!(
            got[2..],
            [
                "Validate/Before validate_associated_records_for_author".to_string(),
                "Validation/After _ensure_no_duplicate_errors".to_string(),
            ]
        );
        // A collection already has it: nothing is added twice.
        let got = lines(&run(
            "has_many :journals\naccepts_nested_attributes_for :journals",
        ));
        assert_eq!(got, collection_set("journals"));
        // An explicit `validate: false` still wins over the new autosave.
        let got = lines(&run(
            "has_one :author, validate: false\naccepts_nested_attributes_for :author",
        ));
        assert_eq!(got.len(), 2, "{got:?}");
        // An association that is not in this class body is not guessed.
        let got = lines(&run("accepts_nested_attributes_for :ghost"));
        assert!(got[0].starts_with("undecided(ghost)"), "{got:?}");
    }

    #[test]
    fn a_non_literal_option_is_undecided_not_guessed() {
        let got = lines(&run("has_one :author, autosave: SOME_FLAG"));
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(
            got[2].starts_with("undecided(author): `autosave:` is `SOME_FLAG`"),
            "{got:?}"
        );
        // The save callbacks do not depend on it and are still reported.
        assert!(got[0].starts_with("Create/After"));

        let got = lines(&run("has_many :journals, validate: flag?"));
        assert!(
            got.last()
                .is_some_and(|l| l.contains("`validate:` is `<expr>`")),
            "{got:?}"
        );

        let got = lines(&run("belongs_to :project, optional: allow?"));
        assert!(
            got.last()
                .is_some_and(|l| l.contains("`optional:` is `<expr>`")),
            "{got:?}"
        );

        // A collection is decided by its kind even when `autosave:` is opaque.
        let got = lines(&run("has_many :journals, autosave: flag?"));
        assert_eq!(got, collection_set("journals"));
    }

    #[test]
    fn callbacks_come_out_in_declaration_order_with_their_origin() {
        let got = run("belongs_to :project, optional: true\nhas_many :journals");
        let names: Vec<&str> = got
            .iter()
            .filter_map(|g| match g {
                Generated::Callback(c) => Some(c.association.as_str()),
                Generated::Undecided(_) => None,
            })
            .collect();
        assert_eq!(names.first().copied(), Some("project"));
        assert_eq!(names.last().copied(), Some("journals"));
        let Generated::Callback(first) = &got[0] else {
            panic!("expected a callback");
        };
        assert_eq!(first.origin.declaration_seq, 0);
        assert!(first.origin.rule.ends_with("autosave_association.rb:213"));
        let Generated::Callback(later) = &got[1] else {
            panic!("expected a callback");
        };
        assert_eq!(later.origin.declaration_seq, 1);
        assert!(later.origin.declaration_anchor > first.origin.declaration_anchor);
    }

    fn undecided_for(got: &[String], what: &str) -> bool {
        got.iter()
            .any(|l| l.starts_with("undecided(") && l.contains("**opts") && l.contains(what))
    }

    #[test]
    fn the_harvest_records_a_keyword_splat_apart_from_the_options() {
        let assoc = |body: &str| {
            let src = format!("class M < ApplicationRecord\n{body}\nend\n");
            let mut syms = Symbols::default();
            let classes = source_ore(&src, "op", &mut syms);
            let Some(Declaration::Association(decl)) = classes[0]
                .declarations
                .iter()
                .map(|e| &e.decl)
                .find(|d| matches!(d, Declaration::Association(_)))
            else {
                panic!("no association in {body}");
            };
            decl.clone()
        };
        let plain = assoc("has_many :x, autosave: true");
        assert!(plain.option_splats.is_empty());
        let splat = assoc("has_many :x, autosave: true, **opts, validate: false");
        // The splat is not a fake option pair.
        assert_eq!(splat.options.len(), 2, "{:?}", splat.options);
        assert!(splat.options.iter().all(|(k, _)| k != "**"));
        assert_eq!(splat.option_splats.len(), 1);
        assert_eq!(splat.option_splats[0].source, "**opts");
        assert_eq!(splat.option_splats[0].before, 1);
    }

    #[test]
    fn a_keyword_splat_is_never_read_as_absent_options() {
        // Negative control: without the splat these admit a validation / presence callback.
        let got = lines(&run("has_many :x, **opts"));
        assert!(undecided_for(&got, "`validate:`"), "{got:?}");
        assert!(
            !got.iter().any(|l| l.contains("validate_associated")),
            "{got:?}"
        );

        let got = lines(&run("has_one :x, **opts"));
        assert!(undecided_for(&got, "`validate:`"), "{got:?}");
        assert!(undecided_for(&got, "`required:`"), "{got:?}");
        assert!(!got.iter().any(|l| l.contains("presence")), "{got:?}");
        assert!(
            !got.iter().any(|l| l.contains("validate_associated")),
            "{got:?}"
        );

        // `belongs_to :x` is required by default; with a splat that may
        // supply `optional:` the presence validator is not admitted.
        let got = lines(&run("belongs_to :x, **opts"));
        assert!(undecided_for(&got, "`required:`"), "{got:?}");
        assert!(!got.iter().any(|l| l.contains("presence")), "{got:?}");
        assert!(
            !got.iter().any(|l| l.contains("validate_associated")),
            "{got:?}"
        );
    }

    #[test]
    fn the_unconditional_save_callbacks_survive_a_keyword_splat() {
        let got = lines(&run("has_many :x, **opts"));
        assert_eq!(got[..3], collection_set("x")[..3]);
        let got = lines(&run("has_one :x, **opts"));
        assert_eq!(
            got[..2],
            [
                "Create/After autosave_associated_records_for_x".to_string(),
                "Update/After autosave_associated_records_for_x".to_string(),
            ]
        );
        let got = lines(&run("belongs_to :x, **opts"));
        assert_eq!(got[0], "Save/Before autosave_associated_records_for_x");
    }

    #[test]
    fn an_explicit_key_decides_only_after_the_splat() {
        // `autosave` after the splat is known, but a splat may still supply
        // `validate:`, which `validate?` prefers (`reflection.rb:717-719`).
        let after = lines(&run("has_one :x, **opts, autosave: true"));
        assert!(undecided_for(&after, "`validate:`"), "{after:?}");
        assert!(!undecided_for(&after, "`autosave:`"), "{after:?}");
        // Every option a rule reads is given after the splat: decided.
        let both = lines(&run("has_one :x, **opts, validate: true, required: false"));
        assert!(!both.iter().any(|l| l.starts_with("undecided")), "{both:?}");
        assert!(both.iter().any(|l| l.contains("validate_associated")));
        let off = lines(&run("has_one :x, **opts, validate: false, required: false"));
        assert!(!off.iter().any(|l| l.starts_with("undecided")), "{off:?}");
        assert!(!off.iter().any(|l| l.contains("validate_associated")));

        let before = lines(&run("has_one :x, autosave: true, **opts"));
        assert!(undecided_for(&before, "`validate:`"), "{before:?}");
        assert!(!before.iter().any(|l| l.contains("validate_associated")));

        // belongs_to: `required` after the splat decides, `optional` after the
        // splat does not (a splat may still supply `required`).
        let req = lines(&run(
            "belongs_to :x, **opts, required: false, validate: false",
        ));
        assert!(!req.iter().any(|l| l.starts_with("undecided")), "{req:?}");
        assert!(!req.iter().any(|l| l.contains("presence")));
        let opt = lines(&run("belongs_to :x, **opts, optional: true"));
        assert!(undecided_for(&opt, "`required:`"), "{opt:?}");
    }

    #[test]
    fn a_keyword_splat_reaches_nested_attributes_and_habtm_forwarding() {
        let got = lines(&run("has_one :x, **opts\naccepts_nested_attributes_for :x"));
        assert!(got.last().is_some_and(|l| l.contains("**opts")), "{got:?}");
        // habtm forwards `validate` / `autosave`, which a splat may carry.
        let got = lines(&run("has_and_belongs_to_many :x, **opts"));
        assert!(undecided_for(&got, "`validate:`"), "{got:?}");
    }

    #[test]
    fn line_of_counts_from_one() {
        assert_eq!(line_of("a\nb\nc", 0), 1);
        assert_eq!(line_of("a\nb\nc", 2), 2);
        assert_eq!(line_of("a\nb\nc", 99), 3);
    }
}
