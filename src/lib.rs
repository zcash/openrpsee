//! Utilities for generating OpenRPC documents.
//!
//! This crate provides utilities to generate OpenRPC documents
//! in projects that uses the `jsonrpsee` crate for JSON-RPC method definitions.

use std::{error::Error, fs, path::Path};

use quote::ToTokens;

pub mod openrpc;

pub use phf;

/// Generates a lookup table for the JSON-RPC methods defined in the given source file.
///
/// This function is meant to be used in the build script (`build.rs`) of a project.
pub fn generate_openrpc(
    json_rpc_methods_rs: &str,
    trait_names: &[&str],
    use_parent_module: bool,
    out_dir: &Path,
) -> Result<(), Box<dyn Error>> {
    generate(
        json_rpc_methods_rs,
        trait_names,
        use_parent_module,
        false,
        out_dir,
    )
}

/// Like [`generate_openrpc`] without `use_parent_module`, but takes each parameter's description,
/// `default` and `example` from a `# Parameters` section in the method's doc comment instead of
/// `PARAM_*` constants, and leaves that section out of the method description.
///
/// Each parameter needs a line like
/// ``- `verbosity`: (number, optional, default=1, example=1) 0 for hex, 1 for a JSON object.``.
/// `required` or `optional` must match the Rust type. Every problem in every method is returned
/// in one error.
pub fn generate_openrpc_with_doc_params(
    json_rpc_methods_rs: &str,
    trait_names: &[&str],
    out_dir: &Path,
) -> Result<(), Box<dyn Error>> {
    generate(json_rpc_methods_rs, trait_names, false, true, out_dir)
}

fn generate(
    json_rpc_methods_rs: &str,
    trait_names: &[&str],
    use_parent_module: bool,
    doc_params: bool,
    out_dir: &Path,
) -> Result<(), Box<dyn Error>> {
    // Parse the source file containing the requested traits.
    let methods_rs = fs::read_to_string(json_rpc_methods_rs)?;
    let methods_ast = syn::parse_file(&methods_rs)?;

    // Collect references to all requested traits.
    let traits: Vec<&syn::ItemTrait> = trait_names
        .iter()
        .map(|name| {
            methods_ast
                .items
                .iter()
                .find_map(|item| match item {
                    syn::Item::Trait(item_trait) if item_trait.ident == *name => Some(item_trait),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    panic!("trait `{name}` must be present in {json_rpc_methods_rs}")
                })
        })
        .collect();

    // `phf_map!` expands to a relative `phf::Map`, so bring the re-export into scope.
    let mut contents = "/// Lookup table for JSON-RPC methods.
#[allow(unused_qualifications)]
pub(crate) static METHODS: ::openrpsee::phf::Map<&str, ::openrpsee::openrpc::RpcMethod> = {
use ::openrpsee::phf;
phf::phf_map! {
"
    .to_string();

    let mut errors = vec![];
    for item in traits.iter().flat_map(|tr| tr.items.iter()) {
        if let syn::TraitItem::Fn(method) = item {
            // Find methods via their `#[method(name = "command")]` attribute.
            let mut command = None;
            method
                .attrs
                .iter()
                .find(|attr| attr.path().is_ident("method"))
                .and_then(|attr| {
                    attr.parse_nested_meta(|meta| {
                        command = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                        Ok(())
                    })
                    .ok()
                });

            if let Some(command) = command {
                let module = match &method.sig.output {
                    syn::ReturnType::Type(_, ret) => match ret.as_ref() {
                        syn::Type::Path(type_path) => type_path.path.segments.first(),
                        _ => None,
                    },
                    _ => None,
                }
                .expect("required")
                .ident
                .to_string();

                let params: Vec<_> = method
                    .sig
                    .inputs
                    .iter()
                    .filter_map(|arg| match arg {
                        syn::FnArg::Receiver(_) => None,
                        syn::FnArg::Typed(pat_type) => match pat_type.pat.as_ref() {
                            syn::Pat::Ident(pat_ident) => {
                                let parameter = pat_ident.ident.to_string();
                                let rust_ty = pat_type.ty.as_ref();

                                // If we can determine the parameter's optionality, do so.
                                let (param_ty, required) = match rust_ty {
                                    syn::Type::Path(type_path) => {
                                        let is_standalone_ident =
                                            type_path.path.leading_colon.is_none()
                                                && type_path.path.segments.len() == 1;
                                        let first_segment = &type_path.path.segments[0];

                                        if first_segment.ident == "Option" && is_standalone_ident {
                                            // Strip the `Option<_>` for the schema type.
                                            let schema_ty = match &first_segment.arguments {
                                                syn::PathArguments::AngleBracketed(args) => {
                                                    match args.args.first().expect("valid Option") {
                                                        syn::GenericArgument::Type(ty) => ty,
                                                        _ => panic!("Invalid Option"),
                                                    }
                                                }
                                                _ => panic!("Invalid Option"),
                                            };
                                            (schema_ty, Some(false))
                                        } else if first_segment.ident == "Vec" {
                                            // We don't know whether the vec may be empty.
                                            (rust_ty, None)
                                        } else {
                                            (rust_ty, Some(true))
                                        }
                                    }
                                    _ => (rust_ty, Some(true)),
                                };

                                // Handle a few conversions we know we need.
                                let param_ty = param_ty.to_token_stream().to_string();
                                let schema_ty = match param_ty.as_str() {
                                    "age :: secrecy :: SecretString" => "String".into(),
                                    _ => param_ty,
                                };

                                Some((parameter, schema_ty, required))
                            }
                            _ => None,
                        },
                    })
                    .collect();

                contents.push('"');
                contents.push_str(&command);
                contents.push_str("\" => ::openrpsee::openrpc::RpcMethod {\n");

                let mut doc_lines = vec![];
                for attr in method
                    .attrs
                    .iter()
                    .filter(|attr| attr.path().is_ident("doc"))
                {
                    if let syn::Meta::NameValue(doc_line) = &attr.meta
                        && let syn::Expr::Lit(docs) = &doc_line.value
                        && let syn::Lit::Str(s) = &docs.lit
                    {
                        // Trim the leading space from the doc comment line.
                        let line = s.value();
                        doc_lines.push(if line.is_empty() {
                            line
                        } else {
                            line[1..].to_string()
                        });
                    }
                }
                let documented = doc_params.then(|| split_parameters_section(&mut doc_lines));

                let mut tags = vec![];
                contents.push_str("    description: \"");
                for trimmed_line in &doc_lines {
                    // A `tags: a, b` line becomes the method's OpenRPC tags.
                    if let Some(line_tags) = trimmed_line.strip_prefix("tags:") {
                        tags.extend(
                            line_tags
                                .split(',')
                                .map(str::trim)
                                .filter(|tag| !tag.is_empty())
                                .map(str::to_string),
                        );
                        continue;
                    }

                    let escaped = strip_intra_doc_links(trimmed_line)
                        .escape_default()
                        .collect::<String>();

                    contents.push_str(&escaped);
                    contents.push_str("\\n");
                }
                contents.push_str("\",\n");

                contents.push_str("    tags: &[");
                for tag in &tags {
                    contents.push('"');
                    contents.push_str(&tag.escape_default().collect::<String>());
                    contents.push_str("\", ");
                }
                contents.push_str("],\n");

                contents.push_str("    params: |_g| vec![\n");
                if let Some(documented) = &documented {
                    contents.push_str(&documented_params(
                        &command,
                        &params,
                        documented,
                        &mut errors,
                    ));
                }
                for (parameter, schema_ty, required) in params.iter().filter(|_| !doc_params) {
                    let required = *required;
                    let param_upper = parameter.to_uppercase();

                    contents.push_str("        _g.param::<");
                    contents.push_str(schema_ty);
                    contents.push_str(">(\"");
                    contents.push_str(parameter);
                    if use_parent_module {
                        contents.push_str("\", super::");
                        contents.push_str(&module);
                    } else {
                        contents.push_str("\", crate::methods");
                    }
                    contents.push_str("::PARAM_");
                    contents.push_str(&param_upper);
                    contents.push_str("_DESC, ");
                    match required {
                        Some(required) => contents.push_str(&required.to_string()),
                        None => {
                            // Require a helper const to be present.
                            if use_parent_module {
                                contents.push_str("super::");
                            } else {
                                contents.push_str("crate::methods::");
                            }
                            contents.push_str(&module);
                            contents.push_str("::PARAM_");
                            contents.push_str(&param_upper);
                            contents.push_str("_REQUIRED");
                        }
                    }
                    contents.push_str("),\n");
                }
                contents.push_str("    ],\n");

                contents.push_str("    result: |g| ");
                contents.push_str(&result_call(
                    &method.sig.output,
                    &module,
                    use_parent_module,
                    &command,
                )?);
                contents.push_str(",\n");

                contents.push_str("    deprecated: ");
                contents.push_str(
                    &method
                        .attrs
                        .iter()
                        .any(|attr| attr.path().is_ident("deprecated"))
                        .to_string(),
                );
                contents.push_str(",\n");

                contents.push_str("},\n");
            }
        }
    }

    if !errors.is_empty() {
        return Err(format!(
            "{} problems in `# Parameters` doc sections:\n  {}",
            errors.len(),
            errors.join("\n  ")
        )
        .into());
    }

    contents.push_str("}\n};");

    let rpc_openrpc_path = out_dir.join("rpc_openrpc.rs");
    fs::write(&rpc_openrpc_path, contents)?;

    Ok(())
}

/// Returns the generated call that builds the result descriptor for `command`.
///
/// With `use_parent_module`, each method's module defines a `ResultType`. Otherwise the result
/// is the `T` in a `Result<T, ..>` or `RpcResult<T>` return type, or the `ResultType` next to a
/// `path::Response` return type.
fn result_call(
    output: &syn::ReturnType,
    module: &str,
    use_parent_module: bool,
    command: &str,
) -> Result<String, Box<dyn Error>> {
    if use_parent_module {
        return Ok(format!(
            "g.result::<super::{module}::ResultType>(\"{command}_result\")"
        ));
    }

    let path = match output {
        syn::ReturnType::Type(_, ty) => match ty.as_ref() {
            syn::Type::Path(type_path) => Some(&type_path.path),
            _ => None,
        },
        syn::ReturnType::Default => None,
    };
    let last = path.and_then(|path| path.segments.last());

    if let Some(last) = last {
        if (last.ident == "Result" || last.ident == "RpcResult")
            && let syn::PathArguments::AngleBracketed(args) = &last.arguments
            && let Some(syn::GenericArgument::Type(ty)) = args.args.first()
        {
            let ty = ty.to_token_stream().to_string();
            return Ok(format!(
                "g.result_schema::<{ty}>(\"{command}_result\", \"\")"
            ));
        }

        if last.ident == "Response" && last.arguments.is_empty() {
            let mut path = path.expect("last segment exists").clone();
            path.segments.last_mut().expect("last segment exists").ident =
                syn::Ident::new("ResultType", last.ident.span());
            let ty = path.to_token_stream().to_string();
            return Ok(format!("g.result::<{ty}>(\"{command}_result\")"));
        }
    }

    Err(format!(
        "can't find the result type of `{command}`: return `Result<T, E>`, `RpcResult<T>` \
         or a `Response` alias with a `ResultType` beside it"
    )
    .into())
}

/// Replaces rustdoc intra-doc links like ``[`Type`]`` with their text, because they only
/// resolve inside rustdoc. Markdown links like ``[`name`](url)`` are kept.
fn strip_intra_doc_links(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;

    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];

        let Some(end) = after.find(']') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let text = &after[..end];
        let tail = &after[end + 1..];

        let is_markdown_link =
            tail.starts_with('(') || tail.starts_with('[') || tail.starts_with(':');

        let is_reference_label = start == 0 && out.ends_with(']');

        if is_intra_doc_link_text(text) && !is_markdown_link && !is_reference_label {
            out.push_str(text);
        } else {
            out.push('[');
            out.push_str(text);
            out.push(']');
        }
        rest = tail;
    }

    out.push_str(rest);
    out
}

/// Returns true for the text of a rustdoc intra-doc link: a Rust path, optionally in backticks.
fn is_intra_doc_link_text(text: &str) -> bool {
    let path = text
        .strip_prefix('`')
        .and_then(|text| text.strip_suffix('`'))
        .unwrap_or(text);

    path.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// A parameter line from a method's `# Parameters` doc section.
struct DocParam {
    name: String,
    annotation: Vec<String>,
    description: String,
}

/// Removes the `# Parameters` section from `lines` and returns its parameter lines.
///
/// Indented and continuation lines are added to the parameter above them.
fn split_parameters_section(lines: &mut Vec<String>) -> Vec<DocParam> {
    let Some(start) = lines.iter().position(|line| line.trim() == "# Parameters") else {
        return vec![];
    };
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.starts_with("# "))
        .map_or(lines.len(), |i| start + 1 + i);

    let mut params: Vec<DocParam> = vec![];
    for line in lines.drain(start..end).skip(1) {
        if let Some((name, rest)) = line
            .strip_prefix("- `")
            .and_then(|rest| rest.split_once('`'))
        {
            let rest = rest.trim_start_matches(':').trim_start();
            let (annotation, text) = match rest.strip_prefix('(').and_then(split_parenthesized) {
                Some((inner, after)) => (split_top_level(inner), after.trim_start_matches(':')),
                None => (vec![], rest),
            };
            params.push(DocParam {
                name: name.to_string(),
                annotation,
                description: text.trim().to_string(),
            });
        } else if !line.trim().is_empty()
            && let Some(param) = params.last_mut()
        {
            if !param.description.is_empty() {
                param.description.push('\n');
            }
            param.description.push_str(line.trim_end());
        }
    }
    params
}

/// Splits `text`, which follows an opening `(`, at its matching `)`.
fn split_parenthesized(text: &str) -> Option<(&str, &str)> {
    let mut depth = 0;
    let mut quoted = false;
    for (i, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' | '[' | '{' if !quoted => depth += 1,
            ')' if !quoted && depth == 0 => return Some((&text[..i], &text[i + 1..])),
            ')' | ']' | '}' if !quoted => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Splits an annotation at the commas outside brackets and quotes.
fn split_top_level(text: &str) -> Vec<String> {
    let mut parts = vec![];
    let (mut depth, mut quoted, mut start) = (0, false, 0);
    for (i, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '(' | '[' | '{' if !quoted => depth += 1,
            ')' | ']' | '}' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                parts.push(text[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim().to_string());
    parts
}

/// Returns `value` as JSON text: as written when it is JSON, otherwise as a string.
fn json_text(value: &str) -> String {
    let value = value.trim().replace("\\\"", "\"");
    serde_json::from_str::<serde_json::Value>(&value)
        .unwrap_or_else(|_| serde_json::Value::String(value.trim_matches('"').to_string()))
        .to_string()
}

/// Returns the generated parameter descriptors for `command`, recording problems in `errors`.
fn documented_params(
    command: &str,
    params: &[(String, String, Option<bool>)],
    documented: &[DocParam],
    errors: &mut Vec<String>,
) -> String {
    let word = |required: bool| if required { "required" } else { "optional" };
    for doc in documented {
        if !params
            .iter()
            .any(|(parameter, _, _)| *parameter == doc.name)
        {
            errors.push(format!(
                "`{command}`: `{}` is documented but is not a parameter",
                doc.name
            ));
        }
    }

    let mut code = String::new();
    for (parameter, schema_ty, type_required) in params {
        let Some(doc) = documented.iter().find(|doc| doc.name == *parameter) else {
            errors.push(format!(
                "`{command}`: `{parameter}` has no line in `# Parameters`"
            ));
            continue;
        };

        let (mut doc_required, mut default, mut examples) = (None, None, vec![]);
        for token in &doc.annotation {
            match token
                .split_once('=')
                .map(|(key, value)| (key.trim(), value))
            {
                Some(("default", value)) => default = Some(json_text(value)),
                Some(("example", value)) => examples.push(json_text(value)),
                _ if token == "required" => doc_required = Some(true),
                _ if token == "optional" => doc_required = Some(false),
                _ => {}
            }
        }
        let required = match (*type_required, doc_required) {
            (Some(by_type), Some(by_doc)) if by_type != by_doc => {
                errors.push(format!(
                    "`{command}`: `{parameter}` is documented as {} but its type makes it {}",
                    word(by_doc),
                    word(by_type)
                ));
                by_type
            }
            (Some(by_type), _) => by_type,
            (None, Some(by_doc)) => by_doc,
            (None, None) => {
                errors.push(format!(
                    "`{command}`: `{parameter}` needs `required` or `optional` in its annotation"
                ));
                true
            }
        };
        if doc.description.is_empty() {
            errors.push(format!("`{command}`: `{parameter}` has no description"));
        }

        let literal = |text: &str| format!("\"{}\"", text.escape_default());
        let description = doc
            .description
            .lines()
            .map(strip_intra_doc_links)
            .collect::<Vec<_>>()
            .join("\n");
        code.push_str(&format!(
            "        _g.param_documented::<{schema_ty}>({}, {}, {required}, {}, &[{}]),\n",
            literal(parameter),
            literal(&description),
            default
                .as_deref()
                .map_or("None".to_string(), |d| format!("Some({})", literal(d))),
            examples
                .iter()
                .map(|e| literal(e))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the generator on `source` and returns the generated file.
    fn generate(source: &str, use_parent_module: bool) -> Result<String, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let methods_rs = dir.path().join("methods.rs");
        fs::write(&methods_rs, source)?;

        generate_openrpc(
            methods_rs.to_str().expect("utf-8 path"),
            &["Rpc"],
            use_parent_module,
            dir.path(),
        )?;

        Ok(fs::read_to_string(dir.path().join("rpc_openrpc.rs"))?)
    }

    #[test]
    fn result_type_comes_from_the_return_type() {
        let generated = generate(
            r#"
trait Rpc {
    #[method(name = "getinfo")]
    async fn get_info(&self) -> Result<GetInfo>;
    #[method(name = "stop")]
    async fn stop(&self) -> jsonrpsee::core::RpcResult<()>;
    #[method(name = "rpc.discover")]
    fn openrpc(&self) -> openrpsee::openrpc::Response;
}
"#,
            false,
        )
        .unwrap();

        assert!(generated.contains(r#"g.result_schema::<GetInfo>("getinfo_result", "")"#));
        assert!(generated.contains(r#"g.result_schema::<()>("stop_result", "")"#));
        assert!(
            generated.contains(
                r#"g.result::<openrpsee :: openrpc :: ResultType>("rpc.discover_result")"#
            )
        );
    }

    #[test]
    fn parent_module_result_type_is_unchanged() {
        let generated = generate(
            r#"
trait Rpc {
    #[method(name = "getwalletinfo")]
    async fn get_wallet_info(&self) -> get_wallet_info::Response;
}
"#,
            true,
        )
        .unwrap();

        assert!(
            generated.contains(
                r#"g.result::<super::get_wallet_info::ResultType>("getwalletinfo_result")"#
            )
        );
    }

    #[test]
    fn unknown_result_type_is_an_error() {
        let error = generate(
            r#"
trait Rpc {
    #[method(name = "getinfo")]
    async fn get_info(&self) -> GetInfo;
}
"#,
            false,
        )
        .unwrap_err();

        assert!(error.to_string().contains("`getinfo`"), "{error}");
    }

    #[test]
    fn tags_line_becomes_tags() {
        let generated = generate(
            r#"
trait Rpc {
    /// Returns software information.
    ///
    /// tags: control, network
    #[method(name = "getinfo")]
    async fn get_info(&self) -> Result<GetInfo>;
}
"#,
            false,
        )
        .unwrap();

        assert!(generated.contains(r#"tags: &["control", "network", ],"#));
        assert!(!generated.contains("tags: control"));
    }

    #[test]
    fn methods_table_does_not_need_phf() {
        let generated = generate("trait Rpc {}", false).unwrap();

        assert!(generated.contains("pub(crate) static METHODS: ::openrpsee::phf::Map"));
        assert!(generated.contains("use ::openrpsee::phf;"));
    }

    #[test]
    fn intra_doc_links_are_stripped() {
        for (line, expected) in [
            ("as a [`GetInfo`] JSON struct", "as a `GetInfo` JSON struct"),
            ("see [crate::methods::Rpc]", "see crate::methods::Rpc"),
            (
                "[`getinfo`](https://zcash.github.io/rpc/getinfo.html)",
                "[`getinfo`](https://zcash.github.io/rpc/getinfo.html)",
            ),
            (
                "[the docs][docs] and [docs]: https://example.com",
                "[the docs][docs] and [docs]: https://example.com",
            ),
            (r#"`["t1abc", "t1def"]`"#, r#"`["t1abc", "t1def"]`"#),
            (
                "a [1] footnote and an unclosed [",
                "a [1] footnote and an unclosed [",
            ),
        ] {
            assert_eq!(strip_intra_doc_links(line), expected, "{line}");
        }
    }

    /// Runs the doc-comment parameter generator on `source` and returns the generated file.
    fn generate_with_docs(source: &str) -> Result<String, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let methods_rs = dir.path().join("methods.rs");
        fs::write(&methods_rs, source)?;
        generate_openrpc_with_doc_params(
            methods_rs.to_str().expect("utf-8 path"),
            &["Rpc"],
            dir.path(),
        )?;
        Ok(fs::read_to_string(dir.path().join("rpc_openrpc.rs"))?)
    }

    #[test]
    fn parameters_come_from_the_doc_comment() {
        let generated = generate_with_docs(
            r#"
trait Rpc {
    /// Returns a block.
    ///
    /// # Parameters
    ///
    /// - `hash_or_height`: (string, required, example="1") The hash or height of the [`Block`].
    /// - `verbosity`: (number, optional, default=1, example=1) 0 for hex,
    ///   1 for a JSON object.
    ///
    /// # Notes
    ///
    /// Some notes.
    #[method(name = "getblock")]
    async fn get_block(&self, hash_or_height: String, verbosity: Option<u8>) -> Result<Block>;
}
"#,
        )
        .unwrap();

        assert!(generated.contains(
            r#"_g.param_documented::<String>("hash_or_height", "The hash or height of the `Block`.", true, None, &["\"1\""]),"#
        ));
        assert!(generated.contains(
            r#"_g.param_documented::<u8>("verbosity", "0 for hex,\n  1 for a JSON object.", false, Some("1"), &["1"]),"#
        ));
        assert!(!generated.contains("# Parameters"), "{generated}");
        assert!(generated.contains("# Notes"));
        assert!(!generated.contains("PARAM_"));
    }

    #[test]
    fn annotation_values_may_contain_commas() {
        let generated = generate_with_docs(
            r#"
trait Rpc {
    /// # Parameters
    ///
    /// - `request`: (object, required, example={"addresses": ["a", "b"]}) The request.
    #[method(name = "getaddressbalance")]
    async fn get_address_balance(&self, request: Request) -> Result<Balance>;
}
"#,
        )
        .unwrap();

        assert!(
            generated.contains(r#"&["{\"addresses\":[\"a\",\"b\"]}"]"#),
            "{generated}"
        );
    }

    #[test]
    fn every_doc_problem_is_reported() {
        let error = generate_with_docs(
            r#"
trait Rpc {
    /// # Parameters
    ///
    /// - `verbose`: (bool, required) Whether to be verbose.
    /// - `extra`: (string, required) Not a parameter.
    #[method(name = "getrawtransaction")]
    async fn get_raw_transaction(&self, txid: String, verbose: Option<u8>) -> Result<Tx>;

    #[method(name = "getblockhash")]
    async fn get_block_hash(&self, index: i32) -> Result<Hash>;

    /// # Parameters
    ///
    /// - `hexdata`: (string, required)
    #[method(name = "submitblock")]
    async fn submit_block(&self, hexdata: String) -> Result<Submit>;
}
"#,
        )
        .unwrap_err()
        .to_string();

        for problem in [
            "5 problems",
            "`getrawtransaction`: `extra` is documented but is not a parameter",
            "`getrawtransaction`: `txid` has no line in `# Parameters`",
            "`getrawtransaction`: `verbose` is documented as required but its type makes it optional",
            "`getblockhash`: `index` has no line in `# Parameters`",
            "`submitblock`: `hexdata` has no description",
        ] {
            assert!(error.contains(problem), "missing {problem:?} in:\n{error}");
        }
    }

    #[test]
    fn constants_layout_keeps_the_parameters_section() {
        let generated = generate(
            r#"
trait Rpc {
    /// # Parameters
    ///
    /// - `index`: (number, required) The block index.
    #[method(name = "getblockhash")]
    async fn get_block_hash(&self, index: i32) -> Result<Hash>;
}
"#,
            false,
        )
        .unwrap();

        assert!(generated.contains("# Parameters"));
        assert!(generated.contains("crate::methods::PARAM_INDEX_DESC"));
    }
}
