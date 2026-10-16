# 0025. Wrapped parts hold payloads by address

Status: accepted

## Context

A projection returning an `Option` or `Result` returns a wrapped part. [NIR](../spec/19-nir.md#types-and-references) types each payload that is not a view as `ref T` or `ref mut T`, Copy or not, and `for p in mut e` binds writable places of Copy parts, so assigning one must write into `e` ([iteration](../spec/07-ownership.md#iteration)). Elsewhere a wrapped part of Copy type is copied when used as a value.

## Decision

A wrapped part holds the address of each payload that is not a view, Copy payloads included, whether a read or a mut projection returns it. Where a wrapped part is used as an owned value, a read argument, a stored or returned value outside a projection, a field or a payload, the lowering copies its payloads out into a value of the plain type.

Alternatives rejected:

- **Copy payloads by value in read wrapped parts:** the layout of a wrapped part would depend on whether a mut projection returned it, so every place, payload step and supplied projection would carry that flag beside the type.

## Consequences

- A wrapped part has one layout per type, the one its NIR type gives.
- Using a wrapped part of Copy type as a value costs a copy of each payload it points at.
- A wrapped part owns nothing: the elaboration never releases one, nor stores one into a temporary.
