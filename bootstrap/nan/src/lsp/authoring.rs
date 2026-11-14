//! Authoring: completion, signature help, inlay hints and semantic tokens.

use super::Server;
use super::json::Json;

pub fn completion(_: &Server, _: &Json) -> Option<Json> {
    Some(Json::Null)
}
pub fn signature_help(_: &Server, _: &Json) -> Option<Json> {
    Some(Json::Null)
}
pub fn inlay_hints(_: &Server, _: &Json) -> Option<Json> {
    Some(Json::Null)
}
pub fn semantic_tokens(_: &Server, _: &Json) -> Option<Json> {
    Some(Json::Null)
}
pub fn semantic_tokens_options() -> Json {
    Json::Null
}
