# 0077. Construction text

Status: accepted

## Context

NaN OS shows typed state to agents as the text derived `Render` writes, such as `Config(mtu: 1500, dhcp: true)`, and takes their requests in the same form. Std decoded only `mem::wire` and JSON, so reading such text back meant a hand-written parser per type: a declassifying validator, hence a trust escape, re-checking by hand what derived decoders already check. The rendering itself could not be read back: text inside a rendering was written raw, so `Title(name: a, seq: 2)` could come from a name `a` or `a, seq: 2`.

## Decision

- **Nested text renders as a literal.** Inside the rendering of another value, a char or text is written as a NaN char or text literal, escaping what `CHARACTER` excludes, and a box or shared value as its contents. `core::Renderer.write_nested` writes this nested rendering; derived `Render` and the std generic renderings write every constituent with it. At the top level text still renders as its characters, so templates and logs are unchanged.
- **`text::construction` decodes, and never encodes.** Its `Codec.decode` is a std `core::Decoder` format like JSON, so derived decoders check keys, variants, niches and predicates, and decoding a type holding text is classified as with any decoder. Writing stays with `Render` and `text::fmt`: a `core::Encoder` never receives type names.
- **The grammar is the rendering's, read leniently where it costs nothing.** Names before records and variant keys are optional or unchecked, fields come in any order, whitespace may separate tokens and numbers may hold `_`; literals follow the lexical grammar exactly. A labelled payload is read as a record without name or parentheses.
- **Untrusted contents stay out.** `<untrusted>` decodes as no kind, so a rendering holding an Untrusted value fails with `Malformed`; the round trip holds for renderings that hold none.

## Consequences

- An agent's request in construction text is decoded with no trust escape beyond the decoder's own class, and any derived rendering that is not elided and holds no Untrusted value decodes back to its value.
- Renderings of values holding text inside them change: text gains quotes and escapes.
- Lowering checks rejected code points when rendering and decoding literals, from the same table as the lexer.
