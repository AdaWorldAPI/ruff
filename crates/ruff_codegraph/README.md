# ruff_codegraph

A one-way projection of ruff's source harvest into the Codegraph store.

## What it is

- Reads a Rails `app/models` tree through `ruff_ruby_spo`.
- Writes the files `lance-graph-codegraph` reads, plus an order lane and a
  receipt.
- The file format is the only interface. The crate does not depend on
  lance-graph, `ruff_charon`, `ruff_graph` or `ty_*`, and nothing in it reads a
  store back into a `ModelGraph`.

## Files written

For a namespace `ns`, into one output directory:

- `ns.rows.tsv`, version 2. Header lines `#version 2` and `#schema` with the
  columns `predicate subject object grade file start end`. Rows are sorted
  (the `CRATE` row first, then by subject, predicate, object, grade) and
  de-duplicated.
- `ns.files.tsv`, version 1. Columns `path fnv64`: every `*.rb` file the harvest
  read, with its FNV-1a 64 digest in 16 hex digits. Paths start with the
  `root` of the config.
- `ns.residuals.tsv`, version 1. Columns `reason subject`. This crate writes two
  reasons: `CALLEE_NOT_NAMED` and `REFERENCE_NOT_NAMED`.
- `ns.events.tsv`, version 1. Columns
  `scope seq lane kind subject object anchor`. Only when the order lane runs.
- `ns.receipt.json`, version 1. Written last.

Subjects are spelled `ns::Model`, `ns::Model::function`, `ns::Model.field` and
`ns::Model::<phase>` for the node a callback phase hangs off. An association is
a `ns::Model.name` node. A cell never holds a tab or a newline. A name that
does would be a different name once repaired, so it is refused. Free text
(option values, DSL arguments, a call as written) is escaped (`\t`, `\n`, `\r`,
`\\`).

The `file`, `start` and `end` cells are always `-`. The Ruby frontend does not
report a file or a span per declaration. The file table comes from walking the
source root.

## Predicates and grades

Grade `SOURCE_STRUCTURE`, a declaration states the fact outright:

- `CRATE` (always `PARTIAL`: the frontend skips files it cannot parse without
  saying so), `DEFINES` (models, functions, fields, callback phase nodes),
  `PUBLIC`, `SYNTHETIC` (callback phase nodes), `INHERITS`.
- `INCLUDES`, `EXTENDS`, `PREPENDS`, `CONCERN_BLOCK`.
- `ASSOCIATION`, `ASSOCIATION_KIND`, `ASSOCIATION_OPTION`.
- `CALLBACK`, `CALLBACK_OPTION`, `VALIDATES`, `VALIDATES_OPTION`, `SCOPE`,
  `DELEGATES`, `DSL_CALL`, `FIELD_TYPE`.
- `IMPORTS`.

Grade `SOURCE_STRUCTURE+POSSIBLE`, derived from names alone:

- `READS`, `WRITES`, `GUARDED_WRITE`, `RAISES`, `TRAVERSES`.
- `CALLS_NAMED` and `UNRESOLVED_CALL`.

`CALLBACK_OPTION` objects read `target:key=value`, and `VALIDATES_OPTION`
objects read `macro:target:key=value`, so an option stays attached to the
declaration it came from. `VALIDATES` objects read `macro:target`.

## Why a name match is never a resolved call

`ruff_ruby_spo` records only calls to a closed set of ActiveRecord mutators
(`save`, `update`, `touch`, …) in `Function::calls`, as `receiver.method`. A
call whose receiver is `self` and whose method names exactly one function or
helper of the same model becomes `CALLS_NAMED`, with the callee's subject as
object. Every other call becomes `UNRESOLVED_CALL` with the call as written,
plus one `CALLEE_NOT_NAMED` residual.

A match by name is possible and never confirmed: the method may be overridden,
the receiver may not be the model's own object, and the frontend's receiver
labels are best effort. So no row has the predicate `CALLS_RESOLVED`, which the
store reserves for calls a compiler confirmed. A call on another receiver
(`order.update`) is never matched, even when the model defines `update`.

A callback whose target names no function of its model still gets its
`CALLBACK` row, and a `REFERENCE_NOT_NAMED` residual on the phase node. That
covers a block or lambda, the class form (`after_create Audit.new`, which the
frontend reports as `<block>`), and a target defined in a concern.

## What the receipt accounts for

`ns.receipt.json` has sorted keys, no timestamp and no absolute path.

- Format versions of each file written.
- The source snapshot: the file count and one FNV-1a 64 digest over the sorted
  `path<TAB>digest<LF>` lines of `files.tsv`.
- For `structural`, `order` and `imports`: `present` or `absent`.
- Rows per predicate, events per lane, residuals per reason.
- Conservation. For associations, callbacks, calls, fields, functions and
  validations, the number of distinct declarations in the source graph against
  the number of rows. For imports, edges against rows. Callback targets with no
  function against `REFERENCE_NOT_NAMED` residuals. Unresolved calls against
  `CALLEE_NOT_NAMED` residuals. The two sides are computed by separate code, and
  a projection that does not balance is an error and writes nothing.
- `not_projected`: declarations the frontend delivered that have no row
  (attributes other than `attribute`, enums, `acts_as`, gem DSLs, dynamic
  methods, refinements), counted by family.
- `direction`: `ruff->store`.

Counts are of distinct facts, because rows are a set. `before_save :a` declared
twice is one `CALLBACK` row. The order lane keeps both declarations.

## How the order lane answers a question the rows cannot

Rows are sorted, so they cannot say which of two `before_save` callbacks runs
first. The `DECL` events of a class (scope `ns::Model`) and the `BODY` events of
a method (scope `ns::Model::method`) keep the order the frontend's ordered walk
produced. Scopes are sorted. Inside a scope, rows are never sorted by value and
never de-duplicated: two identical events are two rows. `seq` and `anchor` (a
source byte offset) are the frontend's.

`order::relation(store_dir, ns, scope, a, b)` reads the receipt and
`ns.events.tsv` and returns `Before`, `After` or `Unknown`. A name matches an
event whose subject or object cell equals it. The answer is `Unknown` when the
receipt says the order lane is `absent`, when either name matches nothing in the
scope, when the names are equal, and when the occurrences of the two names
interleave. It never falls back to the structural rows.

## Next to a Rust store

`lance-graph-codegraph` reads a directory by listing the `*.rows.tsv` and
`*.rows.bin` files in it and treating each stem as a namespace. A store written
by `ruff_charon` (`<crate>.rows.tsv`) and one written here (`ns.rows.tsv`) can
share a directory when their namespaces differ. `ns.events.tsv` and
`ns.receipt.json` have neither suffix, so that reader does not list them.

## Running it

```sh
ruff_codegraph project <config.toml> <out_dir>
ruff_codegraph order <store_dir> <ns> <scope> <a> <b>
```

`order` prints `before`, `after` or `unknown`. Errors go to stderr with a
non-zero exit status.

The config is TOML. Nothing about a corpus is built in.

```toml
namespace = "shop"
frontend = "ruby-rails"        # the only frontend so far
root = "app/models"            # relative to the config file, inside its directory

[lanes]
structural = true              # default true
order = true                   # default true
imports = "imports.json"       # optional; omit for none
```

An unknown key, an unknown frontend, a namespace that is not a file stem and a
path that is absolute or climbs out of the config directory are errors that name
the key.

The imports file is the JSON `ruff analyze graph` prints: an object mapping a
file path to an array of the file paths it imports. Each edge becomes an
`IMPORTS` row whose subject and object are `ns::file:<path>`.

The tests project two revisions of a small synthetic tree in
`tests/data/rev1` and `tests/data/rev2` and compare the store files byte for
byte with `tests/data/expected`. `RUFF_CODEGRAPH_BLESS=1 cargo test -p
ruff_codegraph` rewrites the expected files.

## What it does not do

- Only the Ruby/Rails frontend is wired. There is no Python, C++ or C# frontend
  here.
- No spans and no per-row file. Every `file`, `start` and `end` cell is `-`.
- Fields come only from `attribute` declarations and from `Model::fields`, which
  the Rails model walk leaves empty. Database columns are not read.
- Concern modules (`module X; extend ActiveSupport::Concern`) are not walked: the
  frontend collects classes only, so a concern appears only as the object of an
  `INCLUDES` row and in the file table.
- The event lane drops the `control` tag of a body event, and body events are
  not tied to the structural rows by anything but the scope name.
- Imports come only from a supplied JSON. This crate does not run
  `ruff analyze graph`.
- The reader on the lance-graph side does not read `events.tsv` or
  `receipt.json`; nothing there answers the order question yet.
