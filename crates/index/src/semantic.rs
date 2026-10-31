//! Conservative AST binding IR. Never scans comments or string contents for names.
use code_intel_core::{
    IndexedFile, file_id,
    semantic::{Binding, Export, Location, Module, Relationship, Symbol, Usage},
};
use tree_sitter::Node;

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
        });
    }
    bindings(root, source, file, &mut module);
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
    // Reassignment anywhere in this file invalidates the enclosing binding. This deliberately
    // over-invalidates shadowed writes rather than claiming a mutable value is a known callee.
    let mut writes = Vec::new();
    mutations(root, source, &mut writes);
    for binding in &mut module.bindings {
        binding.mutated = writes.iter().any(|(name, offset)| {
            name == &binding.name && binding.scope_start <= *offset && *offset < binding.scope_end
        });
    }
    module
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
        bind(
            name,
            visible,
            source,
            file,
            module,
            fact.map(|fact| fact.id.clone()),
        );
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
    let (name, member) = if expression.kind() == "member_expression" {
        let object = expression.child_by_field_name("object");
        let property = expression.child_by_field_name("property");
        if let (Some(object), Some(property)) = (object, property) {
            if matches!(object.kind(), "identifier" | "this") {
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
    module.uses.push(Usage {
        syntax: text(expression, source).into(),
        source_id: owner(node, file),
        name,
        member,
        kind,
        location: location(expression, &file.path),
        jsx: false,
    });
}
/// The receiver of a member expression: nested objects are visited as code; an
/// identifier is a use only when it names a non-namespace binding.
fn receiver(member: Node<'_>, source: &str, file: &IndexedFile, module: &mut Module) {
    // The object identifier alone is not a use of the selected namespace member.
    if let Some(object) = member.child_by_field_name("object") {
        if object.kind() != "identifier" {
            uses(object, source, file, module);
        } else if module
            .bindings
            .iter()
            .any(|binding| binding.name == text(object, source) && binding.imported != "*")
        {
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
fn mutations(node: Node<'_>, source: &str, writes: &mut Vec<(String, usize)>) {
    if node.kind() == "for_in_statement"
        && node.child_by_field_name("kind").is_none()
        && let Some(left) = node.child_by_field_name("left")
    {
        assigned_names(left, source, writes);
    }
    if matches!(
        node.kind(),
        "assignment_expression" | "augmented_assignment_expression" | "update_expression"
    ) && let Some(left) = node
        .child_by_field_name("left")
        .or_else(|| node.child_by_field_name("argument"))
    {
        assigned_names(left, source, writes);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        mutations(child, source, writes);
    }
}

fn assigned_names(node: Node<'_>, source: &str, writes: &mut Vec<(String, usize)>) {
    if matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern"
    ) {
        writes.push((text(node, source).into(), node.start_byte()));
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        // Conservative over-invalidation also handles computed/member writes and destructuring.
        assigned_names(child, source, writes);
    }
}
