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

                let params = method.sig.inputs.iter().filter_map(|arg| match arg {
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
                });

                contents.push('"');
                contents.push_str(&command);
                contents.push_str("\" => ::openrpsee::openrpc::RpcMethod {\n");

                let mut tags = vec![];
                contents.push_str("    description: \"");
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
                        let trimmed_line = if line.is_empty() { &line } else { &line[1..] };

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
                for (parameter, schema_ty, required) in params {
                    let param_upper = parameter.to_uppercase();

                    contents.push_str("        _g.param::<");
                    contents.push_str(&schema_ty);
                    contents.push_str(">(\"");
                    contents.push_str(&parameter);
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
}
