//! Conservative AST binding IR. Never scans comments or string contents for names.
use code_intel_core::{
    IndexedFile, file_id,
    language::Language,
    semantic::{
        AliasOf, Binding, BuiltinSymbolKey, ClassInfo, CommonJs, CommonJsExport, Export, Location,
        MEMBER_ONLY_EXPORT_REASONS, MemberContext, MemberWrite, Module, NameAt, ObjectProperty,
        ObjectTable, Relationship, Symbol, Usage,
    },
};
use tree_sitter::Node;

#[allow(clippy::too_many_lines)] // The ordered extraction passes over one file.
pub fn extract(root: Node<'_>, source: &str, file: &IndexedFile) -> Module {
    let mut module = Module {
        complete: !root.has_error(),
        ..Module::default()
    };
    if !module.complete {
        return module;
    }
    // Bound our recursive visitors even for adversarial but syntactically valid files.
    let mut pending = vec![(root, 0_u16)];
    while let Some((node, depth)) = pending.pop() {
        if depth > 256 {
            module.complete = false;
            return module;
        }
        let mut cursor = node.walk();
        pending.extend(
            node.named_children(&mut cursor)
                .map(|child| (child, depth + 1)),
        );
    }
    for fact in &file.facts {
        if matches!(fact.kind.as_str(), "file" | "import" | "export") {
            continue;
        }
        let node = root.descendant_for_byte_range(fact.start_byte, fact.end_byte);
        let parent = file.facts.iter().find(|parent| {
            parent.kind == "class"
                && parent.start_byte < fact.start_byte
                && parent.end_byte >= fact.end_byte
        });
        let value = node.and_then(|node| node.child_by_field_name("value"));
        let signature_only = fact.kind == "method"
            && node.is_some_and(|node| {
                matches!(
                    node.kind(),
                    "method_signature" | "abstract_method_signature"
                )
            });
        module.symbols.push(Symbol {
            id: fact.id.clone(),
            name: fact.name.clone(),
            kind: fact.kind.clone(),
            parent: parent.map(|parent| parent.id.clone()),
            static_member: node.is_some_and(|node| token(node, "static"))
                && !fact
                    .name
                    .rsplit('.')
                    .next()
                    .is_some_and(|name| name.starts_with('#')),
            callable: matches!(fact.kind.as_str(), "function" | "method")
                && node.is_none_or(|node| !token(node, "get") && !token(node, "set"))
                || value.is_some_and(|value| {
                    matches!(
                        value.kind(),
                        "arrow_function" | "function_expression" | "generator_function"
                    )
                }),
            signature_only,
            form: node.and_then(|node| form(node, fact.start_byte, fact.end_byte)),
        });
    }
    bindings(root, source, file, &mut module);
    module.esm_syntax = {
        let mut cursor = root.walk();
        root.named_children(&mut cursor)
            .any(|node| matches!(node.kind(), "import_statement" | "export_statement"))
    };
    let mut writes = Vec::new();
    let mut members = Vec::new();
    mutations(root, source, &mut writes, &mut members);
    // `require` is the CommonJS loader only in a JavaScript file without ES module
    // syntax, where no declaration, parameter or assignment names `require`.
    let commonjs_file = Language::of(&file.path).is_some_and(Language::is_javascript)
        && std::path::Path::new(&file.path)
            .extension()
            .is_none_or(|extension| extension != "mjs")
        && !module.esm_syntax
        && !module
            .bindings
            .iter()
            .any(|binding| binding.name == "require" && !binding.commonjs)
        && !writes.iter().any(|(name, _)| name == "require");
    if !commonjs_file {
        for binding in module
            .bindings
            .iter_mut()
            .filter(|binding| binding.commonjs)
        {
            binding.commonjs = false;
            binding.module = None;
            binding.imported = String::new();
        }
    }
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        if node.kind() == "import_statement" {
            imports(node, root, source, file, &mut module);
        }
        if node.kind() == "export_statement" {
            exports(node, source, file, &mut module);
        }
    }
    uses(root, source, file, &mut module);
    // Rebinding a name anywhere in this file invalidates the enclosing binding. This
    // deliberately over-invalidates shadowed writes rather than claiming a mutable value
    // is a known callee. A write through a member (`x.m = v`, `x[k] = v`) does not rebind
    // `x`: it invalidates only member uses of `x` (`x.m()`), and the identifiers inside a
    // computed key are reads.
    for binding in &mut module.bindings {
        let visible = |offset: usize| binding.scope_start <= offset && offset < binding.scope_end;
        binding.mutated = writes
            .iter()
            .any(|(name, offset)| name == &binding.name && visible(*offset));
        let mut mutated: Vec<String> = members
            .iter()
            .filter(|write| {
                write.receiver.as_deref() == Some(binding.name.as_str()) && visible(write.offset)
            })
            .map(|write| write.property.clone().unwrap_or_else(|| "*".into()))
            .collect();
        mutated.sort();
        mutated.dedup();
        binding.mutated_members = mutated;
    }
    classes(root, source, file, &members, &writes, &mut module);
    module.member_writes = members;
    prototypes(root, source, &mut module);
    if commonjs_file {
        commonjs(root, source, file, &writes, &mut module);
    }
    let javascript = Language::of(&file.path).is_some_and(Language::is_javascript);
    origins(&mut module, root, source, javascript);
    receiver_escapes(&mut module, root, source);
    if javascript {
        objects(root, source, file, &mut module);
        prototype_tables(root, source, file, &mut module);
    }
    module
}

/// The expression form of a fact that is not a declaration statement.
fn form(node: Node<'_>, start: usize, end: usize) -> Option<String> {
    if node.start_byte() != start || node.end_byte() != end {
        return None;
    }
    let value = match node.kind() {
        "assignment_expression" => node.child_by_field_name("right")?,
        "pair" => node.child_by_field_name("value")?,
        "method_definition"
            if node
                .parent()
                .is_some_and(|parent| parent.kind() == "object") =>
        {
            return Some("object_method".into());
        }
        "class" => return Some("class_expression".into()),
        _ => return None,
    };
    match value.kind() {
        "arrow_function" => Some("arrow_function".into()),
        "function_expression" | "function" | "generator_function" => {
            Some("function_expression".into())
        }
        _ => None,
    }
}

/// The callable or class fact for an inline value at `site`: a function value
/// is located at the slot (`site`), a class expression at itself.
fn inline_fact(file: &IndexedFile, site: Node<'_>, value: Node<'_>) -> Option<String> {
    let at = |node: Node<'_>, kinds: &[&str]| {
        file.facts
            .iter()
            .find(|fact| {
                kinds.contains(&fact.kind.as_str())
                    && fact.start_byte == node.start_byte()
                    && fact.end_byte == node.end_byte()
            })
            .map(|fact| fact.id.clone())
    };
    match value.kind() {
        "function_expression" | "function" | "arrow_function" | "generator_function" => {
            at(site, &["function"])
        }
        "class" => at(value, &["class"]),
        _ if site.kind() == "method_definition" => at(site, &["method"]),
        _ => None,
    }
}

/// `require("literal")` or `require("literal").member`.
fn require_call(value: Node<'_>, source: &str) -> Option<(String, Option<String>)> {
    if value.kind() == "member_expression" {
        let property = value
            .child_by_field_name("property")
            .filter(|property| property.kind() == "property_identifier")?;
        let (specifier, None) = require_call(value.child_by_field_name("object")?, source)? else {
            return None;
        };
        return Some((specifier, Some(text(property, source).into())));
    }
    if value.kind() != "call_expression" {
        return None;
    }
    let function = value.child_by_field_name("function")?;
    let arguments = value.child_by_field_name("arguments")?;
    if function.kind() != "identifier"
        || text(function, source) != "require"
        || arguments.kind() != "arguments"
        || arguments.named_child_count() != 1
    {
        return None;
    }
    let argument = arguments.named_child(0)?;
    // A plain string literal; escapes are not interpreted, so they are not resolved.
    (argument.kind() == "string" && !text(argument, source).contains('\\'))
        .then(|| (literal(argument, source), None))
}

/// Mark the bindings a `require` declarator just declared (from `before`):
/// `x = require(…)` binds the whole `module.exports` (or `.member`);
/// `{ a, b: c } = require(…)` binds properties. Defaults, rest elements,
/// computed keys and nested patterns stay plain locals.
fn require_bindings(
    name: Node<'_>,
    specifier: &str,
    member: Option<String>,
    before: usize,
    source: &str,
    module: &mut Module,
) {
    let mut keys = std::collections::HashMap::new();
    match name.kind() {
        "identifier" => {
            keys.insert(name.start_byte(), member.unwrap_or_else(|| "*".into()));
        }
        "object_pattern" if member.is_none() => {
            let mut cursor = name.walk();
            for property in name.named_children(&mut cursor) {
                match property.kind() {
                    "shorthand_property_identifier_pattern" => {
                        keys.insert(property.start_byte(), text(property, source).into());
                    }
                    "pair_pattern" => {
                        let key = property.child_by_field_name("key");
                        let value = property.child_by_field_name("value");
                        if let (Some(key), Some(value)) = (key, value)
                            && value.kind() == "identifier"
                        {
                            match key.kind() {
                                "property_identifier" => {
                                    keys.insert(value.start_byte(), text(key, source).into());
                                }
                                "string" => {
                                    keys.insert(value.start_byte(), literal(key, source));
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    for binding in &mut module.bindings[before..] {
        if let Some(key) = keys.get(&binding.location.start_byte) {
            binding.module = Some(specifier.to_owned());
            binding.imported.clone_from(key);
            binding.commonjs = true;
        }
    }
}

/// Is `node` `module.exports`?
fn module_exports(node: Node<'_>, source: &str) -> bool {
    node.kind() == "member_expression"
        && node
            .child_by_field_name("object")
            .is_some_and(|object| object.kind() == "identifier" && text(object, source) == "module")
        && node
            .child_by_field_name("property")
            .is_some_and(|property| text(property, source) == "exports")
}

/// `exports` (while it still aliases `module.exports`) or `module.exports`.
fn exports_object(node: Node<'_>, source: &str, alias_live: bool) -> bool {
    module_exports(node, source)
        || (alias_live && node.kind() == "identifier" && text(node, source) == "exports")
}

/// The static value of an exported expression.
fn export_value(
    name: String,
    value: Node<'_>,
    site: Node<'_>,
    source: &str,
    file: &IndexedFile,
) -> CommonJsExport {
    let mut export = CommonJsExport {
        name,
        local: None,
        module: None,
        member: None,
        fact: inline_fact(file, site, value),
        location: location(site, &file.path),
    };
    if matches!(value.kind(), "identifier" | "shorthand_property_identifier") {
        export.local = Some(text(value, source).into());
    } else if let Some((specifier, member)) = require_call(value, source) {
        export.module = Some(specifier);
        export.member = member;
    } else if let Some((local, member)) = local_member(value, source) {
        export.local = Some(local);
        export.member = Some(member);
    }
    export
}

/// `x.member` with an identifier object (not `this`) and a plain property name.
fn local_member(value: Node<'_>, source: &str) -> Option<(String, String)> {
    if value.kind() != "member_expression" {
        return None;
    }
    let object = value
        .child_by_field_name("object")
        .filter(|object| object.kind() == "identifier")?;
    let property = value
        .child_by_field_name("property")
        .filter(|property| property.kind() == "property_identifier")?;
    Some((text(object, source).into(), text(property, source).into()))
}

/// `CommonJS` export table: unconditional top-level `module.exports = …`,
/// `module.exports.name = …` and `exports.name = …` statements, in order.
/// Anything that could change the exports at run time without being one of
/// those statements makes the table dynamic instead of guessed.
#[allow(clippy::too_many_lines)] // One arm per supported export statement form.
fn commonjs(
    root: Node<'_>,
    source: &str,
    file: &IndexedFile,
    writes: &[(String, usize)],
    module: &mut Module,
) {
    let mut table = CommonJs::default();
    let mut seen = false;
    let mut alias_live = true;
    let mut handled = std::collections::HashSet::new();
    // The first reason is kept, except that a reason which may replace the
    // `module.exports` value itself outranks one that only affects members.
    let dynamic = |reason: &str, table: &mut CommonJs| {
        let members_only = |reason: &str| MEMBER_ONLY_EXPORT_REASONS.contains(&reason);
        if table
            .dynamic
            .as_deref()
            .is_none_or(|current| members_only(current) && !members_only(reason))
        {
            table.dynamic = Some(reason.into());
        }
    };
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        // `const p = module.exports = …` / `var p = exports = module.exports = …`:
        // the exported value is the declared binding.
        if let Some((declarator, chain, keeps_alias)) = declared_exports(statement, source) {
            for left in chain {
                handled.insert(left);
            }
            seen = true;
            alias_live = keeps_alias;
            table.exports.clear();
            if let Some(name) = declarator.child_by_field_name("name") {
                table.exports.push(CommonJsExport {
                    name: String::new(),
                    local: Some(text(name, source).into()),
                    module: None,
                    member: None,
                    fact: None,
                    location: location(declarator, &file.path),
                });
            }
            continue;
        }
        let Some(mut assignment) = (statement.kind() == "expression_statement")
            .then(|| statement.named_child(0))
            .flatten()
            .filter(|node| node.kind() == "assignment_expression")
        else {
            continue;
        };
        // `exports = module.exports = value`: both name the new value, so the
        // `exports` alias stays live for later `exports.name = …` statements.
        let mut keeps_alias = false;
        if let (Some(outer), Some(inner)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
        ) && outer.kind() == "identifier"
            && text(outer, source) == "exports"
            && inner.kind() == "assignment_expression"
            && inner
                .child_by_field_name("left")
                .is_some_and(|left| module_exports(left, source))
            && inner
                .child_by_field_name("right")
                .is_some_and(|value| value.kind() != "assignment_expression")
        {
            handled.insert(outer.start_byte());
            assignment = inner;
            keeps_alias = true;
        }
        let (Some(left), Some(right)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
        ) else {
            continue;
        };
        if left.kind() == "identifier" && text(left, source) == "exports" {
            // `exports = …` rebinds the local alias only.
            handled.insert(left.start_byte());
            alias_live = false;
            continue;
        }
        if module_exports(left, source) {
            handled.insert(left.start_byte());
            seen = true;
            alias_live = keeps_alias;
            table.exports.clear();
            match right.kind() {
                "object" => {
                    let mut properties = right.walk();
                    for property in right.named_children(&mut properties) {
                        match property.kind() {
                            "shorthand_property_identifier" => {
                                let name = text(property, source).to_owned();
                                table.exports.retain(|export| export.name != name);
                                table
                                    .exports
                                    .push(export_value(name, property, property, source, file));
                            }
                            "pair" => {
                                let key = property.child_by_field_name("key");
                                let value = property.child_by_field_name("value");
                                let name = key.and_then(|key| match key.kind() {
                                    "property_identifier" => Some(text(key, source).to_owned()),
                                    "string" => Some(literal(key, source)),
                                    _ => None,
                                });
                                match (name, value) {
                                    (Some(name), Some(value)) => {
                                        table.exports.retain(|export| export.name != name);
                                        table.exports.push(export_value(
                                            name, value, property, source, file,
                                        ));
                                    }
                                    _ => dynamic("computed_export_key", &mut table),
                                }
                            }
                            "method_definition" => {
                                if let Some(key) = property
                                    .child_by_field_name("name")
                                    .filter(|key| key.kind() == "property_identifier")
                                {
                                    let name = text(key, source).to_owned();
                                    table.exports.retain(|export| export.name != name);
                                    table.exports.push(CommonJsExport {
                                        name,
                                        local: None,
                                        module: None,
                                        member: None,
                                        fact: inline_fact(file, property, property),
                                        location: location(property, &file.path),
                                    });
                                } else {
                                    dynamic("computed_export_key", &mut table);
                                }
                            }
                            "comment" => {}
                            _ => dynamic("spread_or_computed_export", &mut table),
                        }
                    }
                }
                "assignment_expression" => dynamic("chained_export_assignment", &mut table),
                _ => {
                    table.exports.push(export_value(
                        String::new(),
                        right,
                        assignment,
                        source,
                        file,
                    ));
                }
            }
            continue;
        }
        if !alias_live
            && matches!(left.kind(), "member_expression" | "subscript_expression")
            && left.child_by_field_name("object").is_some_and(|object| {
                object.kind() == "identifier" && text(object, source) == "exports"
            })
        {
            // The `exports` alias no longer is `module.exports`: not an export.
            handled.insert(left.start_byte());
            continue;
        }
        if matches!(left.kind(), "member_expression" | "subscript_expression")
            && let Some(object) = left.child_by_field_name("object")
            && exports_object(object, source, alias_live)
        {
            handled.insert(left.start_byte());
            seen = true;
            let name = if left.kind() == "member_expression" {
                left.child_by_field_name("property")
                    .filter(|property| property.kind() == "property_identifier")
                    .map(|property| text(property, source).to_owned())
            } else {
                left.child_by_field_name("index")
                    .filter(|index| index.kind() == "string")
                    .map(|index| literal(index, source))
            };
            match name {
                Some(name) => {
                    table.exports.retain(|export| export.name != name);
                    table
                        .exports
                        .push(export_value(name, right, assignment, source, file));
                }
                None => dynamic("computed_export_key", &mut table),
            }
        }
    }
    // Writes to the export objects anywhere else (nested, conditional, aliased):
    // rebinding `module` or assigning `module.exports` may replace the value;
    // member writes (and rebinding the `exports` alias) change members only.
    if writes
        .iter()
        .any(|(name, offset)| name == "module" && !handled.contains(offset))
        || export_writes(root, source, &handled, true)
    {
        seen = true;
        dynamic("nested_or_conditional_export_write", &mut table);
    }
    if writes
        .iter()
        .any(|(name, offset)| name == "exports" && !handled.contains(offset))
        || export_writes(root, source, &handled, false)
    {
        seen = true;
        dynamic("nested_or_conditional_export_member_write", &mut table);
    }
    if module_escapes(root, source) {
        seen = true;
        dynamic("module_object_escapes", &mut table);
    }
    if escapes(root, source) {
        seen = true;
        dynamic("exports_object_escapes", &mut table);
    }
    if module
        .bindings
        .iter()
        .any(|binding| matches!(binding.name.as_str(), "module" | "exports"))
    {
        dynamic("shadowed_module_or_exports", &mut table);
    }
    if seen
        || std::path::Path::new(&file.path)
            .extension()
            .is_some_and(|extension| extension == "cjs")
    {
        module.commonjs = Some(table);
    }
}

/// `module` used as a value (`factory(module)`, `const m = module`): code that
/// receives it may replace `module.exports`. `module.x` member uses and
/// comparisons (`require.main === module`) do not let it escape.
fn module_escapes(node: Node<'_>, source: &str) -> bool {
    if matches!(node.kind(), "comment" | "string" | "template_chars") {
        return false;
    }
    if node.kind() == "identifier"
        && text(node, source) == "module"
        && !node.parent().is_some_and(|parent| {
            (parent.kind() == "member_expression"
                && parent.child_by_field_name("object") == Some(node))
                || parent.kind() == "binary_expression"
        })
    {
        return true;
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    children
        .into_iter()
        .any(|child| module_escapes(child, source))
}

/// A write to `module.exports` itself (`whole`), or to a member of `exports` or
/// `module.exports` (otherwise), that is not one of the `handled` top-level
/// statements.
fn export_writes(
    node: Node<'_>,
    source: &str,
    handled: &std::collections::HashSet<usize>,
    whole: bool,
) -> bool {
    if matches!(
        node.kind(),
        "assignment_expression" | "augmented_assignment_expression" | "update_expression"
    ) && let Some(left) = node
        .child_by_field_name("left")
        .or_else(|| node.child_by_field_name("argument"))
        && !handled.contains(&left.start_byte())
    {
        let target = |node: Node<'_>| exports_object(node, source, true);
        let member = matches!(left.kind(), "member_expression" | "subscript_expression")
            && left.child_by_field_name("object").is_some_and(target);
        if (whole && module_exports(left, source)) || (!whole && member) {
            return true;
        }
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    children
        .into_iter()
        .any(|child| export_writes(child, source, handled, whole))
}

/// `exports`/`module.exports` passed to a call (`Object.assign(exports, …)`,
/// `factory(module.exports)`) may gain any property. The ubiquitous
/// `Object.defineProperty(exports, "__esModule", …)` only adds that flag.
fn escapes(node: Node<'_>, source: &str) -> bool {
    if node.kind() == "comment" || node.kind() == "string" {
        return false;
    }
    if node.kind() == "call_expression"
        && let Some(arguments) = node.child_by_field_name("arguments")
    {
        let mut cursor = arguments.walk();
        let passed: Vec<Node<'_>> = arguments.named_children(&mut cursor).collect();
        if passed
            .iter()
            .any(|argument| exports_object(*argument, source, true))
        {
            let es_module_flag = node
                .child_by_field_name("function")
                .is_some_and(|function| text(function, source) == "Object.defineProperty")
                && passed.get(1).is_some_and(|key| {
                    key.kind() == "string" && literal(*key, source) == "__esModule"
                });
            if !es_module_flag {
                return true;
            }
        }
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    children.into_iter().any(|child| escapes(child, source))
}

/// The class method (or field initializer, or static block) whose `this` and
/// `super` a node sees: the nearest enclosing non-arrow function must be a
/// member of a top-level class that has a fact. Any other function rebinds
/// `this`, so it yields `None`.
fn member_context(node: Node<'_>, file: &IndexedFile) -> Option<MemberContext> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "arrow_function" => {}
            "method_definition" | "field_definition" | "public_field_definition" => {
                let body = parent.parent().filter(|body| body.kind() == "class_body")?;
                return Some(MemberContext {
                    class: class_fact(body.parent()?, file)?,
                    static_context: token(parent, "static"),
                });
            }
            "class_static_block" => {
                let body = parent.parent().filter(|body| body.kind() == "class_body")?;
                return Some(MemberContext {
                    class: class_fact(body.parent()?, file)?,
                    static_context: true,
                });
            }
            kind if is_function(parent) || matches!(kind, "class_body" | "program") => {
                return None;
            }
            _ => {}
        }
        current = parent;
    }
    None
}

/// The fact ID of a class declaration node, when the declaration was indexed.
fn class_fact(class: Node<'_>, file: &IndexedFile) -> Option<String> {
    file.facts
        .iter()
        .find(|fact| {
            fact.kind == "class"
                && fact.start_byte == class.start_byte()
                && fact.end_byte == class.end_byte()
        })
        .map(|fact| fact.id.clone())
}

/// Own-property evidence for each indexed class: field declarations, string or
/// computed member names, `this.m` writes in its members, and calls that may add
/// arbitrary own properties (`Object.assign(this, …)`).
#[allow(clippy::too_many_lines)] // Members, `this` writes, then computed keys: one pass per class.
fn classes(
    root: Node<'_>,
    source: &str,
    file: &IndexedFile,
    writes: &[MemberWrite],
    rebound: &[(String, usize)],
    module: &mut Module,
) {
    for fact in file.facts.iter().filter(|fact| fact.kind == "class") {
        let Some(node) = root
            .descendant_for_byte_range(fact.start_byte, fact.end_byte)
            .filter(|node| {
                node.start_byte() == fact.start_byte && node.end_byte() == fact.end_byte
            })
        else {
            continue;
        };
        let mut info = ClassInfo {
            id: fact.id.clone(),
            ..ClassInfo::default()
        };
        if let Some(body) = node.child_by_field_name("body") {
            let mut cursor = body.walk();
            for member in body.named_children(&mut cursor) {
                let field = matches!(
                    member.kind(),
                    "field_definition" | "public_field_definition"
                );
                if !field && member.kind() != "method_definition" {
                    continue;
                }
                let Some(name) = member
                    .child_by_field_name("property")
                    .or_else(|| member.child_by_field_name("name"))
                else {
                    continue;
                };
                let named = match name.kind() {
                    "property_identifier" | "private_property_identifier" => {
                        // Methods are symbols; only fields are own properties.
                        field.then(|| text(name, source).to_owned())
                    }
                    "string" | "number" => Some(literal(name, source)),
                    // `[Symbol.iterator]()` cannot collide with an identifier
                    // name, but only while `Symbol` is the global (IR 5).
                    "computed_property_name" => {
                        match name
                            .named_child(0)
                            .map(|key| computed_key(key, root, source, module, rebound))
                        {
                            Some(ComputedKey::Named(named)) => Some(named),
                            Some(ComputedKey::Symbol) => None,
                            Some(ComputedKey::Builtin(key)) => {
                                info.builtin_symbol_keys.push(key);
                                None
                            }
                            Some(ComputedKey::Unknown) | None => {
                                info.dynamic_members = true;
                                None
                            }
                        }
                    }
                    _ => {
                        info.dynamic_members = true;
                        None
                    }
                };
                if let Some(named) = named {
                    if token(member, "static") {
                        info.static_fields.push(named);
                    } else {
                        info.instance_fields.push(named);
                    }
                }
            }
            dynamic_this(body, source, &mut info);
            info.constructor_returns = constructor_returns(body, source);
        }
        for write in writes.iter().filter(|write| {
            write.receiver.as_deref() == Some("this")
                && fact.start_byte <= write.offset
                && write.offset < fact.end_byte
        }) {
            // Written in a static or instance member: list it as both.
            if let Some(property) = &write.property {
                info.instance_fields.push(property.clone());
                info.static_fields.push(property.clone());
            } else {
                // `this[key] = …`: unknown unless the key is a proven symbol
                // (string and integer literals are already named, IR 5).
                let key = root
                    .descendant_for_byte_range(write.offset, write.offset)
                    .and_then(|mut node| {
                        while node.start_byte() == write.offset {
                            if node.kind() == "subscript_expression" {
                                return node.child_by_field_name("index");
                            }
                            node = node.parent()?;
                        }
                        None
                    });
                match key.map(|key| computed_key(key, root, source, module, rebound)) {
                    Some(ComputedKey::Symbol) => {}
                    Some(ComputedKey::Builtin(key)) => info.builtin_symbol_keys.push(key),
                    Some(ComputedKey::Named(_) | ComputedKey::Unknown) | None => {
                        info.dynamic_members = true;
                    }
                }
            }
        }
        info.builtin_symbol_keys.dedup();
        for list in [&mut info.instance_fields, &mut info.static_fields] {
            list.sort();
            list.dedup();
        }
        module.classes.push(info);
    }
}

/// `Object.assign(this, …)`, `Object.defineProperty(this, …)`, `Reflect.set(this, …)`.
fn dynamic_this(node: Node<'_>, source: &str, info: &mut ClassInfo) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && function.kind() == "member_expression"
        && function
            .child_by_field_name("object")
            .is_some_and(|object| matches!(text(object, source), "Object" | "Reflect"))
        && node
            .child_by_field_name("arguments")
            .and_then(|arguments| arguments.named_child(0))
            .is_some_and(|first| first.kind() == "this")
    {
        info.dynamic_members = true;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        dynamic_this(child, source, info);
    }
}

/// Every `X.prototype` use, and prototype manipulation that cannot be
/// attributed to a named class.
fn prototypes(node: Node<'_>, source: &str, module: &mut Module) {
    match node.kind() {
        "member_expression" => {
            if let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("property"),
            ) {
                match text(property, source) {
                    "prototype" if !prototype_may_change(node) => {}
                    "prototype" if object.kind() == "identifier" => {
                        module.prototype_uses.push(NameAt {
                            name: text(object, source).into(),
                            offset: object.start_byte(),
                        });
                    }
                    "prototype" | "__proto__" | "setPrototypeOf" => module.prototype_opaque = true,
                    _ => {}
                }
            }
        }
        "string" | "property_identifier" if literal_or_text(node, source) == "__proto__" => {
            module.prototype_opaque = true;
        }
        "comment" => return,
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        prototypes(child, source, module);
    }
}

/// Can this `X.prototype` expression lead to a change of the prototype? Reading
/// a member (`X.prototype.m.call(this)`) or comparing (`a === X.prototype.m`,
/// `x instanceof X`) cannot. Writing, deleting or letting the object escape
/// (argument, assigned value, alias, return) can.
fn prototype_may_change(prototype: Node<'_>) -> bool {
    let mut top = prototype;
    while let Some(parent) = top.parent() {
        if matches!(parent.kind(), "member_expression" | "subscript_expression")
            && parent.child_by_field_name("object") == Some(top)
        {
            top = parent;
        } else {
            break;
        }
    }
    let Some(parent) = top.parent() else {
        return true;
    };
    let written = matches!(
        parent.kind(),
        "assignment_expression" | "augmented_assignment_expression"
    ) && parent.child_by_field_name("left") == Some(top)
        || parent.kind() == "update_expression"
        || (parent.kind() == "unary_expression" && token(parent, "delete"));
    if written {
        return true;
    }
    if top == prototype {
        // The prototype object itself is used as a value.
        return !matches!(parent.kind(), "binary_expression");
    }
    // A member of the prototype is read: only calls that pass the prototype
    // on (`X.prototype.m.call(X.prototype)`) are handled by the argument rule.
    false
}

fn literal_or_text(node: Node<'_>, source: &str) -> String {
    if node.kind() == "string" {
        literal(node, source)
    } else {
        text(node, source).into()
    }
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}
fn token(node: Node<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|child| child.kind() == kind)
}
fn location(node: Node<'_>, path: &str) -> Location {
    Location {
        path: path.into(),
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: node.start_position().row + 1,
        end_line: (node.end_position().row + usize::from(node.end_position().column > 0))
            .max(node.start_position().row + 1),
    }
}
fn is_function(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "generator_function_declaration"
            | "generator_function"
            | "method_definition"
            | "function_signature"
            | "method_signature"
            | "abstract_method_signature"
    )
}
fn scope(mut node: Node<'_>, function_only: bool) -> Node<'_> {
    while let Some(parent) = node.parent() {
        node = parent;
        if node.kind() == "program"
            || is_function(node)
            || (!function_only
                && matches!(
                    node.kind(),
                    "statement_block"
                        | "for_statement"
                        | "for_in_statement"
                        | "catch_clause"
                        | "switch_body"
                ))
        {
            break;
        }
    }
    node
}
fn bind(
    name: Node<'_>,
    scope: Node<'_>,
    source: &str,
    file: &IndexedFile,
    module: &mut Module,
    target: Option<String>,
) {
    if matches!(
        name.kind(),
        "identifier" | "shorthand_property_identifier_pattern" | "type_identifier"
    ) {
        module.bindings.push(Binding {
            name: text(name, source).into(),
            target,
            module: None,
            imported: String::new(),
            scope_start: scope.start_byte(),
            scope_end: scope.end_byte(),
            top_level: scope.kind() == "program",
            mutated: false,
            type_only: false,
            location: location(name, &file.path),
            mutated_members: Vec::new(),
            commonjs: false,
            alias: None,
            constructed: None,
            escapes: None,
        });
    } else {
        let mut cursor = name.walk();
        for child in name.named_children(&mut cursor) {
            // In { key: local }, key is a property, not a binding.
            if name.kind() == "pair_pattern" && name.child_by_field_name("key") == Some(child) {
                continue;
            }
            // In `x = value` and `{ x = value }`, the default value is an expression.
            if matches!(
                name.kind(),
                "assignment_pattern" | "object_assignment_pattern"
            ) && name.child_by_field_name("right") == Some(child)
            {
                continue;
            }
            bind(child, scope, source, file, module, None);
        }
    }
}
fn bindings(node: Node<'_>, source: &str, file: &IndexedFile, module: &mut Module) {
    if matches!(node.kind(), "import_statement" | "comment" | "string") {
        return;
    }
    bind_type_parameter(node, source, file, module);
    if matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "function_signature"
            | "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration"
            | "internal_module"
            | "variable_declarator"
    ) && let Some(name) = node.child_by_field_name("name")
    {
        let var = node
            .parent()
            .is_some_and(|parent| parent.kind() == "variable_declaration");
        let visible = scope(node, var);
        let fact = file.facts.iter().find(|fact| {
            fact.start_byte == node.start_byte()
                && fact.end_byte == node.end_byte()
                && !matches!(fact.kind.as_str(), "file" | "import" | "export")
        });
        let before = module.bindings.len();
        bind(
            name,
            visible,
            source,
            file,
            module,
            fact.map(|fact| fact.id.clone()),
        );
        if node.kind() == "variable_declarator"
            && let Some((specifier, member)) = node
                .child_by_field_name("value")
                .and_then(|value| require_call(value, source))
        {
            require_bindings(name, &specifier, member, before, source, module);
        }
    }
    if matches!(
        node.kind(),
        "required_parameter" | "optional_parameter" | "catch_clause"
    ) && let Some(name) = node
        .child_by_field_name("pattern")
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.child_by_field_name("parameter"))
    {
        let visible = if node.kind() == "catch_clause" {
            node
        } else {
            scope(node, true)
        };
        bind(name, visible, source, file, module, None);
    }
    // JavaScript parameters are bare patterns inside `formal_parameters`;
    // TypeScript wraps each one in `required_parameter`/`optional_parameter`.
    if node.kind() == "formal_parameters" {
        let mut cursor = node.walk();
        for parameter in node.named_children(&mut cursor) {
            if matches!(
                parameter.kind(),
                "identifier"
                    | "assignment_pattern"
                    | "object_pattern"
                    | "array_pattern"
                    | "rest_pattern"
            ) {
                bind(parameter, scope(node, true), source, file, module, None);
            }
        }
    }
    // Single unparenthesized arrow parameter.
    if node.kind() == "arrow_function"
        && let Some(name) = node.child_by_field_name("parameter")
    {
        bind(name, node, source, file, module, None);
    }
    // Named function expressions have a function-local binding only.
    if matches!(node.kind(), "function_expression" | "generator_function")
        && let Some(name) = node.child_by_field_name("name")
    {
        bind(name, node, source, file, module, None);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        bindings(child, source, file, module);
    }
}
fn literal(node: Node<'_>, source: &str) -> String {
    let text = text(node, source);
    text.get(1..text.len().saturating_sub(1))
        .unwrap_or_default()
        .into()
}
fn bind_type_parameter(node: Node<'_>, source: &str, file: &IndexedFile, module: &mut Module) {
    if node.kind() == "type_parameter"
        && let Some(name) = node.child_by_field_name("name")
    {
        let mut visible = node;
        while let Some(parent) = visible.parent() {
            visible = parent;
            if is_function(visible)
                || matches!(
                    visible.kind(),
                    "class_declaration" | "interface_declaration" | "type_alias_declaration"
                )
            {
                break;
            }
        }
        bind(name, visible, source, file, module, None);
    }
    if node.kind() == "for_in_statement"
        && node.child_by_field_name("kind").is_some()
        && let Some(left) = node.child_by_field_name("left")
    {
        let visible = if token(node, "var") {
            scope(node, true)
        } else {
            node
        };
        bind(left, visible, source, file, module, None);
    }
    if node.kind() == "class"
        && let Some(name) = node.child_by_field_name("name")
    {
        bind(name, node, source, file, module, None);
    }
}

fn imports(node: Node<'_>, root: Node<'_>, source: &str, file: &IndexedFile, module: &mut Module) {
    let Some(from) = node.child_by_field_name("source") else {
        return;
    };
    let path = literal(from, source);
    let mut specs = Vec::new();
    import_specs(node, source, &mut specs);
    if specs.is_empty() {
        specs.push((String::new(), String::new(), node, false));
    }
    for (name, imported, site, type_only) in specs {
        module.bindings.push(Binding {
            name,
            imported,
            target: None,
            module: Some(path.clone()),
            scope_start: 0,
            scope_end: root.end_byte(),
            top_level: true,
            mutated: false,
            type_only: type_only || token(node, "type"),
            location: location(site, &file.path),
            mutated_members: Vec::new(),
            commonjs: false,
            alias: None,
            constructed: None,
            escapes: None,
        });
    }
}
fn import_specs<'a>(
    node: Node<'a>,
    source: &str,
    specs: &mut Vec<(String, String, Node<'a>, bool)>,
) {
    if node.kind() == "import_specifier" {
        if let Some(name) = node.child_by_field_name("name") {
            let local = node.child_by_field_name("alias").unwrap_or(name);
            specs.push((
                text(local, source).into(),
                text(name, source).into(),
                node,
                token(node, "type"),
            ));
        }
        return;
    }
    if node.kind() == "namespace_import" {
        if let Some(name) = node.named_child(0) {
            specs.push((text(name, source).into(), "*".into(), node, false));
        }
        return;
    }
    if node.kind() == "identifier"
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "import_clause")
    {
        specs.push((text(node, source).into(), "default".into(), node, false));
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        import_specs(child, source, specs);
    }
}
fn exports(node: Node<'_>, source: &str, file: &IndexedFile, module: &mut Module) {
    let from = node
        .child_by_field_name("source")
        .map(|node| literal(node, source));
    let default = token(node, "default");
    if let Some(declaration) = node.child_by_field_name("declaration") {
        for binding in &module.bindings {
            if binding.module.is_none()
                && binding.location.start_byte >= declaration.start_byte()
                && binding.location.end_byte <= declaration.end_byte()
                && binding.top_level
            {
                module.exports.push(Export {
                    type_only: token(node, "type"),
                    name: if default {
                        "default".into()
                    } else {
                        binding.name.clone()
                    },
                    local: binding.name.clone(),
                    module: None,
                    location: location(node, &file.path),
                });
            }
        }
        return;
    }
    if default {
        let value = node.child_by_field_name("value");
        module.exports.push(Export {
            type_only: token(node, "type"),
            name: "default".into(),
            local: value.map_or_else(
                || "<anonymous-default>".into(),
                |value| text(value, source).into(),
            ),
            module: None,
            location: location(node, &file.path),
        });
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "export_clause" {
            let mut cursor = child.walk();
            for spec in child.named_children(&mut cursor) {
                if let Some(name) = spec.child_by_field_name("name") {
                    let alias = spec.child_by_field_name("alias").unwrap_or(name);
                    module.exports.push(Export {
                        type_only: token(node, "type") || token(spec, "type"),
                        name: text(alias, source).into(),
                        local: text(name, source).into(),
                        module: from.clone(),
                        location: location(spec, &file.path),
                    });
                }
            }
            return;
        }
    }
    if from.is_some() {
        // Namespace re-export syntax is retained but deliberately unsupported.
        module.exports.push(Export {
            type_only: token(node, "type"),
            name: "*".into(),
            local: if token(node, "as") {
                "<namespace-export>".into()
            } else {
                "*".into()
            },
            module: from,
            location: location(node, &file.path),
        });
    }
}
fn owner(node: Node<'_>, file: &IndexedFile) -> String {
    file.facts
        .iter()
        .filter(|fact| {
            !matches!(fact.kind.as_str(), "file" | "import" | "export")
                && fact.start_byte <= node.start_byte()
                && fact.end_byte >= node.end_byte()
        })
        .min_by_key(|fact| fact.end_byte - fact.start_byte)
        .map_or_else(|| file_id(&file.path), |fact| fact.id.clone())
}
fn usage(
    node: Node<'_>,
    mut expression: Node<'_>,
    kind: Relationship,
    source: &str,
    file: &IndexedFile,
    module: &mut Module,
) {
    // TS grammar can put `await f` in a generic call's function field. These
    // transparent syntax wrappers do not choose a new lexical binding.
    while matches!(
        expression.kind(),
        "await_expression"
            | "non_null_expression"
            | "parenthesized_expression"
            | "instantiation_expression"
    ) {
        let inner = expression
            .child_by_field_name("function")
            .or_else(|| expression.named_child(0));
        let Some(inner) = inner else {
            break;
        };
        expression = inner;
    }
    let (name, member): (String, Option<String>) = if expression.kind() == "member_expression" {
        let object = expression.child_by_field_name("object");
        let property = expression.child_by_field_name("property");
        if let (Some(object), Some(property)) = (object, property) {
            if matches!(object.kind(), "identifier" | "this" | "super") {
                (
                    text(object, source).into(),
                    Some(text(property, source).into()),
                )
            } else {
                (
                    text(expression, source).into(),
                    Some("<unsupported-receiver>".into()),
                )
            }
        } else {
            return;
        }
    } else {
        (text(expression, source).into(), None)
    };
    let context = if matches!(name.as_str(), "this" | "super") && member.is_some() {
        member_context(expression, file)
    } else {
        None
    };
    module.uses.push(Usage {
        syntax: text(expression, source).into(),
        source_id: owner(node, file),
        name,
        member,
        kind,
        location: location(expression, &file.path),
        jsx: false,
        context,
    });
}
/// The receiver of a member expression: nested objects are visited as code; an
/// identifier is a use only when it names a non-namespace binding.
fn receiver(member: Node<'_>, source: &str, file: &IndexedFile, module: &mut Module) {
    // The object identifier alone is not a use of the selected namespace member.
    if let Some(object) = member.child_by_field_name("object") {
        if object.kind() != "identifier" {
            uses(object, source, file, module);
        } else if module.bindings.iter().any(|binding| {
            // An ES namespace object is not itself a use; a CommonJS
            // `module.exports` value held in a variable is.
            binding.name == text(object, source) && (binding.imported != "*" || binding.commonjs)
        }) {
            // A property may be unresolved while its lexical receiver binding is known.
            usage(
                object,
                object,
                Relationship::References,
                source,
                file,
                module,
            );
        }
    }
}

fn uses(node: Node<'_>, source: &str, file: &IndexedFile, module: &mut Module) {
    if matches!(
        node.kind(),
        "comment"
            | "string"
            | "template_chars"
            | "import_statement"
            | "export_clause"
            | "namespace_export"
    ) {
        return;
    }
    // JSX: the tag name of an opening or self-closing element is a component
    // reference unless it is intrinsic. Closing tags repeat the name and are skipped.
    // CODE_INTEL_DISABLE_JSX=1 reproduces the pre-3.7 extractor for benchmark controls.
    let jsx = std::env::var_os("CODE_INTEL_DISABLE_JSX").is_none_or(|value| value != "1");
    if jsx && node.kind() == "jsx_closing_element" {
        return;
    }
    if jsx
        && matches!(
            node.kind(),
            "jsx_opening_element" | "jsx_self_closing_element"
        )
    {
        let name = node.child_by_field_name("name");
        if let Some(name) = name.filter(|name| is_component(*name, source)) {
            let before = module.uses.len();
            usage(node, name, Relationship::References, source, file, module);
            if module.uses.len() > before {
                module.uses[before].jsx = true;
            }
            // `<Ctx.Provider>` still uses `Ctx`, as `Ctx.Provider` outside JSX does.
            if name.kind() == "member_expression" {
                receiver(name, source, file, module);
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if Some(child) != name {
                uses(child, source, file, module);
            }
        }
        return;
    }
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
    {
        usage(node, function, Relationship::Calls, source, file, module);
    }
    if node.kind() == "extends_clause"
        && let Some(value) = node.child_by_field_name("value")
    {
        usage(node, value, Relationship::Extends, source, file, module);
    }
    // JavaScript's `class_heritage` holds the superclass expression directly
    // (TypeScript nests it in `extends_clause`). Its identifiers are still
    // visited below as references, exactly as for TypeScript.
    if node.kind() == "class_heritage"
        && let Some(value) = node.named_child(0).filter(|value| {
            !matches!(
                value.kind(),
                "extends_clause" | "implements_clause" | "comment"
            )
        })
    {
        usage(node, value, Relationship::Extends, source, file, module);
    }
    if node.kind() == "member_expression" {
        usage(node, node, Relationship::References, source, file, module);
        receiver(node, source, file, module);
        return;
    }
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "shorthand_property_identifier"
    ) {
        let binding = module.bindings.iter().any(|binding| {
            binding.location.start_byte == node.start_byte()
                && binding.location.end_byte == node.end_byte()
        });
        let declaration_name = node.parent().is_some_and(|parent| {
            parent.child_by_field_name("name") == Some(node)
                && !matches!(parent.kind(), "new_expression")
        });
        let label = node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "labeled_statement" | "break_statement" | "continue_statement"
            )
        });
        if !binding && !declaration_name && !label {
            usage(node, node, Relationship::References, source, file, module);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        uses(child, source, file, module);
    }
}
/// React/TypeScript rule: a lowercase first letter, a hyphen or a namespace
/// (`svg:rect`) makes a tag intrinsic; member names (`<Ns.Comp>`) are values.
fn is_component(name: Node<'_>, source: &str) -> bool {
    match name.kind() {
        "member_expression" => true,
        "identifier" => {
            let text = text(name, source);
            !text.contains('-') && !text.starts_with(|first: char| first.is_ascii_lowercase())
        }
        _ => false,
    }
}
fn mutations(
    node: Node<'_>,
    source: &str,
    writes: &mut Vec<(String, usize)>,
    members: &mut Vec<MemberWrite>,
) {
    if node.kind() == "for_in_statement"
        && node.child_by_field_name("kind").is_none()
        && let Some(left) = node.child_by_field_name("left")
    {
        assigned_names(left, source, writes, members);
    }
    if matches!(
        node.kind(),
        "assignment_expression" | "augmented_assignment_expression" | "update_expression"
    ) && let Some(left) = node
        .child_by_field_name("left")
        .or_else(|| node.child_by_field_name("argument"))
    {
        // `this.m = this.m.bind(this)` keeps the declared method as the callee.
        if !(node.kind() == "assignment_expression" && rebinds_own_method(node, left, source)) {
            assigned_names(left, source, writes, members);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        mutations(child, source, writes, members);
    }
}

/// `this.m = this.m.bind(this)`: the own property is the prototype method bound
/// to the instance, so the declaration it calls is unchanged.
fn rebinds_own_method(assignment: Node<'_>, left: Node<'_>, source: &str) -> bool {
    let this_member = |node: Node<'_>| -> Option<String> {
        (node.kind() == "member_expression"
            && node
                .child_by_field_name("object")
                .is_some_and(|object| object.kind() == "this"))
        .then(|| node.child_by_field_name("property"))
        .flatten()
        .filter(|property| property.kind() == "property_identifier")
        .map(|property| text(property, source).to_owned())
    };
    let Some(name) = this_member(left) else {
        return false;
    };
    let Some(right) = assignment.child_by_field_name("right") else {
        return false;
    };
    let Some(function) = right
        .child_by_field_name("function")
        .filter(|_| right.kind() == "call_expression")
    else {
        return false;
    };
    let bound = function.kind() == "member_expression"
        && function
            .child_by_field_name("property")
            .is_some_and(|property| text(property, source) == "bind")
        && function
            .child_by_field_name("object")
            .and_then(this_member)
            .as_deref()
            == Some(name.as_str());
    let arguments = right.child_by_field_name("arguments");
    let only_this = arguments.is_some_and(|arguments| {
        arguments.named_child_count() == 1
            && arguments
                .named_child(0)
                .is_some_and(|first| first.kind() == "this")
    });
    bound && only_this
}

/// The names a write target rebinds, and the members it writes. Identifiers in
/// a computed key, a member's object or a destructuring default are reads.
fn assigned_names(
    node: Node<'_>,
    source: &str,
    writes: &mut Vec<(String, usize)>,
    members: &mut Vec<MemberWrite>,
) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            writes.push((text(node, source).into(), node.start_byte()));
        }
        "member_expression" | "subscript_expression" => {
            let object = node.child_by_field_name("object");
            let property = if node.kind() == "member_expression" {
                node.child_by_field_name("property")
                    .map(|property| text(property, source).to_owned())
            } else {
                node.child_by_field_name("index")
                    .and_then(|index| match index.kind() {
                        "string" if !text(index, source).contains('\\') => {
                            Some(literal(index, source))
                        }
                        // `x[0]` writes the property "0" (JavaScript property-key
                        // coercion); only canonical decimal integers are named.
                        "number" => canonical_integer(text(index, source)),
                        _ => None,
                    })
            };
            // `module.exports.x` and `F.prototype.x` keep their dotted receiver so
            // the binder can tell export and prototype statements from writes
            // on unknown objects.
            let dotted = object
                .filter(|object| object.kind() == "member_expression")
                .and_then(|object| {
                    let (base, member) = local_member(object, source)?;
                    (property.is_some()
                        && ((base == "module" && member == "exports") || member == "prototype"))
                        .then(|| format!("{base}.{member}"))
                });
            members.push(MemberWrite {
                receiver: object
                    .filter(|object| matches!(object.kind(), "identifier" | "this"))
                    .map(|object| text(object, source).to_owned())
                    .or(dotted),
                property,
                offset: node.start_byte(),
            });
        }
        "parenthesized_expression"
        | "non_null_expression"
        | "as_expression"
        | "satisfies_expression"
        | "type_assertion" => {
            if let Some(inner) = node.named_child(0) {
                assigned_names(inner, source, writes, members);
            }
        }
        "object_pattern"
        | "array_pattern"
        | "rest_pattern"
        | "pair_pattern"
        | "assignment_pattern"
        | "object_assignment_pattern" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                // In `{ key: target }` the key (even computed) is not written.
                if node.kind() == "pair_pattern" && node.child_by_field_name("key") == Some(child) {
                    continue;
                }
                // In `x = value` / `{ x = value }` the default is an expression.
                if matches!(
                    node.kind(),
                    "assignment_pattern" | "object_assignment_pattern"
                ) && node.child_by_field_name("right") == Some(child)
                {
                    continue;
                }
                assigned_names(child, source, writes, members);
            }
        }
        _ => {
            // Unknown target shapes keep the conservative whole-subtree rule.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                assigned_names(child, source, writes, members);
            }
        }
    }
}

/// `const p = module.exports = value` or `var p = exports = module.exports = value`
/// as the only declarator of a top-level declaration: the declarator, the
/// written targets' offsets, and whether `exports` still aliases the value.
fn declared_exports<'a>(statement: Node<'a>, source: &str) -> Option<(Node<'a>, Vec<usize>, bool)> {
    if !matches!(
        statement.kind(),
        "lexical_declaration" | "variable_declaration"
    ) || statement.named_child_count() != 1
    {
        return None;
    }
    let declarator = statement.named_child(0)?;
    declarator
        .child_by_field_name("name")
        .filter(|name| name.kind() == "identifier")?;
    let mut value = declarator.child_by_field_name("value")?;
    let mut chain = Vec::new();
    let mut alias = false;
    let mut exported = false;
    while value.kind() == "assignment_expression" {
        let left = value.child_by_field_name("left")?;
        if module_exports(left, source) && !exported {
            exported = true;
        } else if left.kind() == "identifier" && text(left, source) == "exports" && !exported {
            alias = true;
        } else {
            return None;
        }
        chain.push(left.start_byte());
        value = value.child_by_field_name("right")?;
    }
    exported.then_some((declarator, chain, alias))
}

/// Alias (`const v = w`, `const v = w.p`) and constructor (`const x = new F()`)
/// origins of declared bindings. `require(…)` declarators are `CommonJS` bindings.
/// TypeScript records only constructor origins: its aliases keep the 0.1.2
/// lexical rule.
fn origins(module: &mut Module, root: Node<'_>, source: &str, javascript: bool) {
    for binding in &mut module.bindings {
        if binding.module.is_some() || binding.commonjs {
            continue;
        }
        let Some(declarator) = root
            .descendant_for_byte_range(binding.location.start_byte, binding.location.end_byte)
            .and_then(|name| name.parent())
            .filter(|declarator| {
                declarator.kind() == "variable_declarator"
                    && declarator
                        .child_by_field_name("name")
                        .is_some_and(|name| name.start_byte() == binding.location.start_byte)
            })
        else {
            continue;
        };
        let Some(value) = declarator.child_by_field_name("value") else {
            continue;
        };
        match value.kind() {
            "identifier"
                if javascript && !matches!(text(value, source), "undefined" | "arguments") =>
            {
                binding.alias = Some(AliasOf {
                    name: text(value, source).into(),
                    member: None,
                });
            }
            "member_expression" if javascript => {
                if let Some((name, member)) = local_member(value, source) {
                    binding.alias = Some(AliasOf {
                        name,
                        member: Some(member),
                    });
                }
            }
            "new_expression" => {
                binding.constructed =
                    value
                        .child_by_field_name("constructor")
                        .and_then(|constructor| match constructor.kind() {
                            "identifier" => Some(text(constructor, source).to_owned()),
                            // `new ns.C()`: a namespace or module member.
                            "member_expression" => local_member(constructor, source)
                                .map(|(name, member)| format!("{name}.{member}")),
                            _ => None,
                        });
            }
            _ => {}
        }
    }
}

/// `0`, `7`, `42`: a decimal integer literal whose text is the property key
/// JavaScript coerces it to. Leading zeros, fractions, exponents, separators,
/// other radixes and values beyond 2^53 are not named.
fn canonical_integer(literal: &str) -> Option<String> {
    let canonical = !literal.is_empty()
        && literal.len() <= 15
        && literal.bytes().all(|byte| byte.is_ascii_digit())
        && (literal == "0" || !literal.starts_with('0'));
    canonical.then(|| literal.to_owned())
}

/// The binding `name` resolves to at `offset`: the innermost visible one, or
/// `None` when there is none or several share the innermost scope.
fn visible_binding(module: &Module, name: &str, offset: usize) -> Option<usize> {
    innermost(module, 0..module.bindings.len(), name, offset)
}

/// [`visible_binding`] over candidate binding indexes (all with that name, or a superset).
fn innermost(
    module: &Module,
    candidates: impl IntoIterator<Item = usize>,
    name: &str,
    offset: usize,
) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    let mut tied = false;
    for index in candidates {
        let binding = &module.bindings[index];
        if binding.name != name || offset < binding.scope_start || offset >= binding.scope_end {
            continue;
        }
        let size = binding.scope_end - binding.scope_start;
        match best {
            Some((_, smallest)) if size > smallest => {}
            Some((_, smallest)) if size == smallest => tied = true,
            _ => {
                best = Some((index, size));
                tied = false;
            }
        }
    }
    best.filter(|_| !tied).map(|(index, _)| index)
}

/// Is the global `name` (`Symbol`) unshadowed and never assigned in this module?
fn global(module: &Module, writes: &[(String, usize)], name: &str, offset: usize) -> bool {
    !module.bindings.iter().any(|binding| {
        binding.name == name && binding.scope_start <= offset && offset < binding.scope_end
    }) && !writes.iter().any(|(written, _)| written == name)
}

/// Well-known symbols (ECMAScript, plus explicit resource management).
const WELL_KNOWN_SYMBOLS: [&str; 15] = [
    "asyncDispose",
    "asyncIterator",
    "dispose",
    "hasInstance",
    "isConcatSpreadable",
    "iterator",
    "match",
    "matchAll",
    "replace",
    "search",
    "species",
    "split",
    "toPrimitive",
    "toStringTag",
    "unscopables",
];

/// What a computed property key statically is.
enum ComputedKey {
    /// A string or integer key with this name.
    Named(String),
    /// A symbol that cannot equal any string-named property: a well-known
    /// symbol of the global `Symbol`, `Symbol.for("literal")`, or a
    /// `const k = Symbol(…)` binding.
    Symbol,
    /// A Node.js built-in symbol reached through a module binding; the binder
    /// checks the specifier is the built-in module.
    Builtin(BuiltinSymbolKey),
    Unknown,
}

/// Classify a computed key expression. Anything not proven from source and
/// the language's built-ins is `Unknown`: a shadowed or reassigned `Symbol`,
/// `Symbol.for(name)`, `getKey()`, a mutable binding or another module's value.
fn computed_key(
    key: Node<'_>,
    root: Node<'_>,
    source: &str,
    module: &Module,
    writes: &[(String, usize)],
) -> ComputedKey {
    let mut key = key;
    while key.kind() == "parenthesized_expression" {
        let Some(inner) = key.named_child(0) else {
            return ComputedKey::Unknown;
        };
        key = inner;
    }
    let offset = key.start_byte();
    let symbol_global = || global(module, writes, "Symbol", offset);
    match key.kind() {
        "string" if !text(key, source).contains('\\') => ComputedKey::Named(literal(key, source)),
        "number" => {
            canonical_integer(text(key, source)).map_or(ComputedKey::Unknown, ComputedKey::Named)
        }
        "member_expression" => {
            if let Some((object, property)) = local_member(key, source)
                && object == "Symbol"
                && WELL_KNOWN_SYMBOLS.contains(&property.as_str())
                && symbol_global()
            {
                return ComputedKey::Symbol;
            }
            builtin_symbol(key, source, module).map_or(ComputedKey::Unknown, ComputedKey::Builtin)
        }
        "call_expression" => {
            let registered = key
                .child_by_field_name("function")
                .and_then(|function| local_member(function, source))
                .is_some_and(|(object, property)| object == "Symbol" && property == "for");
            let literal_argument = key
                .child_by_field_name("arguments")
                .is_some_and(|arguments| {
                    arguments.named_child_count() == 1
                        && arguments.named_child(0).is_some_and(|argument| {
                            argument.kind() == "string" && !text(argument, source).contains('\\')
                        })
                });
            if registered && literal_argument && symbol_global() {
                ComputedKey::Symbol
            } else {
                ComputedKey::Unknown
            }
        }
        "identifier" => {
            let Some(index) = visible_binding(module, text(key, source), offset) else {
                return ComputedKey::Unknown;
            };
            let binding = &module.bindings[index];
            if binding.mutated || binding.module.is_some() || binding.commonjs {
                return ComputedKey::Unknown;
            }
            // `const k = Symbol()` / `let k = Symbol("description")`.
            let unique = root
                .descendant_for_byte_range(binding.location.start_byte, binding.location.end_byte)
                .and_then(|name| name.parent())
                .filter(|declarator| {
                    declarator.kind() == "variable_declarator"
                        && declarator
                            .parent()
                            .is_some_and(|declaration| declaration.kind() == "lexical_declaration")
                })
                .and_then(|declarator| declarator.child_by_field_name("value"))
                .filter(|value| value.kind() == "call_expression")
                .and_then(|call| call.child_by_field_name("function"))
                .is_some_and(|function| {
                    function.kind() == "identifier"
                        && text(function, source) == "Symbol"
                        && global(module, writes, "Symbol", function.start_byte())
                });
            if unique {
                ComputedKey::Symbol
            } else {
                ComputedKey::Unknown
            }
        }
        _ => ComputedKey::Unknown,
    }
}

/// `util.inspect.custom` / `inspect.custom` where the binding is an unwritten
/// `require()` or `import` of `util` / `node:util`.
fn builtin_symbol(key: Node<'_>, source: &str, module: &Module) -> Option<BuiltinSymbolKey> {
    let property = key
        .child_by_field_name("property")
        .filter(|property| text(*property, source) == "custom")?;
    let object = key.child_by_field_name("object")?;
    let (binding_name, imported, read): (String, &[&str], Option<String>) = match object.kind() {
        // `inspect.custom` with `inspect` imported from util.
        "identifier" => (text(object, source).to_owned(), &["inspect"], None),
        // `util.inspect.custom` with the module object.
        "member_expression" => {
            let (name, member) = local_member(object, source)?;
            if member != "inspect" {
                return None;
            }
            (name, &["*", "default"], Some(member))
        }
        _ => return None,
    };
    let binding = &module.bindings[visible_binding(module, &binding_name, key.start_byte())?];
    let specifier = binding.module.as_deref()?;
    let written = |member: &str| {
        binding
            .mutated_members
            .iter()
            .any(|written| written == member || written == "*")
    };
    let valid = matches!(specifier, "util" | "node:util")
        && imported.contains(&binding.imported.as_str())
        && !(binding.commonjs && binding.imported == "default")
        && !binding.mutated
        && !binding.type_only
        && read.as_deref().is_none_or(|member| !written(member))
        && (read.is_some() || !written(text(property, source)));
    valid.then(|| BuiltinSymbolKey {
        expression: text(key, source).to_owned(),
        specifier: specifier.to_owned(),
        property: text(property, source).to_owned(),
    })
}

/// Does a class `constructor` return a value other than `this`? Returns in
/// nested functions and classes belong to them.
fn constructor_returns(body: Node<'_>, source: &str) -> bool {
    let mut cursor = body.walk();
    body.named_children(&mut cursor).any(|member| {
        member.kind() == "method_definition"
            && member
                .child_by_field_name("name")
                .is_some_and(|name| text(name, source) == "constructor")
            && !token(member, "static")
            && member
                .child_by_field_name("body")
                .is_some_and(|body| returns_value(body))
    })
}
fn returns_value(node: Node<'_>) -> bool {
    if node.kind() == "return_statement" {
        return node
            .named_child(0)
            .is_some_and(|value| value.kind() != "this" && value.kind() != "comment");
    }
    if is_function(node) || matches!(node.kind(), "class" | "class_declaration") {
        return false;
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    children.into_iter().any(returns_value)
}

/// Why each constructed binding (`const x = new C()`) and identifier alias
/// (`const y = x`) may let its value reach code Repogi does not follow.
/// Member accesses, calls, comparisons, `typeof`, and an alias declaration
/// (whose own uses then count, transitively) keep the value local; passing,
/// returning, storing, assigning or exporting it does not. Member writes
/// through an alias are also writes through the original binding.
fn receiver_escapes(module: &mut Module, root: Node<'_>, source: &str) {
    let tracked: Vec<usize> = module
        .bindings
        .iter()
        .enumerate()
        .filter(|(_, binding)| {
            binding.constructed.is_some()
                || binding
                    .alias
                    .as_ref()
                    .is_some_and(|alias| alias.member.is_none())
        })
        .map(|(index, _)| index)
        .collect();
    if tracked.is_empty() {
        return;
    }
    let names: std::collections::HashSet<String> = tracked
        .iter()
        .map(|index| module.bindings[*index].name.clone())
        .collect();
    // Every binding with a tracked name, so each occurrence only compares
    // same-named bindings (large files stay linear in practice).
    let mut by_name: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for (index, binding) in module.bindings.iter().enumerate() {
        if names.contains(&binding.name) {
            by_name
                .entry(binding.name.as_str())
                .or_default()
                .push(index);
        }
    }
    let tracked_set: std::collections::HashSet<usize> = tracked.iter().copied().collect();
    let mut direct: std::collections::HashMap<usize, &'static str> =
        std::collections::HashMap::new();
    let mut children: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if matches!(node.kind(), "comment" | "string" | "template_chars") {
            continue;
        }
        if matches!(node.kind(), "identifier" | "shorthand_property_identifier")
            && let Some(candidates) = by_name.get(text(node, source))
            && let Some(index) = innermost(
                module,
                candidates.iter().copied(),
                text(node, source),
                node.start_byte(),
            )
            && tracked_set.contains(&index)
        {
            match use_of(node, &module.bindings[index], module, source) {
                Use::Local => {}
                Use::Alias(alias) => children.entry(index).or_default().push(alias),
                Use::Escapes(reason) => {
                    direct.entry(index).or_insert(reason);
                }
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    // An alias's escapes and member writes are the original value's too.
    for &index in &tracked {
        let mut reason = direct.get(&index).map(|reason| (*reason).to_owned());
        let mut members = module.bindings[index].mutated_members.clone();
        let mut stack = children.get(&index).cloned().unwrap_or_default();
        let mut visited = std::collections::HashSet::from([index]);
        while let Some(alias) = stack.pop() {
            if !visited.insert(alias) {
                continue;
            }
            if visited.len() > 64 {
                reason.get_or_insert_with(|| "alias_chain_too_long".into());
                break;
            }
            if reason.is_none()
                && let Some(escaped) = direct.get(&alias)
            {
                reason = Some(format!("{escaped} through {}", module.bindings[alias].name));
            }
            members.extend(module.bindings[alias].mutated_members.iter().cloned());
            stack.extend(children.get(&alias).into_iter().flatten().copied());
        }
        members.sort();
        members.dedup();
        let binding = &mut module.bindings[index];
        binding.escapes = reason;
        binding.mutated_members = members;
    }
}

/// How one occurrence of a tracked binding uses its value.
enum Use {
    /// Member access, call, comparison, `typeof`, or a rebinding of the name.
    Local,
    /// `const y = x`: the alias binding's uses count too.
    Alias(usize),
    /// Passed, returned, stored, assigned, exported or otherwise a value.
    Escapes(&'static str),
}

fn use_of(node: Node<'_>, binding: &Binding, module: &Module, source: &str) -> Use {
    if node.start_byte() == binding.location.start_byte {
        // The declaration's own name: exported declarations escape.
        let exported = node
            .parent()
            .and_then(|declarator| declarator.parent())
            .and_then(|declaration| declaration.parent())
            .is_some_and(|statement| statement.kind() == "export_statement");
        return if exported {
            Use::Escapes("exported")
        } else {
            Use::Local
        };
    }
    let Some(parent) = node.parent() else {
        return Use::Escapes("used_as_value");
    };
    let field = |name: &str| parent.child_by_field_name(name) == Some(node);
    match parent.kind() {
        "member_expression" | "subscript_expression" if field("object") => Use::Local,
        "call_expression" if field("function") => Use::Local,
        "binary_expression" | "expression_statement" => Use::Local,
        "unary_expression" if ["typeof", "!", "void"].iter().any(|op| token(parent, op)) => {
            Use::Local
        }
        // `x = …` rebinds `x` (recorded as a mutation, not an escape).
        "assignment_expression" if field("left") => Use::Local,
        "variable_declarator" if field("value") => parent
            .child_by_field_name("name")
            .filter(|name| name.kind() == "identifier")
            .and_then(|name| {
                module.bindings.iter().position(|alias| {
                    alias.location.start_byte == name.start_byte()
                        && alias.alias.as_ref().is_some_and(|origin| {
                            origin.member.is_none() && origin.name == text(node, source)
                        })
                })
            })
            .map_or(Use::Escapes("aliased"), Use::Alias),
        "arguments" => Use::Escapes("passed_as_argument"),
        "return_statement" => Use::Escapes("returned"),
        "arrow_function" if field("body") => Use::Escapes("returned"),
        "export_specifier" => Use::Escapes("exported"),
        "assignment_expression" | "pair" | "array" => Use::Escapes("stored"),
        _ if node.kind() == "shorthand_property_identifier" => Use::Escapes("stored"),
        _ => Use::Escapes("used_as_value"),
    }
}

/// A property of an object literal or an assigned member: its value's fact,
/// local identifier, `local.member` or `require()` origin.
fn property_value(
    name: String,
    site: Node<'_>,
    value: Node<'_>,
    source: &str,
    file: &IndexedFile,
) -> ObjectProperty {
    let export = export_value(name, value, site, source, file);
    ObjectProperty {
        name: export.name,
        fact: export.fact,
        local: export.local,
        member: export.member,
        module: export.module,
        accessor: false,
        redefined: false,
        location: export.location,
    }
}

fn add_property(table: &mut ObjectTable, property: ObjectProperty) {
    if let Some(existing) = table
        .properties
        .iter_mut()
        .find(|existing| existing.name == property.name)
    {
        existing.redefined = true;
    } else {
        table.properties.push(property);
    }
}

/// The plain key of an object-literal property: identifier, string or number.
fn property_key(key: Node<'_>, source: &str) -> Option<String> {
    match key.kind() {
        "property_identifier" | "number" => Some(text(key, source).into()),
        "string" if !text(key, source).contains('\\') => Some(literal(key, source)),
        _ => None,
    }
}

/// Is `node` in a written position: an assignment target, an update operand
/// or a `delete` operand?
fn written(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        (matches!(
            parent.kind(),
            "assignment_expression" | "augmented_assignment_expression"
        ) && parent.child_by_field_name("left") == Some(node))
            || parent.kind() == "update_expression"
            || (parent.kind() == "unary_expression" && token(parent, "delete"))
    })
}

/// Is the identifier at `node` the value of a top-level `CommonJS` export
/// statement (`module.exports = x`, `exports.n = x`, `module.exports = { x }`)?
fn exported_value(node: Node<'_>, source: &str) -> bool {
    let mut site = node;
    if let Some(parent) = node.parent().filter(|parent| parent.kind() == "object") {
        site = parent;
    }
    let Some(assignment) = site
        .parent()
        .filter(|parent| parent.kind() == "assignment_expression")
    else {
        return false;
    };
    let top = assignment.parent().is_some_and(|statement| {
        statement.kind() == "expression_statement"
            && statement
                .parent()
                .is_some_and(|root| root.kind() == "program")
    });
    let left = assignment.child_by_field_name("left");
    top && assignment.child_by_field_name("right") == Some(site)
        && left.is_some_and(|left| {
            module_exports(left, source)
                || (site == node
                    && left.kind() == "member_expression"
                    && left
                        .child_by_field_name("object")
                        .is_some_and(|object| exports_object(object, source, true)))
        })
}

/// The first use of `name` (at top-level binding scope) that lets the value
/// escape or change outside the statements that built its table. `allowed`
/// accepts the remaining shapes.
fn escape(
    node: Node<'_>,
    name: &str,
    source: &str,
    module: &Module,
    allowed: &dyn Fn(Node<'_>) -> Option<&'static str>,
) -> Option<&'static str> {
    if matches!(node.kind(), "comment" | "string" | "template_chars") {
        return None;
    }
    if matches!(node.kind(), "identifier" | "shorthand_property_identifier")
        && text(node, source) == name
        && !module.bindings.iter().any(|binding| {
            binding.name == name
                && !binding.top_level
                && binding.scope_start <= node.start_byte()
                && node.start_byte() < binding.scope_end
        })
        && let Some(reason) = allowed(node)
    {
        return Some(reason);
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    children
        .into_iter()
        .find_map(|child| escape(child, name, source, module, allowed))
}

/// Object tables of top-level `const|let|var x = { … }` bindings (also
/// `x = module.exports = { … }`), extended by unconditional top-level
/// `x.name = value` statements. The table is dynamic when `x` is used in any
/// other way that could change or leak the object: passed or assigned as a
/// value, written in nested code, computed or deleted members, spreads,
/// computed keys or `__proto__` in the literal.
#[allow(clippy::too_many_lines)] // Literal, statements, then the escape scan.
fn objects(root: Node<'_>, source: &str, file: &IndexedFile, module: &mut Module) {
    let mut tables: Vec<(usize, ObjectTable)> = Vec::new();
    let mut cursor = root.walk();
    let statements: Vec<Node<'_>> = root.named_children(&mut cursor).collect();
    for statement in &statements {
        let declaration = if statement.kind() == "export_statement" {
            statement.child_by_field_name("declaration")
        } else {
            Some(*statement)
        };
        let Some(declaration) = declaration.filter(|declaration| {
            matches!(
                declaration.kind(),
                "lexical_declaration" | "variable_declaration"
            )
        }) else {
            continue;
        };
        let mut inner = declaration.walk();
        for declarator in declaration.named_children(&mut inner) {
            let (Some(name), Some(mut value)) = (
                declarator
                    .child_by_field_name("name")
                    .filter(|name| name.kind() == "identifier"),
                declarator.child_by_field_name("value"),
            ) else {
                continue;
            };
            let mut supported = true;
            while value.kind() == "assignment_expression" {
                let left = value.child_by_field_name("left");
                supported &= left.is_some_and(|left| {
                    module_exports(left, source)
                        || (left.kind() == "identifier" && text(left, source) == "exports")
                });
                let Some(right) = value.child_by_field_name("right") else {
                    break;
                };
                value = right;
            }
            let Some(owner) = file
                .facts
                .iter()
                .find(|fact| {
                    fact.kind == "variable"
                        && fact.start_byte == declarator.start_byte()
                        && fact.end_byte == declarator.end_byte()
                })
                .map(|fact| fact.id.clone())
            else {
                continue;
            };
            if value.kind() != "object" || !supported {
                continue;
            }
            let mut table = ObjectTable {
                name: text(name, source).into(),
                owner,
                ..ObjectTable::default()
            };
            let mut properties = value.walk();
            for property in value.named_children(&mut properties) {
                match property.kind() {
                    "comment" => {}
                    "shorthand_property_identifier" => {
                        let key = text(property, source).to_owned();
                        add_property(
                            &mut table,
                            property_value(key, property, property, source, file),
                        );
                    }
                    "pair" => {
                        let key = property
                            .child_by_field_name("key")
                            .and_then(|key| property_key(key, source));
                        match (key, property.child_by_field_name("value")) {
                            (Some(key), _) if key == "__proto__" => {
                                table.dynamic = Some("prototype_set_in_literal".into());
                            }
                            (Some(key), Some(value)) => add_property(
                                &mut table,
                                property_value(key, property, value, source, file),
                            ),
                            _ => table.dynamic = Some("computed_or_spread_property".into()),
                        }
                    }
                    "method_definition" => {
                        match property
                            .child_by_field_name("name")
                            .and_then(|key| property_key(key, source))
                        {
                            Some(key) => {
                                let accessor = token(property, "get") || token(property, "set");
                                let mut entry =
                                    property_value(key, property, property, source, file);
                                entry.fact = (!accessor)
                                    .then(|| inline_fact(file, property, property))
                                    .flatten();
                                entry.accessor = accessor;
                                add_property(&mut table, entry);
                            }
                            None => {
                                table.dynamic = Some("computed_or_spread_property".into());
                            }
                        }
                    }
                    _ => table.dynamic = Some("computed_or_spread_property".into()),
                }
            }
            tables.push((name.start_byte(), table));
        }
    }
    // Unconditional top-level `x.name = value` statements, in source order.
    for statement in &statements {
        let Some(assignment) = (statement.kind() == "expression_statement")
            .then(|| statement.named_child(0))
            .flatten()
            .filter(|node| node.kind() == "assignment_expression")
        else {
            continue;
        };
        let (Some(left), Some(value)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
        ) else {
            continue;
        };
        let Some((receiver, property)) = local_member(left, source) else {
            continue;
        };
        let Some((_, table)) = tables.iter_mut().find(|(_, table)| table.name == receiver) else {
            continue;
        };
        table.statements.push(left.start_byte());
        let mut entry = property_value(property, assignment, value, source, file);
        if value.kind() == "assignment_expression" {
            // `x.a = y.b = f`: the chain's other targets are separate writes.
            entry.fact = None;
            entry.local = None;
            entry.member = None;
            entry.module = None;
        }
        add_property(table, entry);
    }
    for (declared, table) in &mut tables {
        if table.dynamic.is_some() {
            continue;
        }
        let statements = table.statements.clone();
        let declared = *declared;
        let allowed = |node: Node<'_>| -> Option<&'static str> {
            if node.start_byte() == declared || exported_value(node, source) {
                return None;
            }
            let parent = node.parent()?;
            match parent.kind() {
                "member_expression" if parent.child_by_field_name("object") == Some(node) => {
                    (written(parent) && !statements.contains(&parent.start_byte()))
                        .then_some("object_member_written_elsewhere")
                }
                "subscript_expression" if parent.child_by_field_name("object") == Some(node) => {
                    written(parent).then_some("object_member_written_elsewhere")
                }
                _ => Some("object_escapes"),
            }
        };
        table.dynamic = escape(root, &table.name, source, module, &allowed).map(str::to_owned);
    }
    // A table's own statements define its properties: they are not writes
    // that invalidate member uses of the binding (redefinitions are tracked
    // on the table instead).
    for (_, table) in &tables {
        for binding in module
            .bindings
            .iter_mut()
            .filter(|binding| binding.top_level && binding.name == table.name)
        {
            let visible =
                |offset: usize| binding.scope_start <= offset && offset < binding.scope_end;
            let mut mutated: Vec<String> = module
                .member_writes
                .iter()
                .filter(|write| {
                    write.receiver.as_deref() == Some(binding.name.as_str())
                        && visible(write.offset)
                        && !table.statements.contains(&write.offset)
                })
                .map(|write| write.property.clone().unwrap_or_else(|| "*".into()))
                .collect();
            mutated.sort();
            mutated.dedup();
            binding.mutated_members = mutated;
        }
    }
    module.objects = tables.into_iter().map(|(_, table)| table).collect();
}

/// The fact of the top-level function declaration `name`.
fn function_declaration(module: &Module, file: &IndexedFile, name: &str) -> Option<String> {
    module
        .bindings
        .iter()
        .find(|binding| binding.top_level && binding.name == name && binding.module.is_none())
        .and_then(|binding| binding.target.as_deref())
        .filter(|target| {
            file.facts
                .iter()
                .any(|fact| fact.id == *target && fact.kind == "function")
        })
        .map(str::to_owned)
}

/// Prototype tables of top-level function declarations, from unconditional
/// top-level `F.prototype.name = value` statements. Dynamic when the prototype
/// is replaced or changed elsewhere, `F` escapes (`inherits(F, …)`, aliases),
/// the module manipulates prototypes opaquely, or `this` gains computed own
/// members (`this[k] = …`, `Object.assign(this, …)`).
#[allow(clippy::too_many_lines)] // Statements, then each documented reason a table is dynamic.
fn prototype_tables(root: Node<'_>, source: &str, file: &IndexedFile, module: &mut Module) {
    let mut tables: Vec<ObjectTable> = Vec::new();
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        let Some(assignment) = (statement.kind() == "expression_statement")
            .then(|| statement.named_child(0))
            .flatten()
            .filter(|node| node.kind() == "assignment_expression")
        else {
            continue;
        };
        let (Some(left), Some(value)) = (
            assignment.child_by_field_name("left"),
            assignment.child_by_field_name("right"),
        ) else {
            continue;
        };
        // `F.prototype = …` replaces the prototype: F gets a table so its
        // instances report the replacement instead of a generic failure.
        if let Some((function, member)) = local_member(left, source)
            && member == "prototype"
            && !tables.iter().any(|table| table.name == function)
            && let Some(owner) = function_declaration(module, file, &function)
        {
            tables.push(ObjectTable {
                name: function,
                owner,
                ..ObjectTable::default()
            });
            continue;
        }
        let Some(prototype) = left
            .child_by_field_name("object")
            .filter(|_| left.kind() == "member_expression")
        else {
            continue;
        };
        let Some((function, "prototype")) = local_member(prototype, source)
            .as_ref()
            .map(|(name, member)| (name.clone(), member.as_str()))
        else {
            continue;
        };
        let Some(property) = left
            .child_by_field_name("property")
            .filter(|property| property.kind() == "property_identifier")
            .map(|property| text(property, source).to_owned())
        else {
            continue;
        };
        let Some(owner) = function_declaration(module, file, &function) else {
            continue;
        };
        let index = if let Some(index) = tables.iter().position(|table| table.name == function) {
            index
        } else {
            tables.push(ObjectTable {
                name: function.clone(),
                owner,
                ..ObjectTable::default()
            });
            tables.len() - 1
        };
        let table = &mut tables[index];
        table.statements.push(left.start_byte());
        let mut entry = property_value(property, assignment, value, source, file);
        if value.kind() == "assignment_expression" {
            entry.fact = None;
            entry.local = None;
            entry.member = None;
            entry.module = None;
        }
        add_property(table, entry);
    }
    let mut computed_this = ClassInfo::default();
    dynamic_this(root, source, &mut computed_this);
    let computed_this = computed_this.dynamic_members
        || module
            .member_writes
            .iter()
            .any(|write| write.receiver.as_deref() == Some("this") && write.property.is_none());
    for table in &mut tables {
        let statements = table.statements.clone();
        let reason = if module.prototype_opaque {
            Some("prototype_may_be_modified")
        } else if computed_this {
            Some("dynamic_own_members")
        } else if module
            .prototype_uses
            .iter()
            .any(|used| used.name == table.name && !statements.contains(&used.offset))
        {
            Some("prototype_may_be_modified")
        } else {
            let allowed = |node: Node<'_>| -> Option<&'static str> {
                if exported_value(node, source) {
                    return None;
                }
                let parent = node.parent()?;
                match parent.kind() {
                    // The declaration's own name.
                    "function_declaration" | "generator_function_declaration"
                        if parent.child_by_field_name("name") == Some(node) =>
                    {
                        None
                    }
                    "new_expression" if parent.child_by_field_name("constructor") == Some(node) => {
                        None
                    }
                    "call_expression" if parent.child_by_field_name("function") == Some(node) => {
                        None
                    }
                    // `F.x` reads; `F.prototype…` changes are prototype uses.
                    "member_expression" if parent.child_by_field_name("object") == Some(node) => {
                        None
                    }
                    "binary_expression" => None,
                    _ => Some("constructor_escapes"),
                }
            };
            escape(root, &table.name, source, module, &allowed)
        };
        table.dynamic = reason.map(str::to_owned);
    }
    module.prototypes = tables;
}

#[cfg(test)]
mod invalidation_tests {
    use crate::syntax::extract;

    fn module(path: &str, source: &str) -> code_intel_core::semantic::Module {
        let file = extract(path, source, String::new()).unwrap();
        assert!(file.semantic.complete, "{path}");
        file.semantic
    }

    fn binding<'a>(
        module: &'a code_intel_core::semantic::Module,
        name: &str,
    ) -> &'a code_intel_core::semantic::Binding {
        module
            .bindings
            .iter()
            .find(|binding| binding.name == name)
            .unwrap_or_else(|| panic!("no binding {name}"))
    }

    #[test]
    fn integer_and_symbol_keys_are_classified_conservatively() {
        assert_eq!(super::canonical_integer("0").as_deref(), Some("0"));
        assert_eq!(super::canonical_integer("42").as_deref(), Some("42"));
        for text in ["01", "1.5", "1e3", "0x10", "1_000", "9007199254740993", ""] {
            assert_eq!(super::canonical_integer(text), None, "{text}");
        }
        let module = module(
            "a.js",
            "const x = {};\nx[0] = 1;\nx[1.5] = 2;\nx['\\u0061'] = 3;\n",
        );
        assert_eq!(binding(&module, "x").mutated_members, vec!["*", "0"]);
        let module = module_with_classes(
            "class A { constructor() { this[Symbol.iterator] = 1; } }\n\
             class B { constructor() { this[Symbol.for('k')] = 1; } }\n\
             const k = Symbol();\nclass C { constructor() { this[k] = 1; } }\n\
             let j = Symbol();\nj = 'x';\nclass D { constructor() { this[j] = 1; } }\n\
             class E { constructor(n) { this[Symbol.for(n)] = 1; } }\n\
             class F { constructor() { return new Proxy(this, {}); } }\n\
             class G { constructor() { const f = () => { return 1; }; return this; } }\n",
        );
        let dynamic: Vec<bool> = module.classes.iter().map(|c| c.dynamic_members).collect();
        assert_eq!(dynamic, vec![false, false, false, true, true, false, false]);
        let returns: Vec<bool> = module
            .classes
            .iter()
            .map(|c| c.constructor_returns)
            .collect();
        assert_eq!(
            returns,
            vec![false, false, false, false, false, true, false]
        );
        // A shadowed `Symbol` is any object.
        let shadowed = module_with_classes(
            "const Symbol = { iterator: 'm' };\nclass A { constructor() { this[Symbol.iterator] = 1; } m() {} }\n",
        );
        assert!(shadowed.classes[0].dynamic_members);
        // `util.inspect.custom` is left to the binder only for util bindings.
        let util = module_with_classes(
            "const util = require('node:util');\nclass A { constructor() { this[util.inspect.custom] = 1; } }\n\
             const fake = make();\nclass B { constructor() { this[fake.inspect.custom] = 1; } }\n",
        );
        assert!(!util.classes[0].dynamic_members);
        assert_eq!(
            util.classes[0].builtin_symbol_keys[0].specifier,
            "node:util"
        );
        assert!(util.classes[1].dynamic_members);
    }

    fn module_with_classes(source: &str) -> code_intel_core::semantic::Module {
        let module = module("classes.js", source);
        assert!(!module.classes.is_empty());
        module
    }

    #[test]
    fn computed_keys_and_defaults_are_reads_not_writes() {
        for path in ["a.js", "a.ts"] {
            let module = module(
                path,
                "function f() { return 'k'; }\nfunction g() { return 1; }\nconst o = {};\n\
                 let a, k;\no[f()] = 1;\no[k]++;\n({ a = g() } = o);\n",
            );
            assert!(!binding(&module, "f").mutated, "{path}: key call is a read");
            assert!(
                !binding(&module, "k").mutated,
                "{path}: key identifier is a read"
            );
            assert!(
                !binding(&module, "g").mutated,
                "{path}: default value is a read"
            );
            assert!(
                binding(&module, "a").mutated,
                "{path}: pattern target is written"
            );
            let o = binding(&module, "o");
            assert!(!o.mutated, "{path}: writing a member does not rebind o");
            assert_eq!(o.mutated_members, ["*"], "{path}");
        }
    }

    #[test]
    fn member_writes_invalidate_only_that_member() {
        let module = module(
            "a.js",
            "class C { static m() {} static n() {} }\nC.m = () => 0;\nC.n.extra = 1;\n\
             let f = () => 1;\nf = () => 2;\nconst x = {};\n[x.y] = [1];\n",
        );
        let c = binding(&module, "C");
        assert!(!c.mutated);
        // `C.n.extra = 1` mutates the object C.n, not the C.n binding.
        assert_eq!(c.mutated_members, ["m"]);
        assert!(binding(&module, "f").mutated, "rebinding still invalidates");
        assert_eq!(binding(&module, "x").mutated_members, ["y"]);
    }

    #[test]
    fn this_bind_pattern_and_fields_are_recorded_per_class() {
        let module = module(
            "a.js",
            "export class A {\n  f = 1;\n  static s = 2;\n  'q'() {}\n  [Symbol.iterator]() {}\n\
             constructor() { this.g = 1; this.h = this.h.bind(this); }\n  h() {}\n}\n\
             export class B { constructor(k) { this[k] = 1; } }\n",
        );
        let a = &module.classes[0];
        assert_eq!(a.instance_fields, ["f", "g", "q"]);
        assert_eq!(a.static_fields, ["g", "s"]);
        assert!(!a.dynamic_members);
        assert!(module.classes[1].dynamic_members);
    }

    #[test]
    fn require_is_commonjs_only_where_node_would_load_it() {
        let cjs = module(
            "a.cjs",
            "const { a, b: c, d = 1 } = require('./x');\nconst y = require('./y').z;\n",
        );
        for (name, imported) in [("a", "a"), ("c", "b"), ("y", "z")] {
            let binding = binding(&cjs, name);
            assert!(binding.commonjs, "{name}");
            assert_eq!(binding.imported, imported);
        }
        assert!(!binding(&cjs, "d").commonjs, "defaults stay locals");
        for (path, source) in [
            ("a.mjs", "const x = require('./x');\n"),
            ("a.js", "import 'y';\nconst x = require('./x');\n"),
            ("a.ts", "const x = require('./x');\n"),
            ("a.js", "function require() {}\nconst x = require('./x');\n"),
            ("a.js", "const x = require(`./x`);\n"),
        ] {
            let module = module(path, source);
            assert!(!binding(&module, "x").commonjs, "{path}: {source}");
            assert!(binding(&module, "x").module.is_none(), "{path}: {source}");
        }
    }

    #[test]
    fn export_tables_follow_node_semantics() {
        let names = |source: &str| {
            let module = module("a.js", source);
            let table = module.commonjs.expect("commonjs table");
            (
                table
                    .exports
                    .iter()
                    .map(|export| export.name.clone())
                    .collect::<Vec<_>>(),
                table.dynamic,
            )
        };
        assert_eq!(
            names("function x() {}\nexports.x = x;\nmodule.exports = { y: x };\nexports.z = x;\n"),
            (vec!["y".to_owned()], None)
        );
        assert_eq!(
            names("function x() {}\nexports = {};\nexports.x = x;\nmodule.exports.w = x;\n"),
            (vec!["w".to_owned()], None)
        );
        for dynamic in [
            "function x() {}\nif (a) { exports.x = x; }\n",
            "function x() {}\nmodule.exports = { ...x };\n",
            "function x() {}\nexports[x()] = x;\n",
            "Object.assign(exports, {});\n",
            "factory(module.exports);\n",
            "module.exports = exports = {};\n",
            "function f(exports) { exports.x = 1; }\nexports.y = 1;\n",
        ] {
            assert!(names(dynamic).1.is_some(), "{dynamic}");
        }
        assert_eq!(
            names(
                "Object.defineProperty(exports, '__esModule', { value: true });\nexports.a = 1;\n"
            )
            .1,
            None
        );
    }
}
