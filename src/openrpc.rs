//! OpenRPC document generation for JSON-RPC methods.

use std::borrow::Cow;

use documented::Documented;
use jsonrpsee::core::{JsonValue, RpcResult};
use schemars::{JsonSchema, Schema, SchemaGenerator, generate::SchemaSettings};
use serde::Serialize;

/// Response to an `rpc.discover` RPC request.
pub type Response = RpcResult<ResultType>;
/// The result type for an `rpc.discover` RPC request.
pub type ResultType = OpenRpc;

/// Static information about a JSON-RPC method.
pub struct RpcMethod {
    /// A description of the method.
    pub description: &'static str,
    /// The method's OpenRPC tags.
    pub tags: &'static [&'static str],
    /// A function that generates the method's parameters.
    pub params: fn(&mut Generator) -> Vec<ContentDescriptor>,
    /// A function that generates the method's result.
    pub result: fn(&mut Generator) -> ContentDescriptor,
    /// Whether the method is deprecated.
    pub deprecated: bool,
}

impl RpcMethod {
    /// Generates the OpenRPC method descriptor.
    pub fn generate(&self, generator: &mut Generator, name: &'static str) -> Method {
        let description = self.description.trim();

        Method {
            name,
            summary: description
                .split_once('\n')
                .map(|(summary, _)| summary)
                .unwrap_or(description),
            description,
            params: (self.params)(generator),
            result: (self.result)(generator),
            deprecated: self.deprecated,
            tags: self.tags.iter().map(|&name| Tag { name }).collect(),
        }
    }
}

/// An OpenRPC document generator.
pub struct Generator {
    /// Parameters are described as they are deserialized.
    inner: SchemaGenerator,
    /// Results are described as they are serialized, so skipped fields are optional.
    results: SchemaGenerator,
}

impl Default for Generator {
    fn default() -> Self {
        Self::new()
    }
}

impl Generator {
    /// Creates a new OpenRPC document generator.
    pub fn new() -> Self {
        let settings = SchemaSettings::draft07().with(|s| {
            s.definitions_path = "#/components/schemas/".into();
        });

        Self {
            inner: settings.clone().for_deserialize().into_generator(),
            results: settings.for_serialize().into_generator(),
        }
    }

    /// Constructs the descriptor for a JSON-RPC method parameter.
    pub fn param<T: JsonSchema>(
        &mut self,
        name: &'static str,
        description: &'static str,
        required: bool,
    ) -> ContentDescriptor {
        ContentDescriptor {
            name,
            summary: description
                .split_once('\n')
                .map(|(summary, _)| summary)
                .unwrap_or(description),
            description,
            required,
            schema: self.inner.subschema_for::<T>(),
            deprecated: false,
        }
    }

    /// Constructs the descriptor for a JSON-RPC method parameter documented in the method's doc
    /// comment, adding its `default` and `examples` to the schema.
    ///
    /// `default` and `examples` are JSON texts, checked by the build script.
    pub fn param_documented<T: JsonSchema>(
        &mut self,
        name: &'static str,
        description: &'static str,
        required: bool,
        default: Option<&'static str>,
        examples: &'static [&'static str],
    ) -> ContentDescriptor {
        let mut descriptor = self.param::<T>(name, description, required);
        if default.is_none() && examples.is_empty() {
            return descriptor;
        }

        // Draft-07 ignores keywords next to a `$ref`, so wrap a reference.
        let mut schema = descriptor.schema;
        if schema.get("$ref").is_some() {
            schema = schemars::json_schema!({ "allOf": [schema] });
        }
        let json = |text: &str| serde_json::from_str(text).expect("checked by the build script");
        if let Some(default) = default {
            schema.insert("default".into(), json(default));
        }
        if !examples.is_empty() {
            schema.insert(
                "examples".into(),
                serde_json::Value::Array(examples.iter().map(|example| json(example)).collect()),
            );
        }
        descriptor.schema = schema;
        descriptor
    }

    /// Constructs the descriptor for a JSON-RPC method's result type.
    pub fn result<T: Documented + JsonSchema>(&mut self, name: &'static str) -> ContentDescriptor {
        ContentDescriptor {
            name,
            summary: T::DOCS
                .split_once('\n')
                .map(|(summary, _)| summary)
                .unwrap_or(T::DOCS),
            description: T::DOCS,
            required: false,
            schema: self.results.subschema_for::<T>(),
            deprecated: false,
        }
    }

    /// Constructs the descriptor for a JSON-RPC method's result type, with the given description.
    ///
    /// Unlike [`result`](Self::result), `T` doesn't need to implement `Documented`.
    pub fn result_schema<T: JsonSchema>(
        &mut self,
        name: &'static str,
        description: &'static str,
    ) -> ContentDescriptor {
        ContentDescriptor {
            name,
            summary: description
                .split_once('\n')
                .map(|(summary, _)| summary)
                .unwrap_or(description),
            description,
            required: false,
            schema: self.results.subschema_for::<T>(),
            deprecated: false,
        }
    }

    /// Consumes the generator and produces the OpenRPC components.
    pub fn into_components(mut self) -> Components {
        let mut schemas = self.inner.take_definitions(false);
        for (name, schema) in self.results.take_definitions(false) {
            match schemas.get(&name) {
                // A type that serializes differently from how it deserializes, used both as a
                // parameter and in a result, has two schemas under one name. Keep the parameter's.
                Some(existing) => debug_assert!(
                    *existing == schema,
                    "`{name}` has different parameter and result schemas"
                ),
                None => {
                    schemas.insert(name, schema);
                }
            }
        }
        Components { schemas }
    }
}

/// An OpenRPC document.
#[derive(Clone, Debug, Serialize, Documented)]
pub struct OpenRpc {
    /// The OpenRPC specification version.
    pub openrpc: &'static str,
    /// Information about the API.
    pub info: Info,
    /// The available JSON-RPC methods.
    pub methods: Vec<Method>,
    /// The components (schemas) used in the document.
    pub components: Components,
}

// OpenRPC tools reject component names outside `[a-zA-Z0-9.-_]` and may not resolve a remote
// `$ref`, so the document's own schema is named and described inline.
impl JsonSchema for OpenRpc {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("OpenRpcDocument")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "An OpenRPC document, as defined by https://spec.open-rpc.org/.",
        })
    }
}

/// Information about the API.
#[derive(Clone, Debug, Serialize)]
pub struct Info {
    /// The title of the API.
    pub title: &'static str,
    /// A description of the API.
    pub description: &'static str,
    /// The version of the API.
    pub version: &'static str,
}

/// A JSON-RPC method.
#[derive(Clone, Debug, Serialize)]
pub struct Method {
    name: &'static str,
    summary: &'static str,
    description: &'static str,
    params: Vec<ContentDescriptor>,
    result: ContentDescriptor,
    #[serde(skip_serializing_if = "is_false")]
    deprecated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<Tag>,
}

/// An OpenRPC tag for grouping methods.
#[derive(Clone, Debug, Serialize)]
pub struct Tag {
    name: &'static str,
}

/// A descriptor for a JSON-RPC method's parameter or result.
#[derive(Clone, Debug, Serialize)]
pub struct ContentDescriptor {
    name: &'static str,
    summary: &'static str,
    description: &'static str,
    #[serde(skip_serializing_if = "is_false")]
    required: bool,
    schema: Schema,
    #[serde(skip_serializing_if = "is_false")]
    deprecated: bool,
}

/// The components (schemas) used in the OpenRPC document.
#[derive(Clone, Debug, Serialize)]
pub struct Components {
    schemas: serde_json::Map<String, JsonValue>,
}

fn is_false(b: &bool) -> bool {
    !b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field the type always deserializes but skips when serializing.
    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    struct Param {
        #[serde(skip_serializing_if = "Vec::is_empty")]
        values: Vec<u32>,
    }

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    struct Output {
        #[serde(skip_serializing_if = "Vec::is_empty")]
        values: Vec<u32>,
    }

    fn required(components: &Components, name: &str) -> Vec<String> {
        serde_json::to_value(&components.schemas[name]["required"])
            .ok()
            .and_then(|required| serde_json::from_value(required).ok())
            .unwrap_or_default()
    }

    #[test]
    fn skipped_fields_are_optional_only_in_results() {
        let mut generator = Generator::new();
        generator.param::<Param>("param", "", true);
        generator.result_schema::<Output>("result", "");
        let components = generator.into_components();

        assert_eq!(required(&components, "Param"), ["values"]);
        assert!(required(&components, "Output").is_empty());
    }

    #[test]
    fn document_schema_is_self_contained() {
        let mut generator = Generator::new();
        generator.result::<OpenRpc>("rpc.discover_result");
        let components = generator.into_components();
        let schemas = serde_json::to_string(&components.schemas).expect("serializes");

        assert!(components.schemas.contains_key("OpenRpcDocument"));
        assert!(components.schemas.keys().all(|name| {
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        }));
        assert!(!schemas.contains("\"$ref\":\"http"), "{schemas}");
    }

    #[test]
    fn documented_params_add_default_and_examples() {
        let mut generator = Generator::new();
        let plain = generator.param_documented::<u8>("verbosity", "", false, Some("1"), &["2"]);
        let reference =
            generator.param_documented::<Param>("request", "", true, None, &["{\"values\":[1]}"]);

        let plain = serde_json::to_value(&plain.schema).unwrap();
        assert_eq!(plain["default"], 1);
        assert_eq!(plain["examples"], serde_json::json!([2]));

        // A `$ref` is wrapped, because draft-07 ignores keywords next to it.
        let reference = serde_json::to_value(&reference.schema).unwrap();
        assert_eq!(reference["allOf"][0]["$ref"], "#/components/schemas/Param");
        assert_eq!(
            reference["examples"],
            serde_json::json!([{ "values": [1] }])
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "`Param` has different parameter and result schemas")]
    fn conflicting_schemas_fail_in_debug_builds() {
        let mut generator = Generator::new();
        generator.param::<Param>("param", "", true);
        generator.result_schema::<Param>("result", "");
        generator.into_components();
    }
}
