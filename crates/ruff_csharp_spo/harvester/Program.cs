// Roslyn C# -> SPO harvester.
//
// Walks a C# source tree and emits one `ruff_spo_triplet::Triple` per line of
// ndjson: {"s":..,"p":..,"o":..,"f":..,"c":..}. The predicate strings are the
// closed `ruff_spo_triplet::Predicate` vocabulary; `ruff_csharp_spo::load`
// rejects anything outside it.
//
// Usage:
//   dotnet run --project CSharpSpoHarvester.csproj -- <source-root> [out.ndjson] [flags]
//
// Flags (all optional; defaults reproduce the original EF-Core-flavoured
// behaviour so existing invocations are unaffected):
//   --ns <name>                      IRI namespace prefix (default "csharp")
//   --mutator-names a,b,c            exact persistence-mutator method names
//                                    that make a `calls` fact fire (default:
//                                    the EF Core set below)
//   --mutator-prefixes add_,del_     method-name PREFIXES that also count as
//                                    mutators (default: none) — for bespoke
//                                    ADO.NET DALs with a naming convention
//                                    instead of a fixed method set (e.g.
//                                    a hand-rolled DAL's `add_*`/`del_*`/`set_*`)
//   --mutator-receivers mysql        restrict the `calls` fact to invocations
//                                    whose receiver's last identifier segment
//                                    is in this list (default: none = any
//                                    receiver). `main.mysql.add_x(...)`
//                                    matches receiver `mysql`; a form's own
//                                    `set_Foo(...)` does not.
//
// Scaffold scope: syntax layer only (declarations + names as written). Symbol
// resolution (fully-qualified bases, overrides, attribute binding) is the
// SemanticModel upgrade documented in README.md.

using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

// Structural facts harvested from declarations are certain by construction;
// mirror ruff_spo_triplet's "declared/structural" provenance tier.
const double f = 1.0;
const double c = 0.9;

// Original scaffold's closed EF-Core persistence-mutator set — kept as the
// default `--mutator-names` so invocations with no flags are unchanged.
var defaultMutatorNames = new[]
{
    "SaveChanges", "SaveChangesAsync", "Update", "UpdateRange",
    "Add", "AddRange", "Remove", "RemoveRange", "Delete",
    "ExecuteDelete", "ExecuteUpdate",
};

var ns = "csharp";
var mutatorNames = new HashSet<string>(defaultMutatorNames, StringComparer.Ordinal);
var mutatorPrefixes = new List<string>();
var mutatorReceivers = new List<string>();
// Navigation-selector property names: an assignment `X.<Prop> = target` where
// <Prop> is one of these is a nav edge to `target` (the ribbon/tab-swap idiom,
// e.g. DevExpress `ribbon.SelectedRibbonTabItem = menu_tab_X`). Configurable so
// a bespoke shell can add its own selector; the default set is the common
// WinForms/DevExpress ones.
var navSelectProps = new HashSet<string>(
    new[] { "SelectedRibbonTabItem", "SelectedTabPage", "SelectedPage", "SelectedTab" },
    StringComparer.Ordinal);
var positional = new List<string>();

// ── UI-config plane (Phase 0 labyrinth recon, the room map) ──
// Data-as-config: the corpus owner supplies the room→concept alias table
// (`--room-aliases con_lab=lab_value,…`, keyed by source-directory name —
// empty by default: the binding is the owner's claim, never a built-in
// guess) and optional room NAME prefixes (`--room-prefixes uc_,mod_,con_`)
// that extend screen classification beyond the base-type check for corpora
// whose screens follow a naming discipline.
var roomAliases = new Dictionary<string, string>(StringComparer.Ordinal);
var roomPrefixes = new List<string>();

for (var i = 0; i < args.Length; i++)
{
    switch (args[i])
    {
        case "--ns" when i + 1 < args.Length:
            ns = args[++i];
            break;
        case "--mutator-names" when i + 1 < args.Length:
            mutatorNames = new HashSet<string>(SplitCsv(args[++i]), StringComparer.Ordinal);
            break;
        case "--mutator-prefixes" when i + 1 < args.Length:
            mutatorPrefixes = SplitCsv(args[++i]);
            break;
        case "--mutator-receivers" when i + 1 < args.Length:
            mutatorReceivers = SplitCsv(args[++i]);
            break;
        case "--nav-select-props" when i + 1 < args.Length:
            navSelectProps = new HashSet<string>(SplitCsv(args[++i]), StringComparer.Ordinal);
            break;
        case "--room-aliases" when i + 1 < args.Length:
            foreach (var kv in SplitCsv(args[++i]))
            {
                var eq = kv.IndexOf('=');
                if (eq > 0)
                {
                    roomAliases[kv[..eq]] = kv[(eq + 1)..];
                }
            }
            break;
        case "--room-prefixes" when i + 1 < args.Length:
            roomPrefixes = SplitCsv(args[++i]);
            break;
        default:
            positional.Add(args[i]);
            break;
    }
}

if (positional.Count < 1)
{
    Console.Error.WriteLine(
        "usage: csharp-spo-harvest <source-root> [out.ndjson] " +
        "[--ns <name>] [--mutator-names a,b,c] [--mutator-prefixes add_,del_] " +
        "[--mutator-receivers mysql] [--nav-select-props SelectedRibbonTabItem,SelectedTab] " +
        "[--room-aliases con_lab=lab_value,…] [--room-prefixes uc_,mod_,con_]");
    return 2;
}

var root = positional[0];
if (!Directory.Exists(root))
{
    Console.Error.WriteLine($"error: source root not found: {root}");
    return 2;
}

// The one `calls`-fact predicate for this run, closed over the configured
// name set / prefixes / receivers — built once, passed down explicitly
// (EmitBodyArm is a `static` local function and cannot capture top-level
// variables).
var isMutatorCall = BuildMutatorPredicate(mutatorNames, mutatorPrefixes, mutatorReceivers);

var triples = new List<Triple>();

// Parse every file once — reused by the screen-type pre-pass and the main pass.
// The source path rides along so the room-alias binding can key on the
// screen's source-directory name.
var parsed = new List<(string File, SyntaxNode Root)>();
foreach (var file in Directory.EnumerateFiles(root, "*.cs", SearchOption.AllDirectories))
{
    try
    {
        parsed.Add((file, CSharpSyntaxTree.ParseText(File.ReadAllText(file)).GetRoot()));
    }
    catch (IOException ex)
    {
        Console.Error.WriteLine($"skip {file}: {ex.Message}");
    }
}

// Pre-pass — the SCREEN-TYPE universe: every class whose base type is
// `UserControl` or ends in `Form` (`Form` / `XtraForm` / `Office2007Form` / …).
// Agnostic + syntactic (no app-specific prefixes): a WinForms/DevExpress screen
// is a UserControl/Form subclass. This is what lets the nav arm emit an edge on
// the UserControl-SPA idiom `field = new SomeScreen(...)` — the common case in
// a ribbon/panel-swap app — and not only on the MDI idiom `new Form().Show()`.
// Extended to FIXPOINT through corpus base chains (`class X : uc_Grid` where
// `uc_Grid : UserControl` is a screen too) and, when the corpus follows a
// room naming discipline, through `--room-prefixes`. Also records each
// type's source-directory name for the `--room-aliases` concept binding.
var screenTypes = new HashSet<string>(StringComparer.Ordinal);
var typeBases = new Dictionary<string, List<string>>(StringComparer.Ordinal);
var typeDir = new Dictionary<string, string>(StringComparer.Ordinal);
foreach (var (file, preRoot) in parsed)
{
    var dir = Path.GetFileName(Path.GetDirectoryName(file) ?? string.Empty);
    foreach (var type in preRoot.DescendantNodes().OfType<TypeDeclarationSyntax>())
    {
        var tname = type.Identifier.Text;
        typeDir.TryAdd(tname, dir);
        if (!typeBases.TryGetValue(tname, out var blist))
        {
            typeBases[tname] = blist = new List<string>();
        }
        if (type.BaseList is not null)
        {
            blist.AddRange(type.BaseList.Types.Select(b => LastSegment(BareName(b.Type))));
        }
        if (roomPrefixes.Any(pfx => tname.StartsWith(pfx, StringComparison.Ordinal)))
        {
            screenTypes.Add(tname);
        }
    }
}
var screensChanged = true;
while (screensChanged)
{
    screensChanged = false;
    foreach (var (tname, blist) in typeBases)
    {
        if (!screenTypes.Contains(tname)
            && blist.Any(b => IsScreenBase(b) || screenTypes.Contains(b)))
        {
            screenTypes.Add(tname);
            screensChanged = true;
        }
    }
}

foreach (var (_, rootNode) in parsed)
{
    // class / struct / record / interface declarations all share TypeDeclarationSyntax.
    foreach (var type in rootNode.DescendantNodes().OfType<TypeDeclarationSyntax>())
    {
        var name = type.Identifier.Text;
        var subj = $"{ns}:{name}";

        // (Class, rdf:type, ogit:ObjectType) — structural classification.
        triples.Add(new Triple(subj, "rdf:type", "ogit:ObjectType", f, c));

        // ── UI-CONFIG PLANE (Phase 0 labyrinth recon, the room map) ──
        // The config half of the nav harvest: the room→concept binding
        // (corpus-owner claim, Authoritative) + Designer event wiring +
        // control-tree containment. The Klickweg EDGES stay EmitNavArm's
        // job below. Type-level walk (covers constructors and
        // InitializeComponent partials alike).
        if (screenTypes.Contains(name))
        {
            if (roomAliases.Count > 0
                && typeDir.TryGetValue(name, out var roomDir)
                && roomAliases.TryGetValue(roomDir, out var concept))
            {
                triples.Add(new Triple(subj, "surfaces_concept", concept, 0.95, 0.90));
            }
            EmitUiConfigArm(triples, ns, name, subj, type);
        }

        // (Class, inherits_from, Base) for each base type / interface, name as written.
        if (type.BaseList is not null)
        {
            foreach (var b in type.BaseList.Types)
            {
                triples.Add(new Triple(subj, "inherits_from", $"{ns}:{BareName(b.Type)}", f, c));
            }
        }

        foreach (var member in type.Members)
        {
            switch (member)
            {
                case PropertyDeclarationSyntax p:
                    EmitField(triples, ns, subj, name, p.Identifier.Text, p.Type, f, c);
                    break;

                case FieldDeclarationSyntax fd:
                    foreach (var v in fd.Declaration.Variables)
                    {
                        EmitField(triples, ns, subj, name, v.Identifier.Text, fd.Declaration.Type, f, c);
                    }
                    break;

                case MethodDeclarationSyntax m:
                    {
                        var msubj = $"{ns}:{name}.{m.Identifier.Text}";
                        triples.Add(new Triple(subj, "has_function", msubj, f, c));
                        triples.Add(new Triple(msubj, "rdf:type", "ogit:Function", f, c));
                        if (m.Modifiers.Any(t => t.IsKind(SyntaxKind.StaticKeyword)))
                        {
                            triples.Add(new Triple(msubj, "is_static", "true", f, c));
                        }

                        // AST-DLL signature plane (returns_type / has_param_type /
                        // has_visibility) — mirrors the C++ frontend's cpp_method
                        // (ruff_spo_triplet::expand.rs cpp_method) so the codegen's
                        // "helpers/private split" (fuzzy-recipe-codebook.md §2) and
                        // the AST-DLL signature shape work identically for C#.
                        if (m.ReturnType.ToString() != "void")
                        {
                            triples.Add(new Triple(msubj, "returns_type", m.ReturnType.ToString(), f, c));
                        }
                        var parameters = m.ParameterList.Parameters;
                        for (var pi = 0; pi < parameters.Count; pi++)
                        {
                            if (parameters[pi].Type is { } pType)
                            {
                                triples.Add(new Triple(msubj, "has_param_type", $"{pi}:{pType}", f, c));
                            }
                        }
                        // Always present — every method has a visibility, matching
                        // the C++ frontend's "always present" access specifier.
                        triples.Add(new Triple(
                            msubj,
                            "has_visibility",
                            VisibilityOf(m.Modifiers, type is InterfaceDeclarationSyntax),
                            f,
                            c));

                        // DTO ARM (syntax-only) — the body-fact fingerprint
                        // the fuzzy recipe-codebook needs (ruff/.claude/
                        // knowledge/fuzzy-recipe-codebook.md §2). Populates
                        // writes_field / reads_field / raises / calls /
                        // writes_if_blank for a C# method body, so the same
                        // recipe centroids that classify Rails hooks classify
                        // C# `OnSaving`/`SaveChanges` overrides + property
                        // setters, and bespoke ADO.NET DALs (`add_*`/`del_*`
                        // naming, no ORM) via
                        // --mutator-prefixes/--mutator-receivers. TESTED:
                        // built + run with dotnet-sdk-8 against
                        // `fixtures/recipe_shapes.cs` (all 7 recipe centroids
                        // classify correctly, `ruff_csharp_spo` unit tests
                        // round-trip the emitted predicates) AND end-to-end
                        // against a real production C# corpus (~97k triples,
                        // `ruff_csharp_spo::load` validates all of them).
                        EmitBodyArm(triples, ns, name, msubj, m.Body, m.ExpressionBody, f, c, isMutatorCall);

                        // UI-NAVIGATION ARM — the WinForms form→form Klickweg
                        // edge (navigates_to). Subject is the CLASS that
                        // navigates (not the method), so a screen's edges are
                        // the union over all its handlers.
                        EmitNavArm(triples, ns, name, m.Body, m.ExpressionBody, screenTypes, navSelectProps, f, c);
                        break;
                    }

                default:
                    break;
            }
        }
    }
}

var json = new JsonSerializerOptions { DefaultIgnoreCondition = JsonIgnoreCondition.Never };
using (var w = positional.Count > 1
           ? new StreamWriter(positional[1])
           : new StreamWriter(Console.OpenStandardOutput()))
{
    foreach (var t in triples)
    {
        w.WriteLine(JsonSerializer.Serialize(t, json));
    }
}

Console.Error.WriteLine($"harvested {triples.Count} triples from {root}");
return 0;

// UI-CONFIG ARM (Phase 0 labyrinth recon) — the room-map facts for one
// screen type. Walks ALL descendant nodes of the type (constructors +
// InitializeComponent + handlers alike; partial Designer classes are their
// own TypeDeclarationSyntax and get their own walk), emitting:
//
//   handles_event    `this.<control>.<Event> += new …(this.<Handler>)`
//                    → (Class.control, handles_event, "<Event>:<ns>:Class.Handler")
//                    Authoritative: the `+=` names both ends.
//   contains_control `this.<parent>.Controls.Add(this.<child>)`
//                    → (Class.parent|Class, contains_control, ns:Class.child)
//                    Authoritative: machine-readable containment call.
//   docked_at        `this.<ctrl>.Dock = System.Windows.Forms.DockStyle.<Style>;`
//                    (also matches the unqualified `DockStyle.<Style>` form —
//                    only the outermost member-access segment is read either way)
//                    → (Class.ctrl, docked_at, "<style, lowercase>")
//                    Authoritative: DockStyle is a closed WinForms enum.
//   tab_order        `this.<ctrl>.TabIndex = <N>;`
//                    → (Class.ctrl, tab_order, "<N>")
//                    Authoritative: TabIndex is a machine-readable int literal.
//   opens_popup      `this.<ctrl>.ContextMenuStrip = this.<menuCtrl>;`
//                    → (Class.ctrl, opens_popup, ns:Class.menuCtrl)
//                    Authoritative: machine-readable Designer assignment.
//
// The Klickweg EDGES (navigates_to / selects_view) are EmitNavArm's job —
// this arm carries only the room map. Syntax-only, no SemanticModel.
static void EmitUiConfigArm(
    List<Triple> triples,
    string ns,
    string className,
    string classSubj,
    TypeDeclarationSyntax type)
{
    // The `<ns>:<Class>.<member>` IRI for a `this.<member>` / bare-identifier
    // receiver chain, or the class subject itself for a bare `this`.
    string SubjectOf(ExpressionSyntax receiver) => receiver switch
    {
        ThisExpressionSyntax => classSubj,
        IdentifierNameSyntax id => $"{ns}:{className}.{id.Identifier.Text}",
        MemberAccessExpressionSyntax ma => $"{ns}:{className}.{ma.Name.Identifier.Text}",
        _ => classSubj,
    };

    static string? LastIdentifier(ExpressionSyntax e) => e switch
    {
        IdentifierNameSyntax id => id.Identifier.Text,
        MemberAccessExpressionSyntax ma => ma.Name.Identifier.Text,
        _ => null,
    };

    foreach (var node in type.DescendantNodes())
    {
        switch (node)
        {
            // Designer event wiring: <control-chain>.<Event> += <handler>.
            case AssignmentExpressionSyntax asg
                when asg.IsKind(SyntaxKind.AddAssignmentExpression)
                     && asg.Left is MemberAccessExpressionSyntax evAccess:
                {
                    // Handler: `new EventHandler(this.Foo_Click)` or a bare
                    // method group `this.Foo_Click` / `Foo_Click`.
                    var handler = asg.Right switch
                    {
                        ObjectCreationExpressionSyntax oc
                            when oc.ArgumentList is { Arguments.Count: > 0 } =>
                            LastIdentifier(oc.ArgumentList.Arguments[0].Expression),
                        _ => LastIdentifier(asg.Right),
                    };
                    if (handler is not null)
                    {
                        var control = SubjectOf(evAccess.Expression);
                        var ev = evAccess.Name.Identifier.Text;
                        triples.Add(new Triple(
                            control,
                            "handles_event",
                            $"{ev}:{ns}:{className}.{handler}",
                            0.95,
                            0.90));
                    }
                    break;
                }

            // Designer containment: <parent-chain>.Controls.Add(<child>).
            case InvocationExpressionSyntax inv
                when inv.Expression is MemberAccessExpressionSyntax
                     {
                         Name.Identifier.Text: "Add",
                         Expression: MemberAccessExpressionSyntax
                         {
                             Name.Identifier.Text: "Controls",
                         } controlsAccess,
                     }
                     && inv.ArgumentList.Arguments.Count > 0:
                {
                    var child = LastIdentifier(inv.ArgumentList.Arguments[0].Expression);
                    if (child is not null)
                    {
                        triples.Add(new Triple(
                            SubjectOf(controlsAccess.Expression),
                            "contains_control",
                            $"{ns}:{className}.{child}",
                            0.95,
                            0.90));
                    }
                    break;
                }

            // Designer layout: <control-chain>.Dock = System.Windows.Forms.DockStyle.<Style>
            // (also matches the unqualified `DockStyle.<Style>` form — LastIdentifier
            // only reads the outermost member-access segment either way).
            case AssignmentExpressionSyntax dockAsg
                when dockAsg.IsKind(SyntaxKind.SimpleAssignmentExpression)
                     && dockAsg.Left is MemberAccessExpressionSyntax { Name.Identifier.Text: "Dock" } dockAccess
                     && LastIdentifier(dockAsg.Right) is { } dockStyle:
                {
                    triples.Add(new Triple(
                        SubjectOf(dockAccess.Expression),
                        "docked_at",
                        dockStyle.ToLowerInvariant(),
                        0.95,
                        0.90));
                    break;
                }

            // Designer layout: <control-chain>.TabIndex = <N>.
            case AssignmentExpressionSyntax tabAsg
                when tabAsg.IsKind(SyntaxKind.SimpleAssignmentExpression)
                     && tabAsg.Left is MemberAccessExpressionSyntax { Name.Identifier.Text: "TabIndex" } tabAccess
                     && tabAsg.Right is LiteralExpressionSyntax
                        { RawKind: (int)SyntaxKind.NumericLiteralExpression } tabLiteral:
                {
                    triples.Add(new Triple(
                        SubjectOf(tabAccess.Expression),
                        "tab_order",
                        tabLiteral.Token.ValueText,
                        0.95,
                        0.90));
                    break;
                }

            // Designer popup wiring: <control-chain>.ContextMenuStrip = this.<menuCtrl>.
            case AssignmentExpressionSyntax menuAsg
                when menuAsg.IsKind(SyntaxKind.SimpleAssignmentExpression)
                     && menuAsg.Left is MemberAccessExpressionSyntax
                        { Name.Identifier.Text: "ContextMenuStrip" } menuAccess
                     && menuAsg.Right is MemberAccessExpressionSyntax:
                {
                    triples.Add(new Triple(
                        SubjectOf(menuAccess.Expression),
                        "opens_popup",
                        SubjectOf(menuAsg.Right),
                        0.95,
                        0.90));
                    break;
                }

            default:
                break;
        }
    }
}

// (Class, has_field, Class.field) + (Class.field, rdf:type, ogit:Property)
//                                 + (Class.field, field_type, <Type as written>)
static void EmitField(
    List<Triple> triples,
    string ns,
    string classSubj,
    string className,
    string field,
    TypeSyntax type,
    double f,
    double c)
{
    var fsubj = $"{ns}:{className}.{field}";
    triples.Add(new Triple(classSubj, "has_field", fsubj, f, c));
    triples.Add(new Triple(fsubj, "rdf:type", "ogit:Property", f, c));
    triples.Add(new Triple(fsubj, "field_type", type.ToString(), f, c));
}

// DTO ARM (syntax-only) — the body-fact fingerprint for the fuzzy
// recipe-codebook (ruff/.claude/knowledge/fuzzy-recipe-codebook.md §2). Emits,
// for one C# method body, the SAME predicates the Ruby frontend emits so the
// recipe centroids are language-agnostic:
//   writes_field    `this.X = …` / `X = …`     assignment to a member
//   reads_field     `this.X` / bare `X`         member read
//   raises          `throw new XException(…)`   abort signal
//   calls           `ctx.SaveChanges()` / …     persistence-mutator dispatch
//   writes_if_blank `X ??= v` / `if (X==null) X = v`   J1 default-vs-normalize
// Syntax-only, no SemanticModel: a bare `X` is heuristically a member read
// (Inferred, matching Ruby's convention) — a SemanticModel upgrade would prune
// locals/params. `isMutatorCall` is the configured persistence-mutator
// predicate (`--mutator-names`/`--mutator-prefixes`/`--mutator-receivers`,
// see `BuildMutatorPredicate`) — the C# analogue of Ruby's closed
// AR_MUTATORS, made agnostic to ORM vs bespoke ADO.NET DAL naming.
static void EmitBodyArm(
    List<Triple> triples,
    string ns,
    string className,
    string msubj,
    BlockSyntax? body,
    ArrowExpressionClauseSyntax? expressionBody,
    double f,
    double c,
    Func<MemberAccessExpressionSyntax, bool> isMutatorCall)
{
    // Unify block-bodied (`{ … }`) and expression-bodied (`=> …`) methods:
    // walk whichever node holds the body.
    SyntaxNode? root = (SyntaxNode?)body ?? expressionBody?.Expression;
    if (root is null)
    {
        return;
    }

    // LHS of an assignment -> the member name it writes, or null if the LHS is
    // not a plain member (`this.X` / `X`). Indexers, tuples, locals -> null.
    static string? WrittenMember(ExpressionSyntax lhs) => lhs switch
    {
        // `this.X = …`
        MemberAccessExpressionSyntax ma
            when ma.Expression is ThisExpressionSyntax => ma.Name.Identifier.Text,
        // `X = …` (bare — may be a local; SemanticModel would confirm it is a
        // member. Syntax-only keeps it, matching Ruby's bare-attr convention.)
        IdentifierNameSyntax id => id.Identifier.Text,
        _ => null,
    };

    foreach (var node in root.DescendantNodesAndSelf())
    {
        switch (node)
        {
            // ── writes_field + J1 writes_if_blank ──
            case AssignmentExpressionSyntax asgn:
                var w = WrittenMember(asgn.Left);
                if (w is not null)
                {
                    triples.Add(new Triple(msubj, "writes_field", $"{ns}:{className}.{w}", f, c));
                    // J1: `X ??= v` is the null-coalescing default (the C#
                    // spelling of Ruby `x ||= v` / `x = v if x.blank?`).
                    if (asgn.OperatorToken.IsKind(SyntaxKind.QuestionQuestionEqualsToken))
                    {
                        triples.Add(new Triple(msubj, "writes_if_blank", $"{ns}:{className}.{w}", f, c));
                    }
                }
                break;

            // ── J1 writes_if_blank via the `if (X == null) X = v` guard ──
            case IfStatementSyntax ifs
                when NullGuardedField(ifs.Condition) is string gf:
                // Any write to `gf` inside the guarded branch is a default.
                foreach (var inner in ifs.Statement.DescendantNodesAndSelf()
                             .OfType<AssignmentExpressionSyntax>())
                {
                    if (WrittenMember(inner.Left) == gf)
                    {
                        triples.Add(new Triple(msubj, "writes_if_blank", $"{ns}:{className}.{gf}", f, c));
                    }
                }
                break;

            // ── raises ──
            case ThrowStatementSyntax { Expression: ObjectCreationExpressionSyntax oce }:
                triples.Add(new Triple(msubj, "raises", $"exc:{BareName(oce.Type)}", f, c));
                break;
            case ThrowExpressionSyntax { Expression: ObjectCreationExpressionSyntax oce2 }:
                triples.Add(new Triple(msubj, "raises", $"exc:{BareName(oce2.Type)}", f, c));
                break;

            // ── calls (persistence mutators only) ──
            case InvocationExpressionSyntax { Expression: MemberAccessExpressionSyntax mac }
                when isMutatorCall(mac):
                // "receiver.method" verbatim, like Ruby's calls object.
                triples.Add(new Triple(msubj, "calls", $"{mac.Expression}.{mac.Name.Identifier.Text}", f, c));
                break;

            // ── reads_field (`this.X` member reads) ──
            // EXCLUDE the assignment LHS: `this.X = …` — the LHS `this.X` is
            // also a MemberAccess node, but it is a WRITE, not a read. Counting
            // it as a read would make `this.X = f(y)` look like `W ⊆ R` (a
            // SelfMap) when it is a Compute — the dangerous over-read direction.
            case MemberAccessExpressionSyntax { Expression: ThisExpressionSyntax } thisRead
                when !(thisRead.Parent is AssignmentExpressionSyntax pa && pa.Left == thisRead):
                triples.Add(new Triple(msubj, "reads_field", $"{ns}:{className}.{thisRead.Name.Identifier.Text}", f, c));
                break;
        }
    }
}

// UI-NAVIGATION ARM (syntax-only) — the WinForms form→form Klickweg edge.
// Emits `(csharp:ThisClass, navigates_to, csharp:TargetForm)` when a method
// body opens another screen via `new TargetForm().Show()` / `.ShowDialog()`
// (one-liner) or the two-statement `var f = new TargetForm(); …; f.Show();`
// local-tracking pattern. Subject is the CLASS that navigates (not the method),
// so a screen's edge set is the union over all its handlers. Framework
// CommonDialogs (MessageBox / {Open,Save}FileDialog / …) are modal system
// dialogs, not application screens, so they are excluded — navigates_to means
// "opens another screen", the Klickweg. `navigates_to` is Inferred in
// ruff_spo_triplet: syntax-only, and the two-statement local-tracking is
// heuristic (a SemanticModel upgrade would resolve the receiver's type).
static void EmitNavArm(
    List<Triple> triples,
    string ns,
    string className,
    BlockSyntax? body,
    ArrowExpressionClauseSyntax? expressionBody,
    HashSet<string> screenTypes,
    HashSet<string> navSelectProps,
    double f,
    double c)
{
    SyntaxNode? root = (SyntaxNode?)body ?? expressionBody?.Expression;
    if (root is null)
    {
        return;
    }

    // Framework CommonDialogs / message boxes — modal system dialogs, NOT app
    // screens; excluded so a navigates_to edge always names a real screen.
    var frameworkDialogs = new HashSet<string>(StringComparer.Ordinal)
    {
        "MessageBox", "OpenFileDialog", "SaveFileDialog", "FolderBrowserDialog",
        "ColorDialog", "FontDialog", "PrintDialog", "PageSetupDialog",
        "PrintPreviewDialog",
    };

    // Pre-pass: local variable -> instantiated type, for the two-statement
    // `var f = new TargetForm();` / `TargetForm f = new TargetForm();` pattern.
    var localType = new Dictionary<string, string>(StringComparer.Ordinal);
    foreach (var decl in root.DescendantNodesAndSelf().OfType<VariableDeclaratorSyntax>())
    {
        if (decl.Initializer?.Value is ObjectCreationExpressionSyntax oce)
        {
            localType[decl.Identifier.Text] = LastSegment(BareName(oce.Type));
        }
    }

    // Walk for `.Show()` / `.ShowDialog()` and resolve the target screen.
    // Dedup per method body; the SPO store dedups across methods by key.
    var seen = new HashSet<string>(StringComparer.Ordinal);
    foreach (var inv in root.DescendantNodesAndSelf().OfType<InvocationExpressionSyntax>())
    {
        if (inv.Expression is not MemberAccessExpressionSyntax mac)
        {
            continue;
        }
        var method = mac.Name.Identifier.Text;
        if (method != "Show" && method != "ShowDialog")
        {
            continue;
        }
        string? target = mac.Expression switch
        {
            // `new TargetForm(...).Show()`
            ObjectCreationExpressionSyntax oce => LastSegment(BareName(oce.Type)),
            // `f.Show()` where `f` was `new TargetForm()` earlier in the body
            IdentifierNameSyntax id when localType.TryGetValue(id.Identifier.Text, out var t) => t,
            _ => null,
        };
        if (target is null || frameworkDialogs.Contains(target) || !seen.Add(target))
        {
            continue;
        }
        triples.Add(new Triple($"{ns}:{className}", "navigates_to", $"{ns}:{target}", f, c));
    }

    // UserControl-SPA idiom — the dominant case in a ribbon/panel-swap app:
    // `field = new SomeScreen(...)` (no `.Show()`; the instance is hosted into a
    // panel elsewhere). A screen instantiating another SCREEN TYPE (pre-pass
    // set) is a navigation edge. This is syntax-only reachability — it does NOT
    // distinguish a *stack* (composition: a screen embeds a sub-view) from a
    // *jump* (navigation: a screen opens another): both spell `new Screen()`.
    // So it harvests the composition+navigation CLOSURE; splitting stack vs jump
    // needs more signal (is the instance added to own layout vs a content host)
    // and is left to the consumer. Framework dialogs never enter `screenTypes`
    // (they end in `Dialog`, not `Form`), so they cannot be emitted here.
    foreach (var oce in root.DescendantNodesAndSelf().OfType<ObjectCreationExpressionSyntax>())
    {
        var target = LastSegment(BareName(oce.Type));
        if (target == className || !screenTypes.Contains(target) || !seen.Add(target))
        {
            continue;
        }
        triples.Add(new Triple($"{ns}:{className}", "navigates_to", $"{ns}:{target}", f, c));
    }

    // Selector-assignment idiom — the ribbon/tab top-nav: `X.<NavProp> = target`
    // where <NavProp> is a configured navigation selector (default
    // SelectedRibbonTabItem / SelectedTabPage / SelectedPage / SelectedTab). The
    // target is the assigned identifier (the tab/page field, e.g. a DevExpress
    // RibbonTabItem), NOT a screen class — so it is emitted as `selects_view`,
    // never `navigates_to` (codex P2 on #64): `navigates_to` stays a pure
    // screen→screen graph, and selector VALUES live on their own predicate so
    // the screen graph never carries dangling non-screen nodes. Syntax-only
    // cannot resolve which screen a tab shows (Designer/runtime wiring); the
    // consumer bridges selector values to screen nodes via its config map.
    // Only a bare-identifier RHS is taken (an index literal / expression is a
    // mode toggle, not a named destination).
    foreach (var asgn in root.DescendantNodesAndSelf().OfType<AssignmentExpressionSyntax>())
    {
        if (asgn.Left is not MemberAccessExpressionSyntax lhs
            || !navSelectProps.Contains(lhs.Name.Identifier.Text)
            || asgn.Right is not IdentifierNameSyntax rhs)
        {
            continue;
        }
        var target = rhs.Identifier.Text;
        if (target == className || !seen.Add(target))
        {
            continue;
        }
        triples.Add(new Triple($"{ns}:{className}", "selects_view", $"{ns}:{target}", f, c));
    }
}

// A "screen" base type, syntactically: `UserControl` or any name ending in
// `Form` (`Form` / `XtraForm` / `Office2007Form` / `RibbonForm` / …). Used by
// the screen-type pre-pass. Agnostic — no app-specific class-name prefixes.
static bool IsScreenBase(string baseName) =>
    baseName == "UserControl" || baseName.EndsWith("Form", StringComparison.Ordinal);

// J1 helper — `X == null` / `X is null` -> the tested member `X`; else null.
// The C# analogue of Ruby's `self.X.blank?`/`.nil?` guard.
static string? NullGuardedField(ExpressionSyntax cond)
{
    static string? MemberName(ExpressionSyntax e) => e switch
    {
        MemberAccessExpressionSyntax ma when ma.Expression is ThisExpressionSyntax
            => ma.Name.Identifier.Text,
        IdentifierNameSyntax id => id.Identifier.Text,
        _ => null,
    };
    return cond switch
    {
        // `X == null`
        BinaryExpressionSyntax be when be.OperatorToken.IsKind(SyntaxKind.EqualsEqualsToken)
            && be.Right is LiteralExpressionSyntax { RawKind: (int)SyntaxKind.NullLiteralExpression }
            => MemberName(be.Left),
        // `X is null`
        IsPatternExpressionSyntax ip when ip.Pattern is ConstantPatternSyntax
            { Expression: LiteralExpressionSyntax { RawKind: (int)SyntaxKind.NullLiteralExpression } }
            => MemberName(ip.Expression),
        // `string.IsNullOrEmpty(X)` / `string.IsNullOrWhiteSpace(X)`
        InvocationExpressionSyntax { Expression: MemberAccessExpressionSyntax ma } inv
            when ma.Name.Identifier.Text is "IsNullOrEmpty" or "IsNullOrWhiteSpace"
            && inv.ArgumentList.Arguments.Count == 1
            => MemberName(inv.ArgumentList.Arguments[0].Expression),
        _ => null,
    };
}

// Split a `--flag a,b,c` CLI value into a trimmed, empty-entry-free list.
static List<string> SplitCsv(string s) =>
    s.Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries).ToList();

// The receiver of an invocation, reduced to its last identifier segment, so
// `main.mysql.add_x(...)` matches `--mutator-receivers mysql` (the `.mysql`
// leg), not the whole `main.mysql` chain. `ctx.SaveChanges()` -> "ctx";
// `this.mysql.add_x(...)` -> "mysql" (the `this.mysql` receiver is itself a
// MemberAccess whose Name is "mysql").
static string ReceiverLastSegment(ExpressionSyntax receiver) => receiver switch
{
    MemberAccessExpressionSyntax ma => ma.Name.Identifier.Text,
    IdentifierNameSyntax id => id.Identifier.Text,
    ThisExpressionSyntax => "this",
    _ => receiver.ToString(),
};

// Builds the one `calls`-fact predicate for a harvest run from the
// `--mutator-names`/`--mutator-prefixes`/`--mutator-receivers` configuration.
// A name match is `mutatorNames.Contains(name)` (exact, the original EF-Core
// behaviour) OR a `mutatorPrefixes` prefix match (for naming-convention DALs
// with `add_*`/`del_*`-style names) — either is sufficient. When
// `mutatorReceivers` is non-empty, the match is further restricted to
// invocations whose receiver's last segment (see `ReceiverLastSegment`) is in
// that list; an empty `mutatorReceivers` matches any receiver (the original
// behaviour, e.g. `ctx.SaveChanges()` with no receiver filter at all).
static Func<MemberAccessExpressionSyntax, bool> BuildMutatorPredicate(
    HashSet<string> mutatorNames,
    List<string> mutatorPrefixes,
    List<string> mutatorReceivers) => mac =>
{
    var name = mac.Name.Identifier.Text;
    var nameMatches = mutatorNames.Contains(name)
        || mutatorPrefixes.Any(p => name.StartsWith(p, StringComparison.Ordinal));
    if (!nameMatches)
    {
        return false;
    }
    return mutatorReceivers.Count == 0
        || mutatorReceivers.Contains(ReceiverLastSegment(mac.Expression));
};

// Method access specifier -> "public"|"protected"|"private", the closed
// `has_visibility` vocabulary (`ruff_spo_triplet::Predicate::HasVisibility`,
// mirroring the C++ frontend's `CppAccess`). C# has no explicit keyword for
// the common "no modifier" case, so the two language defaults are applied:
// `private` for class/struct/record members, *implicitly* `public` for
// interface members. `internal`-only members (package-visible, no C#
// equivalent in the closed three-value vocabulary) collapse to "private":
// per the codebook's API-surface signal (fuzzy-recipe-codebook.md §2 —
// "public methods are the adapter surface; private/protected are likely
// internal"), an internal-only member is not on the public adapter surface
// either, so folding it into "private" preserves that signal even though C#
// `internal` and `private` are technically distinct accessibilities.
// `protected internal` / `private protected` collapse to "protected" (the
// first matching, more-restrictive keyword below wins).
static string VisibilityOf(SyntaxTokenList modifiers, bool isInterfaceMember)
{
    if (modifiers.Any(t => t.IsKind(SyntaxKind.PublicKeyword)))
    {
        return "public";
    }
    if (modifiers.Any(t => t.IsKind(SyntaxKind.ProtectedKeyword)))
    {
        return "protected";
    }
    if (modifiers.Any(t => t.IsKind(SyntaxKind.PrivateKeyword)))
    {
        return "private";
    }
    if (modifiers.Any(t => t.IsKind(SyntaxKind.InternalKeyword)))
    {
        return "private";
    }
    return isInterfaceMember ? "public" : "private";
}

// Base/type name as written, generics stripped (`List<Foo>` -> `List`) so the
// object IRI is a stable class reference. Full generic + symbol resolution is
// the SemanticModel upgrade (README.md).
static string BareName(TypeSyntax type)
{
    var s = type.ToString();
    var lt = s.IndexOf('<');
    return lt >= 0 ? s[..lt] : s;
}

// The last identifier segment of a possibly namespace-qualified name:
// `con_x.uc_Screen` -> `uc_Screen`, `DevExpress.XtraBars.RibbonForm` ->
// `RibbonForm`. Used ONLY by the navigation arm (screen-type matching + nav
// targets) so qualified instantiations — `field = new some.ns.Screen(...)`,
// the dominant hosting idiom in namespace-organized WinForms apps — resolve to
// the same screen node as their bare declaration. `inherits_from` /
// `field_type` keep the name as written (their wire contract).
static string LastSegment(string name)
{
    var dot = name.LastIndexOf('.');
    return dot >= 0 ? name[(dot + 1)..] : name;
}

// Mirrors ruff_spo_triplet::Triple field-for-field; the JSON keys are exactly
// s/p/o/f/c so `from_ndjson` deserializes it with no transform.
internal sealed record Triple(
    [property: JsonPropertyName("s")] string S,
    [property: JsonPropertyName("p")] string P,
    [property: JsonPropertyName("o")] string O,
    [property: JsonPropertyName("f")] double F,
    [property: JsonPropertyName("c")] double C);
