use anyhow::{Context, Result};
use code_intel_core::{Edge, Fact, IndexedFile, Provenance, file_id};
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
                if child.kind() == "variable_declarator" {
                    if let Some(name) = child
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
    if matches!(kind, "class" | "interface") {
        if let Some(body) = node.child_by_field_name("body") {
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
