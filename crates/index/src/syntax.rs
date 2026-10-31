use anyhow::{Context, Result};
use code_intel_core::{Edge, Fact, IndexedFile, Provenance, Signature, file_id};
use tree_sitter::{Node, Parser};

pub fn extract(path: &str, source: &str, hash: String) -> Result<IndexedFile> {
    let mut parser = Parser::new();
    let tsx = std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext == "tsx");
    let language = if tsx {
        tree_sitter_typescript::LANGUAGE_TSX
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT
    };
    parser
        .set_language(&language.into())
        .context("load bundled TypeScript grammar")?;
    let tree = parser
        .parse(source, None)
        .context("Tree-sitter parsing was cancelled; retry indexing")?;
    let root = tree.root_node();
    let mut file = IndexedFile {
        path: path.into(),
        language: if tsx { "tsx" } else { "typescript" }.into(),
        hash,
        size_bytes: u64::try_from(source.len())?,
        syntax_error: root.has_error(),
        facts: Vec::new(),
        edges: Vec::new(),
        semantic: code_intel_core::semantic::Module::default(),
    };
    let preview = source.split_inclusive('\n').take(12).collect::<String>();
    let end_line = preview.lines().count().max(1);
    file.facts.push(Fact {
        id: file_id(path),
        kind: "file".into(),
        name: path.into(),
        path: path.into(),
        start_byte: 0,
        end_byte: preview.len(),
        start_line: 1,
        end_line,
        snippet: preview,
        signature: None,
        provenance: Provenance {
            source_path: path.into(),
            start_line: 1,
            end_line,
            extraction_method: "tree_sitter_file_preview".into(),
            confidence: if root.has_error() {
                "recovered_syntax"
            } else {
                "syntax"
            }
            .into(),
        },
    });
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        if node.has_error() || node.is_missing() {
            continue;
        }
        match node.kind() {
            "import_statement" => {
                let id = add_fact(&mut file, node, "import", text(node, source), source);
                module_edge(&mut file, node, &id, source);
            }
            "export_statement" => {
                let id = add_fact(&mut file, node, "export", "export", source);
                module_edge(&mut file, node, &id, source);
                if let Some(declaration) = node.child_by_field_name("declaration") {
                    for symbol in declaration_facts(&mut file, declaration, source) {
                        let source_id = file_id(path);
                        add_edge(&mut file, node, source_id, Some(symbol), None, "exports");
                    }
                }
                // Export clauses remain syntax facts; imported/local names are not semantically resolved.
            }
            _ => {
                declaration_facts(&mut file, node, source);
            }
        }
    }
    file.semantic = crate::semantic::extract(root, source, &file);
    Ok(file)
}

fn declaration_facts(file: &mut IndexedFile, node: Node<'_>, source: &str) -> Vec<String> {
    let kind = match node.kind() {
        "function_declaration" | "generator_function_declaration" | "function_signature" => {
            "function"
        }
        "class_declaration" | "abstract_class_declaration" => "class",
        "interface_declaration" => "interface",
        "type_alias_declaration" => "type_alias",
        "enum_declaration" => "enum",
        "lexical_declaration" | "variable_declaration" => {
            let mut ids = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "variable_declarator"
                    && let Some(name) = child
                        .child_by_field_name("name")
                        .filter(|name| name.kind() == "identifier")
                {
                    ids.push(add_fact(
                        file,
                        child,
                        "variable",
                        text(name, source),
                        source,
                    ));
                }
            }
            return ids;
        }
        _ => return Vec::new(),
    };
    let Some(name) = node.child_by_field_name("name") else {
        return Vec::new();
    };
    let name = text(name, source);
    let id = add_fact(file, node, kind, name, source);
    if matches!(kind, "class" | "interface")
        && let Some(body) = node.child_by_field_name("body")
    {
        let mut cursor = body.walk();
        for method in body.named_children(&mut cursor) {
            if method.has_error()
                || !matches!(
                    method.kind(),
                    "method_definition" | "method_signature" | "abstract_method_signature"
                )
            {
                continue;
            }
            if let Some(method_name) = method.child_by_field_name("name").filter(|name| {
                matches!(
                    name.kind(),
                    "property_identifier" | "private_property_identifier"
                )
            }) {
                let qualified = format!("{name}.{}", text(method_name, source));
                let method_id = add_fact(file, method, "method", &qualified, source);
                add_edge(file, method, id.clone(), Some(method_id), None, "contains");
            }
        }
    }
    vec![id]
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

fn provenance(path: &str, node: Node<'_>) -> Provenance {
    // Tree-sitter's end point is exclusive; a node ending at column 0 excludes that line.
    let end_line = node.end_position().row + usize::from(node.end_position().column > 0);
    Provenance {
        source_path: path.into(),
        start_line: node.start_position().row + 1,
        end_line: end_line.max(node.start_position().row + 1),
        extraction_method: "tree_sitter_typescript".into(),
        confidence: "syntax".into(),
    }
}

fn add_fact(
    file: &mut IndexedFile,
    node: Node<'_>,
    kind: &str,
    name: &str,
    source: &str,
) -> String {
    let id = format!(
        "{kind}:{}:{}:{}",
        file.path,
        node.start_byte(),
        node.end_byte()
    );
    let evidence = provenance(&file.path, node);
    file.facts.push(Fact {
        id: id.clone(),
        kind: kind.into(),
        name: name.into(),
        path: file.path.clone(),
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: evidence.start_line,
        end_line: evidence.end_line,
        snippet: text(node, source).into(),
        signature: node.child_by_field_name("body").and_then(|body| {
            let header = source[node.start_byte()..body.start_byte()].trim_end();
            (!header.is_empty()).then(|| Signature {
                snippet: header.into(),
                end_line: node.start_position().row + header.lines().count(),
            })
        }),
        provenance: evidence,
    });
    add_edge(
        file,
        node,
        file_id(&file.path),
        Some(id.clone()),
        None,
        "contains",
    );
    id
}

fn add_edge(
    file: &mut IndexedFile,
    node: Node<'_>,
    source_id: String,
    target_id: Option<String>,
    target_text: Option<String>,
    kind: &str,
) {
    file.edges.push(Edge {
        resolution: None,
        evidence: None,
        location: None,
        source_id,
        target_id,
        target_text,
        kind: kind.into(),
        provenance: provenance(&file.path, node),
    });
}

fn module_edge(file: &mut IndexedFile, node: Node<'_>, source_id: &str, source: &str) {
    if let Some(module) = node.child_by_field_name("source") {
        // Retain literal spelling, without pretending to interpret JS string escapes.
        let literal = text(module, source);
        let module = literal
            .get(1..literal.len().saturating_sub(1))
            .unwrap_or_default();
        add_edge(
            file,
            node,
            source_id.into(),
            None,
            Some(module.into()),
            "imports",
        );
    }
}

/// The header (text before the body) of a variable declarator whose value is a
/// function: `f = (a: A) => …`, `C = memo(function C(p) …)`, `R = forwardRef((p, r) => …)`.
/// `None` when the value is not function-like or does not parse. Used by change
/// impact to tell signature changes from body changes; never stored.
#[must_use]
pub fn variable_header(path: &str, declarator: &str) -> Option<String> {
    const PREFIX: &str = "const ";
    let tsx = std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext == "tsx");
    let language = if tsx {
        tree_sitter_typescript::LANGUAGE_TSX
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT
    };
    let mut parser = Parser::new();
    parser.set_language(&language.into()).ok()?;
    let source = format!("{PREFIX}{declarator};");
    let tree = parser.parse(&source, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let declaration = root.named_child(0)?;
    let mut cursor = declaration.walk();
    let value = declaration
        .named_children(&mut cursor)
        .find(|child| child.kind() == "variable_declarator")?
        .child_by_field_name("value")?;
    let body = function_body(value, 0)?;
    let header = source.get(PREFIX.len()..body.start_byte())?.trim_end();
    (!header.is_empty()).then(|| header.to_owned())
}

/// The body of the first function-like value, looking through calls (HOCs),
/// parentheses and type assertions, at most four levels deep.
fn function_body(node: Node<'_>, depth: usize) -> Option<Node<'_>> {
    if depth > 4 {
        return None;
    }
    match node.kind() {
        "arrow_function" | "function_expression" | "function" | "generator_function" => {
            node.child_by_field_name("body")
        }
        // The binding outlives `cursor`'s borrow; returning the expression directly does not compile.
        #[allow(clippy::let_and_return)]
        "call_expression" => {
            let arguments = node.child_by_field_name("arguments")?;
            let mut cursor = arguments.walk();
            let found = arguments
                .named_children(&mut cursor)
                .find_map(|argument| function_body(argument, depth + 1));
            found
        }
        "parenthesized_expression"
        | "as_expression"
        | "satisfies_expression"
        | "non_null_expression" => node
            .named_child(0)
            .and_then(|inner| function_body(inner, depth + 1)),
        _ => None,
    }
}

/// Top-level code of a module that declaration, import and export-statement
/// comparisons do not cover: statements, and declaration keywords
/// (`const`/`let`/`var`, with declarator text removed). Comments are ignored.
/// `None` when the file does not parse cleanly.
#[must_use]
pub fn module_residue(path: &str, text: &str) -> Option<Vec<String>> {
    let tsx = std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext == "tsx");
    let language = if tsx {
        tree_sitter_typescript::LANGUAGE_TSX
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT
    };
    let mut parser = Parser::new();
    parser.set_language(&language.into()).ok()?;
    let tree = parser.parse(text, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut items = Vec::new();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        let declaration = if node.kind() == "export_statement" {
            match node.child_by_field_name("declaration") {
                Some(declaration) => declaration,
                // Export clauses and re-exports are compared as export statements.
                None => continue,
            }
        } else {
            node
        };
        match declaration.kind() {
            "comment"
            | "import_statement"
            | "function_declaration"
            | "generator_function_declaration"
            | "function_signature"
            | "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration" => {}
            "lexical_declaration" | "variable_declaration" => {
                let mut shape = String::new();
                let mut position = declaration.start_byte();
                let mut inner = declaration.walk();
                for child in declaration.named_children(&mut inner) {
                    if child.kind() == "variable_declarator" {
                        shape.push_str(&text[position..child.start_byte()]);
                        shape.push_str("<declarator>");
                        position = child.end_byte();
                    }
                }
                shape.push_str(&text[position..declaration.end_byte()]);
                items.push(shape.split_whitespace().collect::<Vec<_>>().join(" "));
            }
            _ => items.push(
                text[node.byte_range()]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        }
    }
    Some(items)
}

#[cfg(test)]
mod grammar_gap_tests {
    use super::*;

    #[test]
    fn the_original_tree_does_not_supply_a_type_argument_recovery_boundary() {
        for language in [
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            tree_sitter_typescript::LANGUAGE_TSX,
        ] {
            let mut parser = Parser::new();
            parser.set_language(&language.into()).unwrap();
            let tree = parser
                .parse(
                    include_str!("../../../fixtures/typescript-grammar/call-type.ts"),
                    None,
                )
                .unwrap();
            let root = tree.root_node();
            let shape = root.to_sexp();
            assert!(root.has_error());
            assert!(shape.contains("(binary_expression"), "{shape}");
            assert!(shape.contains("(ERROR"), "{shape}");
            assert!(!shape.contains("(type_arguments"), "{shape}");
        }
    }

    // TypeScript 6.0.3 accepts these fixtures (checked separately by the
    // compiler oracle). The bundled grammar loses the generic-call boundary.
    // A future recovery must replace these expectations with proven bindings,
    // while keeping malformed/shadowed/reassigned controls conservative.
    #[test]
    fn import_types_in_call_arguments_remain_explicitly_partial() {
        for (path, source) in [
            (
                "call-type.ts",
                include_str!("../../../fixtures/typescript-grammar/call-type.ts"),
            ),
            (
                "call-type.tsx",
                include_str!("../../../fixtures/typescript-grammar/call-type.tsx"),
            ),
            (
                "import-type.ts",
                include_str!("../../../fixtures/typescript-grammar/import-type.ts"),
            ),
            (
                "import-type.tsx",
                include_str!("../../../fixtures/typescript-grammar/import-type.tsx"),
            ),
            (
                "import-member.ts",
                include_str!("../../../fixtures/typescript-grammar/import-member.ts"),
            ),
            (
                "import-member.tsx",
                include_str!("../../../fixtures/typescript-grammar/import-member.tsx"),
            ),
            (
                "shadowed.ts",
                include_str!("../../../fixtures/typescript-grammar/shadowed.ts"),
            ),
            (
                "reassigned.ts",
                include_str!("../../../fixtures/typescript-grammar/reassigned.ts"),
            ),
            (
                "malformed.ts",
                include_str!("../../../fixtures/typescript-grammar/malformed.ts"),
            ),
        ] {
            let file = extract(path, source, String::new()).unwrap();
            assert!(file.syntax_error, "{path}");
            assert!(!file.semantic.complete, "{path}");
            assert!(file.semantic.uses.is_empty(), "{path}");
            assert!(file.semantic.bindings.is_empty(), "{path}");
            // Intact lexical evidence is still available, but cannot establish
            // confirmed relationships from a partially parsed module.
            if !path.starts_with("call-type.") {
                assert!(file.facts.iter().any(|f| f.kind == "import"), "{path}");
            }
            assert_eq!(file.facts[0].provenance.confidence, "recovered_syntax");
            assert!(module_residue(path, source).is_none());
        }
    }

    #[test]
    fn strings_comments_and_ordinary_generics_do_not_trigger_the_gap() {
        for (path, source) in [
            (
                "clean.ts",
                include_str!("../../../fixtures/typescript-grammar/clean.ts"),
            ),
            (
                "clean.tsx",
                include_str!("../../../fixtures/typescript-grammar/clean.tsx"),
            ),
        ] {
            let file = extract(path, source, String::new()).unwrap();
            assert!(!file.syntax_error, "{path}");
            assert!(file.semantic.complete, "{path}");
            assert!(!file.semantic.uses.is_empty(), "{path}");
        }
    }
}
