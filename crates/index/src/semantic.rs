//! Conservative AST binding IR. Never scans comments or string contents for names.
use code_intel_core::{
    IndexedFile, file_id,
    language::Language,
    semantic::{
        Binding, ClassInfo, CommonJs, CommonJsExport, Export, Location, MemberContext, MemberWrite,
        Module, NameAt, Relationship, Symbol, Usage,
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
    classes(root, source, file, &members, &mut module);
    module.member_writes = members;
    prototypes(root, source, &mut module);
    if commonjs_file {
        commonjs(root, source, file, &writes, &mut module);
    }
    module
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
        location: location(site, &file.path),
    };
    if matches!(value.kind(), "identifier" | "shorthand_property_identifier") {
        export.local = Some(text(value, source).into());
    } else if let Some((specifier, member)) = require_call(value, source) {
        export.module = Some(specifier);
        export.member = member;
    }
    export
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
    let dynamic = |reason: &str, table: &mut CommonJs| {
        if table.dynamic.is_none() {
            table.dynamic = Some(reason.into());
        }
    };
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        let Some(assignment) = (statement.kind() == "expression_statement")
            .then(|| statement.named_child(0))
            .flatten()
            .filter(|node| node.kind() == "assignment_expression")
        else {
            continue;
        };
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
            alias_live = false;
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
    // Writes to the export objects anywhere else (nested, conditional, aliased).
    if writes.iter().any(|(name, offset)| {
        matches!(name.as_str(), "module" | "exports") && !handled.contains(offset)
    }) || export_writes(root, source, &handled)
    {
        seen = true;
        dynamic("nested_or_conditional_export_write", &mut table);
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

/// A write to `module.exports`, `exports` or one of their members that is not
/// one of the `handled` top-level statements.
fn export_writes(node: Node<'_>, source: &str, handled: &std::collections::HashSet<usize>) -> bool {
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
        if target(left) || member {
            return true;
        }
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    children
        .into_iter()
        .any(|child| export_writes(child, source, handled))
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
fn classes(
    root: Node<'_>,
    source: &str,
    file: &IndexedFile,
    writes: &[MemberWrite],
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
                    // `[Symbol.iterator]()` cannot collide with an identifier name.
                    "computed_property_name"
                        if name.named_child(0).is_some_and(|key| {
                            key.kind() == "member_expression"
                                && key
                                    .child_by_field_name("object")
                                    .is_some_and(|object| text(object, source) == "Symbol")
                        }) =>
                    {
                        None
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
        }
        for write in writes.iter().filter(|write| {
            write.receiver.as_deref() == Some("this")
                && fact.start_byte <= write.offset
                && write.offset < fact.end_byte
        }) {
            match &write.property {
                // Written in a static or instance member: list it as both.
                Some(property) => {
                    info.instance_fields.push(property.clone());
                    info.static_fields.push(property.clone());
                }
                None => info.dynamic_members = true,
            }
        }
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
                    .filter(|index| index.kind() == "string")
                    .map(|index| literal(index, source))
            };
            members.push(MemberWrite {
                receiver: object
                    .filter(|object| matches!(object.kind(), "identifier" | "this"))
                    .map(|object| text(object, source).to_owned()),
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
