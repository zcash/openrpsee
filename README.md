# Openrpsee

**Openrpsee** is a helper library for building **OpenRPC documents** and implementing the `rpc.discover` endpoint.

Its main goal is to centralize common functionality used across the Zcash **Z3 stack** (**Zebra**, **Zallet**, and **Zaino**) while remaining generally useful for any project that builds JSON-RPC servers using the `jsonrpsee` ecosystem.

## Overview

Openrpsee provides utilities to:

- Extract RPC method definitions from Rust traits
- Generate a structured representation of those methods at build time
- Produce standards-compliant OpenRPC documents at runtime
- Expose those documents through a `rpc.discover` RPC endpoint

## Generating OpenRPC Metadata

The `generate_openrpc` function parses the given RPC traits, extracts all method definitions, and builds a map containing the metadata required to generate an OpenRPC document.
This map is then written as a Rust source file into the specified output directory.

### Example

- Zebra: https://github.com/ZcashFoundation/zebra/blob/14c6950a20cb9c0ced7a5b9455e411add4148884/zebra-rpc/build.rs#L171

## Including the Generated File

The generated Rust file is intended to be included from your project’s `methods.rs` (or equivalent) module.

### Example

- Zebra: https://github.com/ZcashFoundation/zebra/blob/14c6950a20cb9c0ced7a5b9455e411add4148884/zebra-rpc/src/methods.rs#L141

## Generating the OpenRPC Document

The `openrpc` module provided by this crate consumes the generated method map and produces an OpenRPC document.
This document is typically returned by an implementation of the `rpc.discover` RPC method.

### Example

- Zebra: https://github.com/ZcashFoundation/zebra/blob/14c6950a20cb9c0ced7a5b9455e411add4148884/zebra-rpc/src/methods.rs#L3622-L3640

## Argument Documentation

All RPC arguments are expected to be described using constants. These constants are used when generating the OpenRPC schema.

### Example

- Zebra: https://github.com/ZcashFoundation/zebra/blob/14c6950a20cb9c0ced7a5b9455e411add4148884/zebra-rpc/src/methods.rs#L145-L173

## Result Types

Each method's result schema comes from its return type, which must implement `schemars::JsonSchema`:

- With `use_parent_module = false`, the `T` in a `Result<T, E>` or `RpcResult<T>` return type.
- With `use_parent_module = true`, the `ResultType` in the method's own module.
- For a `path::Response` return type, the `path::ResultType` beside it.

Any other return type is a build script error.

Results are described as they are serialized and parameters as they are deserialized, so a result field with `skip_serializing_if` is optional. A type used both ways must have the same schema in both; debug builds panic in `Generator::into_components` when it doesn't.

## Tags

A `tags: a, b` line in a method's doc comment becomes the method's OpenRPC `tags`, and is left out of its description.

## Viewing a Document

Call `rpc.discover` on a running node and open the saved file in the [OpenRPC playground](https://playground.open-rpc.org/):

```sh
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"rpc.discover","params":[],"id":1}' \
  http://127.0.0.1:8232/ | jq .result > openrpc.json
```
