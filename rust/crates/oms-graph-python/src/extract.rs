//! AST projection. Child enumeration follows CPython's `ast` field order so
//! refs, edges and nodes come out in the legacy order; every AST variant is
//! matched exhaustively so a new syntax node cannot be skipped silently.

use std::collections::HashSet;

use ruff_python_ast::PythonVersion;
use ruff_python_ast::token as lexemes;
use ruff_python_ast::{
    CmpOp, Comprehension, ExceptHandler, ExceptHandlerExceptHandler, Expr, ExprContext,
    FStringPartRef, InterpolatedStringElement, InterpolatedStringElements, Keyword, MatchCase,
    Parameter, Parameters, Pattern, Stmt, TypeParam, WithItem,
};
use ruff_python_parser::{Mode, ParseOptions, parse_unchecked};
use ruff_source_file::LineIndex;
use ruff_text_size::{Ranged, TextSize};

use crate::LANGUAGE;
use crate::literal::doc_units;
use crate::model::{Edge, Evidence, Metadata, Node, ParseResult, Reference};
use crate::scan::exceeds_cpython_limits;
use crate::text::compact_units;

/// `dir(builtins)` of CPython 3.12 with `site` loaded, as the legacy parser saw it.
const BUILTIN_NAMES: &[&str] = &[
    "ArithmeticError",
    "AssertionError",
    "AttributeError",
    "BaseException",
    "BaseExceptionGroup",
    "BlockingIOError",
    "BrokenPipeError",
    "BufferError",
    "BytesWarning",
    "ChildProcessError",
    "ConnectionAbortedError",
    "ConnectionError",
    "ConnectionRefusedError",
    "ConnectionResetError",
    "DeprecationWarning",
    "EOFError",
    "Ellipsis",
    "EncodingWarning",
    "EnvironmentError",
    "Exception",
    "ExceptionGroup",
    "False",
    "FileExistsError",
    "FileNotFoundError",
    "FloatingPointError",
    "FutureWarning",
    "GeneratorExit",
    "IOError",
    "ImportError",
    "ImportWarning",
    "IndentationError",
    "IndexError",
    "InterruptedError",
    "IsADirectoryError",
    "KeyError",
    "KeyboardInterrupt",
    "LookupError",
    "MemoryError",
    "ModuleNotFoundError",
    "NameError",
    "None",
    "NotADirectoryError",
    "NotImplemented",
    "NotImplementedError",
    "OSError",
    "OverflowError",
    "PendingDeprecationWarning",
    "PermissionError",
    "ProcessLookupError",
    "RecursionError",
    "ReferenceError",
    "ResourceWarning",
    "RuntimeError",
    "RuntimeWarning",
    "StopAsyncIteration",
    "StopIteration",
    "SyntaxError",
    "SyntaxWarning",
    "SystemError",
    "SystemExit",
    "TabError",
    "TimeoutError",
    "True",
    "TypeError",
    "UnboundLocalError",
    "UnicodeDecodeError",
    "UnicodeEncodeError",
    "UnicodeError",
    "UnicodeTranslateError",
    "UnicodeWarning",
    "UserWarning",
    "ValueError",
    "Warning",
    "ZeroDivisionError",
    "__build_class__",
    "__debug__",
    "__doc__",
    "__import__",
    "__loader__",
    "__name__",
    "__package__",
    "__spec__",
    "abs",
    "aiter",
    "all",
    "anext",
    "any",
    "ascii",
    "bin",
    "bool",
    "breakpoint",
    "bytearray",
    "bytes",
    "callable",
    "chr",
    "classmethod",
    "compile",
    "complex",
    "copyright",
    "credits",
    "delattr",
    "dict",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "exit",
    "filter",
    "float",
    "format",
    "frozenset",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "help",
    "hex",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "license",
    "list",
    "locals",
    "map",
    "max",
    "memoryview",
    "min",
    "next",
    "object",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "quit",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "str",
    "sum",
    "super",
    "tuple",
    "type",
    "vars",
    "zip",
];

fn normalized(path: &str) -> String {
    path.replace('\\', "/")
}

/// `node_id` of `model.py`.
fn node_id(kind: &str, path: &str, qualname: &str) -> String {
    let path = normalized(path);
    let path = path.trim_start_matches('/');
    match kind {
        "env" => format!("env:{}", if qualname.is_empty() { path } else { qualname }),
        "file" => format!("file:{path}"),
        "module" => format!("module:{path}"),
        "class" | "function" | "method" | "symbol" => format!("symbol:{path}::{qualname}"),
        _ => format!("{kind}:{path}"),
    }
}

fn env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && (name.contains('_')
            || ["OMS_", "OH_MY_SETTING_", "CODEX_", "CLAUDE_", "GEMINI_"]
                .iter()
                .any(|prefix| name.starts_with(prefix)))
}

/// `a.b.c` for a Name/Attribute chain, empty for anything else.
fn dotted_name(expr: &Expr) -> String {
    let mut parts = Vec::new();
    let mut cursor = expr;
    while let Expr::Attribute(attribute) = cursor {
        parts.push(attribute.attr.as_str());
        cursor = &attribute.value;
    }
    match cursor {
        Expr::Name(name) => {
            parts.push(name.id.as_str());
            parts.reverse();
            parts.join(".")
        }
        _ => String::new(),
    }
}

/// A node of the walk; one variant per `ast` node class that has children or
/// binds names.
#[derive(Clone, Copy)]
enum Item<'a> {
    Stmt(&'a Stmt),
    Expr(&'a Expr),
    Parameters(&'a Parameters),
    Parameter(&'a Parameter),
    Comprehension(&'a Comprehension),
    Handler(&'a ExceptHandlerExceptHandler),
    With(&'a WithItem),
    Keyword(&'a Keyword),
    Case(&'a MatchCase),
    Pattern(&'a Pattern),
    TypeParam(&'a TypeParam),
}

type Visit<'a, 'f> = &'f mut dyn FnMut(Item<'a>);

fn stmts<'a>(body: &'a [Stmt], f: Visit<'a, '_>) {
    body.iter().for_each(|stmt| f(Item::Stmt(stmt)));
}

fn exprs<'a>(list: &'a [Expr], f: Visit<'a, '_>) {
    list.iter().for_each(|expr| f(Item::Expr(expr)));
}

fn opt<'a>(expr: &'a Option<Box<Expr>>, f: Visit<'a, '_>) {
    if let Some(expr) = expr {
        f(Item::Expr(expr));
    }
}

fn type_params<'a>(params: &'a Option<Box<ruff_python_ast::TypeParams>>, f: Visit<'a, '_>) {
    if let Some(params) = params {
        params
            .type_params
            .iter()
            .for_each(|param| f(Item::TypeParam(param)));
    }
}

fn interpolated<'a>(elements: &'a InterpolatedStringElements, f: Visit<'a, '_>) {
    for element in elements {
        match element {
            InterpolatedStringElement::Literal(_) => {}
            InterpolatedStringElement::Interpolation(interpolation) => {
                f(Item::Expr(&interpolation.expression));
                if let Some(spec) = &interpolation.format_spec {
                    interpolated(&spec.elements, f);
                }
            }
        }
    }
}

fn comprehensions<'a>(generators: &'a [Comprehension], f: Visit<'a, '_>) {
    generators
        .iter()
        .for_each(|generator| f(Item::Comprehension(generator)));
}

/// `ast.iter_child_nodes`: direct children in `_fields` order.
fn children<'a>(item: Item<'a>, f: Visit<'a, '_>) {
    match item {
        Item::Stmt(stmt) => match stmt {
            Stmt::FunctionDef(node) => {
                f(Item::Parameters(&node.parameters));
                stmts(&node.body, f);
                node.decorator_list
                    .iter()
                    .for_each(|decorator| f(Item::Expr(&decorator.expression)));
                opt(&node.returns, f);
                type_params(&node.type_params, f);
            }
            Stmt::ClassDef(node) => {
                if let Some(arguments) = &node.arguments {
                    exprs(&arguments.args, f);
                    arguments
                        .keywords
                        .iter()
                        .for_each(|keyword| f(Item::Keyword(keyword)));
                }
                stmts(&node.body, f);
                node.decorator_list
                    .iter()
                    .for_each(|decorator| f(Item::Expr(&decorator.expression)));
                type_params(&node.type_params, f);
            }
            Stmt::Return(node) => opt(&node.value, f),
            Stmt::Delete(node) => exprs(&node.targets, f),
            Stmt::TypeAlias(node) => {
                f(Item::Expr(&node.name));
                type_params(&node.type_params, f);
                f(Item::Expr(&node.value));
            }
            Stmt::Assign(node) => {
                exprs(&node.targets, f);
                f(Item::Expr(&node.value));
            }
            Stmt::AugAssign(node) => {
                f(Item::Expr(&node.target));
                f(Item::Expr(&node.value));
            }
            Stmt::AnnAssign(node) => {
                f(Item::Expr(&node.target));
                f(Item::Expr(&node.annotation));
                opt(&node.value, f);
            }
            Stmt::For(node) => {
                f(Item::Expr(&node.target));
                f(Item::Expr(&node.iter));
                stmts(&node.body, f);
                stmts(&node.orelse, f);
            }
            Stmt::While(node) => {
                f(Item::Expr(&node.test));
                stmts(&node.body, f);
                stmts(&node.orelse, f);
            }
            // `elif` is a nested `If` in `orelse` for CPython; the flat clause
            // list yields the same sequence of tests and bodies.
            Stmt::If(node) => {
                f(Item::Expr(&node.test));
                stmts(&node.body, f);
                for clause in &node.elif_else_clauses {
                    if let Some(test) = &clause.test {
                        f(Item::Expr(test));
                    }
                    stmts(&clause.body, f);
                }
            }
            Stmt::With(node) => {
                node.items.iter().for_each(|item| f(Item::With(item)));
                stmts(&node.body, f);
            }
            Stmt::Match(node) => {
                f(Item::Expr(&node.subject));
                node.cases.iter().for_each(|case| f(Item::Case(case)));
            }
            Stmt::Raise(node) => {
                opt(&node.exc, f);
                opt(&node.cause, f);
            }
            Stmt::Try(node) => {
                stmts(&node.body, f);
                for ExceptHandler::ExceptHandler(handler) in &node.handlers {
                    f(Item::Handler(handler));
                }
                stmts(&node.orelse, f);
                stmts(&node.finalbody, f);
            }
            Stmt::Assert(node) => {
                f(Item::Expr(&node.test));
                opt(&node.msg, f);
            }
            Stmt::Expr(node) => f(Item::Expr(&node.value)),
            Stmt::Import(_)
            | Stmt::ImportFrom(_)
            | Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Pass(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            // IPython escapes are produced only in notebook mode, never here.
            | Stmt::IpyEscapeCommand(_) => {}
        },
        Item::Expr(expr) => match expr {
            Expr::BoolOp(node) => exprs(&node.values, f),
            Expr::Named(node) => {
                f(Item::Expr(&node.target));
                f(Item::Expr(&node.value));
            }
            Expr::BinOp(node) => {
                f(Item::Expr(&node.left));
                f(Item::Expr(&node.right));
            }
            Expr::UnaryOp(node) => f(Item::Expr(&node.operand)),
            Expr::Lambda(node) => {
                if let Some(parameters) = &node.parameters {
                    f(Item::Parameters(parameters));
                }
                f(Item::Expr(&node.body));
            }
            Expr::If(node) => {
                f(Item::Expr(&node.test));
                f(Item::Expr(&node.body));
                f(Item::Expr(&node.orelse));
            }
            // `Dict(keys, values)`: every key (without `**` spreads), then every value.
            Expr::Dict(node) => {
                node.items
                    .iter()
                    .filter_map(|item| item.key.as_ref())
                    .for_each(|key| f(Item::Expr(key)));
                node.items
                    .iter()
                    .for_each(|item| f(Item::Expr(&item.value)));
            }
            Expr::Set(node) => exprs(&node.elts, f),
            Expr::ListComp(node) => {
                f(Item::Expr(&node.elt));
                comprehensions(&node.generators, f);
            }
            Expr::SetComp(node) => {
                f(Item::Expr(&node.elt));
                comprehensions(&node.generators, f);
            }
            Expr::DictComp(node) => {
                opt(&node.key, f);
                f(Item::Expr(&node.value));
                comprehensions(&node.generators, f);
            }
            Expr::Generator(node) => {
                f(Item::Expr(&node.elt));
                comprehensions(&node.generators, f);
            }
            Expr::Await(node) => f(Item::Expr(&node.value)),
            Expr::Yield(node) => opt(&node.value, f),
            Expr::YieldFrom(node) => f(Item::Expr(&node.value)),
            Expr::Compare(node) => exprs(&node.operands, f),
            Expr::Call(node) => {
                f(Item::Expr(&node.func));
                exprs(&node.arguments.args, f);
                node.arguments
                    .keywords
                    .iter()
                    .for_each(|keyword| f(Item::Keyword(keyword)));
            }
            Expr::FString(node) => {
                for part in &node.value {
                    if let FStringPartRef::FString(fstring) = part {
                        interpolated(&fstring.elements, f);
                    }
                }
            }
            Expr::TString(node) => {
                for template in &node.value {
                    interpolated(&template.elements, f);
                }
            }
            Expr::Attribute(node) => f(Item::Expr(&node.value)),
            Expr::Subscript(node) => {
                f(Item::Expr(&node.value));
                f(Item::Expr(&node.slice));
            }
            Expr::Starred(node) => f(Item::Expr(&node.value)),
            Expr::List(node) => exprs(&node.elts, f),
            Expr::Tuple(node) => exprs(&node.elts, f),
            Expr::Slice(node) => {
                opt(&node.lower, f);
                opt(&node.upper, f);
                opt(&node.step, f);
            }
            Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::EllipsisLiteral(_)
            | Expr::Name(_)
            | Expr::IpyEscapeCommand(_) => {}
        },
        // `arguments`: posonlyargs, args, vararg, kwonlyargs, kw_defaults, kwarg, defaults.
        Item::Parameters(parameters) => {
            for positional in parameters.posonlyargs.iter().chain(&parameters.args) {
                f(Item::Parameter(&positional.parameter));
            }
            if let Some(vararg) = &parameters.vararg {
                f(Item::Parameter(vararg));
            }
            for keyword_only in &parameters.kwonlyargs {
                f(Item::Parameter(&keyword_only.parameter));
            }
            for keyword_only in &parameters.kwonlyargs {
                opt(&keyword_only.default, f);
            }
            if let Some(kwarg) = &parameters.kwarg {
                f(Item::Parameter(kwarg));
            }
            for positional in parameters.posonlyargs.iter().chain(&parameters.args) {
                opt(&positional.default, f);
            }
        }
        Item::Parameter(parameter) => opt(&parameter.annotation, f),
        Item::Comprehension(comprehension) => {
            f(Item::Expr(&comprehension.target));
            f(Item::Expr(&comprehension.iter));
            exprs(&comprehension.ifs, f);
        }
        Item::Handler(handler) => {
            opt(&handler.type_, f);
            stmts(&handler.body, f);
        }
        Item::With(item) => {
            f(Item::Expr(&item.context_expr));
            opt(&item.optional_vars, f);
        }
        Item::Keyword(keyword) => f(Item::Expr(&keyword.value)),
        Item::Case(case) => {
            f(Item::Pattern(&case.pattern));
            opt(&case.guard, f);
            stmts(&case.body, f);
        }
        Item::Pattern(pattern) => match pattern {
            Pattern::MatchValue(node) => f(Item::Expr(&node.value)),
            Pattern::MatchSequence(node) => {
                node.patterns
                    .iter()
                    .for_each(|inner| f(Item::Pattern(inner)));
            }
            Pattern::MatchMapping(node) => {
                node.keys.iter().for_each(|key| f(Item::Expr(key)));
                node.patterns
                    .iter()
                    .for_each(|inner| f(Item::Pattern(inner)));
            }
            Pattern::MatchClass(node) => {
                f(Item::Expr(&node.cls));
                node.arguments
                    .patterns
                    .iter()
                    .for_each(|inner| f(Item::Pattern(inner)));
                node.arguments
                    .keywords
                    .iter()
                    .for_each(|keyword| f(Item::Pattern(&keyword.pattern)));
            }
            Pattern::MatchAs(node) => {
                if let Some(inner) = &node.pattern {
                    f(Item::Pattern(inner));
                }
            }
            Pattern::MatchOr(node) => {
                node.patterns
                    .iter()
                    .for_each(|inner| f(Item::Pattern(inner)));
            }
            Pattern::MatchSingleton(_) | Pattern::MatchStar(_) => {}
        },
        Item::TypeParam(param) => match param {
            TypeParam::TypeVar(node) => {
                opt(&node.bound, f);
                opt(&node.default, f);
            }
            TypeParam::TypeVarTuple(node) => opt(&node.default, f),
            TypeParam::ParamSpec(node) => opt(&node.default, f),
        },
    }
}

/// `ast.walk`: every node of the subtree, the root included.
fn walk_all<'a>(item: Item<'a>, f: Visit<'a, '_>) {
    f(item);
    children(item, &mut |child| walk_all(child, f));
}

fn target_names(target: &Expr, into: &mut HashSet<String>) {
    match target {
        Expr::Name(name) => {
            into.insert(name.id.to_string());
        }
        Expr::Tuple(node) => node
            .elts
            .iter()
            .for_each(|element| target_names(element, into)),
        Expr::List(node) => node
            .elts
            .iter()
            .for_each(|element| target_names(element, into)),
        Expr::Starred(node) => target_names(&node.value, into),
        _ => {}
    }
}

/// Every name a subtree binds as a variable (imports excluded). The
/// over-approximation is deliberate: a nested scope's binding also counts for
/// the enclosing function, so a call through such a name yields no edge rather
/// than a repo-wide guess by name.
fn bound_names(root: Item<'_>) -> HashSet<String> {
    let mut names = HashSet::new();
    walk_all(root, &mut |item| match item {
        Item::Stmt(Stmt::Assign(node)) => {
            node.targets
                .iter()
                .for_each(|target| target_names(target, &mut names));
        }
        Item::Stmt(Stmt::AnnAssign(node)) => target_names(&node.target, &mut names),
        Item::Stmt(Stmt::AugAssign(node)) => target_names(&node.target, &mut names),
        Item::Stmt(Stmt::For(node)) => target_names(&node.target, &mut names),
        Item::Comprehension(node) => target_names(&node.target, &mut names),
        Item::Expr(Expr::Named(node)) => target_names(&node.target, &mut names),
        Item::With(node) => {
            if let Some(vars) = &node.optional_vars {
                target_names(vars, &mut names);
            }
        }
        Item::Handler(node) => {
            if let Some(name) = &node.name {
                names.insert(name.as_str().to_owned());
            }
        }
        Item::Parameters(parameters) => {
            let positional = parameters.posonlyargs.iter().chain(&parameters.args);
            for argument in positional.chain(&parameters.kwonlyargs) {
                names.insert(argument.parameter.name.as_str().to_owned());
            }
            for argument in [&parameters.vararg, &parameters.kwarg]
                .into_iter()
                .flatten()
            {
                names.insert(argument.name.as_str().to_owned());
            }
        }
        _ => {}
    });
    names
}

fn python_signature_function(node: &ruff_python_ast::StmtFunctionDef, qualname: &str) -> String {
    let parameters = &node.parameters;
    let default_mark = |has_default: bool| if has_default { "=..." } else { "" };
    let mut parts: Vec<String> = Vec::new();
    for item in &parameters.posonlyargs {
        parts.push(format!(
            "{}{}",
            item.parameter.name.as_str(),
            default_mark(item.default.is_some())
        ));
    }
    if !parameters.posonlyargs.is_empty() {
        parts.push("/".to_owned());
    }
    for item in &parameters.args {
        parts.push(format!(
            "{}{}",
            item.parameter.name.as_str(),
            default_mark(item.default.is_some())
        ));
    }
    if let Some(vararg) = &parameters.vararg {
        parts.push(format!("*{}", vararg.name.as_str()));
    } else if !parameters.kwonlyargs.is_empty() {
        parts.push("*".to_owned());
    }
    for item in &parameters.kwonlyargs {
        parts.push(format!(
            "{}{}",
            item.parameter.name.as_str(),
            default_mark(item.default.is_some())
        ));
    }
    if let Some(kwarg) = &parameters.kwarg {
        parts.push(format!("**{}", kwarg.name.as_str()));
    }
    let prefix = if node.is_async { "async def" } else { "def" };
    format!("{prefix} {qualname}({})", parts.join(", "))
}

/// First-statement string literal as code point units, `ast.get_docstring`'s
/// rule; escaped lone surrogates stay distinct from a literal U+FFFD (see `literal`).
fn docstring(body: &[Stmt], source: &str) -> Option<Vec<u32>> {
    match body.first()? {
        Stmt::Expr(statement) => match &*statement.value {
            Expr::StringLiteral(literal) => Some(doc_units(literal, source)),
            _ => None,
        },
        _ => None,
    }
}

fn summary_of(body: &[Stmt], source: &str) -> Option<String> {
    compact_units(&docstring(body, source)?, 240)
}

/// Grammar baseline: CPython 3.12. Newer syntax Ruff also understands is
/// accepted (its version-gated findings are not hard errors); see the crate docs.
fn parse_options() -> ParseOptions {
    ParseOptions::from(Mode::Module).with_target_version(PythonVersion::PY312)
}

pub(crate) fn project(path: &str, text: &str, digest: &str) -> ParseResult {
    let file_id = node_id("file", path, "");
    let module_id = node_id("module", path, "");
    let mut result = ParseResult::default();
    let module_name = path.rsplit('/').next().unwrap_or(path);
    result.nodes.push(Node {
        id: module_id.clone(),
        kind: "module".to_owned(),
        name: module_name.to_owned(),
        path: Some(normalized(path)),
        language: LANGUAGE.to_owned(),
        source_digest: digest.to_owned(),
        summary: None,
        crux: None,
        metadata: Metadata::default(),
    });
    result.edges.push(Edge {
        source: file_id.clone(),
        target: module_id,
        relation: "contains".to_owned(),
        confidence: "EXTRACTED".to_owned(),
        evidence: Evidence {
            path: normalized(path),
            source_digest: digest.to_owned(),
            line: None,
        },
    });
    // CPython rejects NUL bytes before tokenizing and, for `str` input such as
    // the builder's lossily decoded text, a leading BOM as a stray character.
    if text.contains('\0') || text.starts_with('\u{feff}') || exceeds_cpython_limits(text) {
        return result;
    }
    let parsed = parse_unchecked(text, parse_options());
    if parsed.has_invalid_syntax() {
        return result;
    }
    let Some(parsed) = parsed.try_into_module() else {
        return result;
    };
    let module = parsed.syntax();
    result.nodes[0].summary = summary_of(&module.body, text);

    let mut import_bindings = HashSet::new();
    for statement in &module.body {
        walk_all(Item::Stmt(statement), &mut |item| match item {
            Item::Stmt(Stmt::Import(node)) => {
                for alias in &node.names {
                    import_bindings.insert(binding_of_import(alias));
                }
            }
            Item::Stmt(Stmt::ImportFrom(node)) => {
                for alias in &node.names {
                    import_bindings.insert(
                        alias
                            .asname
                            .as_ref()
                            .unwrap_or(&alias.name)
                            .as_str()
                            .to_owned(),
                    );
                }
            }
            _ => {}
        });
    }
    // Module-level variables (not definitions, not imports): a call through
    // one of them is an object of unknown type, never a repo symbol by name.
    let mut module_variables = HashSet::new();
    for statement in &module.body {
        if !matches!(statement, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
            module_variables.extend(bound_names(Item::Stmt(statement)));
        }
    }
    module_variables.retain(|name| !import_bindings.contains(name));

    let mut extractor = Extractor {
        path,
        digest,
        file_id,
        text,
        text_lines: LineIndex::from_source_text(text),
        tokens: parsed.tokens(),
        import_bindings,
        module_variables,
        class_name: String::new(),
        symbol: String::new(),
        methods_stack: Vec::new(),
        locals_stack: Vec::new(),
        result,
    };
    for statement in &module.body {
        extractor.visit(Item::Stmt(statement));
    }
    extractor.result
}

fn binding_of_import(alias: &ruff_python_ast::Alias) -> String {
    match &alias.asname {
        Some(asname) => asname.as_str().to_owned(),
        None => alias
            .name
            .as_str()
            .split('.')
            .next()
            .unwrap_or_default()
            .to_owned(),
    }
}

struct Extractor<'a> {
    path: &'a str,
    text: &'a str,
    digest: &'a str,
    file_id: String,
    text_lines: LineIndex,
    tokens: &'a lexemes::Tokens,
    import_bindings: HashSet<String>,
    module_variables: HashSet<String>,
    class_name: String,
    symbol: String,
    /// Method names of the enclosing class bodies, so a `self.x()` call
    /// resolves to a sibling method as a fact rather than by name.
    methods_stack: Vec<HashSet<String>>,
    /// Names bound by the enclosing function bodies; a call through one is a variable.
    locals_stack: Vec<HashSet<String>>,
    result: ParseResult,
}

impl<'a> Extractor<'a> {
    fn line(&self, offset: TextSize) -> u64 {
        self.text_lines.line_index(offset).get() as u64
    }

    /// Line of the last character of a node, CPython's `end_lineno`.
    fn end_line(&self, end: TextSize) -> u64 {
        self.line(end.checked_sub(TextSize::new(1)).unwrap_or(end))
    }

    fn source(&self) -> String {
        if self.symbol.is_empty() {
            self.file_id.clone()
        } else {
            self.symbol.clone()
        }
    }

    fn is_variable(&self, name: &str) -> bool {
        if self.import_bindings.contains(name) {
            return false;
        }
        self.module_variables.contains(name)
            || self.locals_stack.iter().any(|frame| frame.contains(name))
    }

    fn edge(&self, source: String, target: String, relation: &str, line: Option<u64>) -> Edge {
        Edge {
            source,
            target,
            relation: relation.to_owned(),
            confidence: "EXTRACTED".to_owned(),
            evidence: Evidence {
                path: normalized(self.path),
                source_digest: self.digest.to_owned(),
                line,
            },
        }
    }

    /// Line of the `def`/`async`/`class` keyword: decorators precede it in the
    /// Ruff range but not in CPython's `lineno`.
    fn keyword_line(&self, start: TextSize, decorator_end: Option<TextSize>) -> u64 {
        let Some(end) = decorator_end else {
            return self.line(start);
        };
        let keyword = self.tokens.after(end).iter().find(|token| {
            matches!(
                token.kind(),
                lexemes::TokenKind::Def | lexemes::TokenKind::Async | lexemes::TokenKind::Class
            )
        });
        self.line(keyword.map_or(start, |token| token.start()))
    }

    #[allow(clippy::too_many_arguments)]
    fn add_symbol(
        &mut self,
        kind: &str,
        name: &str,
        signature: String,
        line: u64,
        end_line: u64,
        body: &[Stmt],
    ) -> String {
        let qualname = if !self.class_name.is_empty() && kind == "method" {
            format!("{}.{name}", self.class_name)
        } else {
            name.to_owned()
        };
        let ident = node_id(kind, self.path, &qualname);
        self.result.nodes.push(Node {
            id: ident.clone(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            path: Some(normalized(self.path)),
            language: LANGUAGE.to_owned(),
            source_digest: self.digest.to_owned(),
            summary: summary_of(body, self.text),
            crux: None,
            metadata: Metadata {
                signature: Some(signature),
                end_line: Some(end_line),
                qualname: Some(qualname.clone()),
                line: Some(line),
            },
        });
        let parent = if kind == "method" {
            node_id("class", self.path, &self.class_name)
        } else {
            self.file_id.clone()
        };
        let edge = self.edge(parent, ident.clone(), "contains", Some(line));
        self.result.edges.push(edge);
        ident
    }

    fn visit(&mut self, item: Item<'a>) {
        match item {
            Item::Stmt(Stmt::ClassDef(node)) => self.visit_class(node),
            Item::Stmt(stmt @ Stmt::FunctionDef(node)) => self.visit_function(stmt, node),
            Item::Stmt(Stmt::Import(node)) => {
                let line = self.line(node.start());
                for alias in &node.names {
                    self.result.refs.push(Reference {
                        from: self.source(),
                        relation: "imports".to_owned(),
                        kind: "module".to_owned(),
                        value: alias.name.as_str().to_owned(),
                        line,
                        binding: Some(binding_of_import(alias)),
                        imported: None,
                        scoped: None,
                        method_call: None,
                    });
                }
            }
            Item::Stmt(Stmt::ImportFrom(node)) => {
                let line = self.line(node.start());
                let module = format!(
                    "{}{}",
                    ".".repeat(node.level as usize),
                    node.module.as_ref().map_or("", |module| module.as_str())
                );
                for alias in &node.names {
                    let binding = alias.asname.as_ref().unwrap_or(&alias.name);
                    self.result.refs.push(Reference {
                        from: self.source(),
                        relation: "imports".to_owned(),
                        kind: "module".to_owned(),
                        value: module.clone(),
                        line,
                        binding: Some(binding.as_str().to_owned()),
                        imported: Some(alias.name.as_str().to_owned()),
                        scoped: None,
                        method_call: None,
                    });
                }
            }
            Item::Expr(Expr::Subscript(node)) => {
                if dotted_name(&node.value) == "os.environ" && node.ctx == ExprContext::Load {
                    self.env_read(&node.slice, self.line(node.start()));
                }
                self.generic(item);
            }
            Item::Expr(Expr::Compare(node)) => {
                let line = self.line(node.start());
                for (index, op) in node.ops.iter().enumerate() {
                    if matches!(op, CmpOp::In | CmpOp::NotIn)
                        && dotted_name(&node.operands[index + 1]) == "os.environ"
                    {
                        self.env_read(&node.operands[index], line);
                    }
                }
                self.generic(item);
            }
            Item::Expr(Expr::Call(node)) => {
                self.visit_call(node);
                self.generic(item);
            }
            _ => self.generic(item),
        }
    }

    fn generic(&mut self, item: Item<'a>) {
        children(item, &mut |child| self.visit(child));
    }

    fn visit_class(&mut self, node: &'a ruff_python_ast::StmtClassDef) {
        let old_class = std::mem::replace(&mut self.class_name, node.name.as_str().to_owned());
        let old_symbol = self.symbol.clone();
        let line = self.keyword_line(
            node.start(),
            node.decorator_list.last().map(|decorator| decorator.end()),
        );
        let bases: Vec<&Expr> = node
            .arguments
            .as_ref()
            .map_or(Vec::new(), |arguments| arguments.args.iter().collect());
        let base_names: Vec<String> = bases
            .iter()
            .map(|base| dotted_name(base))
            .filter(|name| !name.is_empty())
            .collect();
        let signature = format!(
            "class {}{}",
            node.name.as_str(),
            if base_names.is_empty() {
                String::new()
            } else {
                format!("({})", base_names.join(", "))
            }
        );
        self.symbol = self.add_symbol(
            "class",
            node.name.as_str(),
            signature,
            line,
            self.end_line(node.end()),
            &node.body,
        );
        for decorator in &node.decorator_list {
            self.visit(Item::Expr(&decorator.expression));
        }
        self.methods_stack.push(
            node.body
                .iter()
                .filter_map(|child| match child {
                    Stmt::FunctionDef(function) => Some(function.name.as_str().to_owned()),
                    _ => None,
                })
                .collect(),
        );
        for name in base_names {
            if !self.is_variable(name.split('.').next().unwrap_or_default()) {
                self.result.refs.push(Reference {
                    from: self.symbol.clone(),
                    relation: "depends_on".to_owned(),
                    kind: "name".to_owned(),
                    value: name,
                    line,
                    binding: None,
                    imported: None,
                    scoped: None,
                    method_call: None,
                });
            }
        }
        for child in &node.body {
            self.visit(Item::Stmt(child));
        }
        self.methods_stack.pop();
        self.class_name = old_class;
        self.symbol = old_symbol;
    }

    fn visit_function(&mut self, stmt: &'a Stmt, node: &'a ruff_python_ast::StmtFunctionDef) {
        let kind = if self.class_name.is_empty() {
            "function"
        } else {
            "method"
        };
        let old = self.symbol.clone();
        let line = self.keyword_line(
            node.start(),
            node.decorator_list.last().map(|decorator| decorator.end()),
        );
        let qualname = if kind == "method" {
            format!("{}.{}", self.class_name, node.name.as_str())
        } else {
            node.name.as_str().to_owned()
        };
        let signature = python_signature_function(node, &qualname);
        self.symbol = self.add_symbol(
            kind,
            node.name.as_str(),
            signature,
            line,
            self.end_line(node.end()),
            &node.body,
        );
        for decorator in &node.decorator_list {
            self.visit(Item::Expr(&decorator.expression));
        }
        let parameters = &node.parameters;
        for default in parameters
            .posonlyargs
            .iter()
            .chain(&parameters.args)
            .filter_map(|argument| argument.default.as_deref())
        {
            self.visit(Item::Expr(default));
        }
        for default in parameters
            .kwonlyargs
            .iter()
            .filter_map(|argument| argument.default.as_deref())
        {
            self.visit(Item::Expr(default));
        }
        self.locals_stack.push(bound_names(Item::Stmt(stmt)));
        for child in &node.body {
            self.visit(Item::Stmt(child));
        }
        self.locals_stack.pop();
        self.symbol = old;
    }

    fn env_read(&mut self, value: &Expr, line: u64) {
        let Expr::StringLiteral(literal) = value else {
            return;
        };
        let name = literal.value.to_str();
        if !env_name(name) {
            return;
        }
        self.result.nodes.push(Node {
            id: node_id("env", self.path, name),
            kind: "env".to_owned(),
            name: name.to_owned(),
            path: None,
            language: "env".to_owned(),
            source_digest: self.digest.to_owned(),
            summary: None,
            crux: None,
            metadata: Metadata::default(),
        });
        let edge = self.edge(
            self.source(),
            node_id("env", name, ""),
            "reads_env",
            Some(line),
        );
        self.result.edges.push(edge);
    }

    fn visit_call(&mut self, node: &ruff_python_ast::ExprCall) {
        let line = self.line(node.start());
        let value = dotted_name(&node.func);
        if matches!(
            value.as_str(),
            "os.environ.get" | "os.getenv" | "environ.get"
        ) && let Some(first) = node.arguments.args.first()
        {
            self.env_read(first, line);
        }
        let (receiver, member) = match value.split_once('.') {
            Some((receiver, member)) => (receiver, member),
            None => (value.as_str(), ""),
        };
        if matches!(receiver, "self" | "cls")
            && !self.class_name.is_empty()
            && !member.is_empty()
            && !member.contains('.')
        {
            if self
                .methods_stack
                .last()
                .is_some_and(|methods| methods.contains(member))
            {
                let target = node_id(
                    "method",
                    self.path,
                    &format!("{}.{member}", self.class_name),
                );
                let edge = self.edge(self.source(), target, "calls", Some(line));
                self.result.edges.push(edge);
            } else {
                // Inherited or mixed-in: resolve by name across the repo, where
                // a method is the expected kind of target.
                self.result.refs.push(Reference {
                    from: self.source(),
                    relation: "calls".to_owned(),
                    kind: "name".to_owned(),
                    value: member.to_owned(),
                    line,
                    binding: None,
                    imported: None,
                    scoped: None,
                    method_call: Some(true),
                });
            }
        } else if !value.is_empty() && !self.is_variable(receiver) {
            // `scoped`: resolve only through this file's import bindings or its
            // own definitions: an attribute call on a name that is not an
            // import, or a builtin's name, is never matched to a repo symbol
            // just because the names agree.
            let scoped = (!member.is_empty() && !self.import_bindings.contains(receiver))
                || BUILTIN_NAMES.contains(&receiver);
            self.result.refs.push(Reference {
                from: self.source(),
                relation: "calls".to_owned(),
                kind: "name".to_owned(),
                value: value.clone(),
                line,
                binding: None,
                imported: None,
                scoped: scoped.then_some(true),
                method_call: None,
            });
        }
    }
}
