# 0078. Codec uses

Status: accepted

## Context

A `validate` grant is needed where code declassifies Untrusted contents: a use of a validator, of a declassifying decoder or of a declassifying encoder. Generic code decodes and encodes its type parameters through `Encode` bounds without knowing the instantiation, so the use that fixes the type is the line naming the instantiation. Only calls of the std codecs and direct method calls were counted, so a module could decode or encode a type holding Untrusted contents through its own generic function, a function value of it, or a generic impl reached through a bound or `Dyn`, without the grant.

## Decision

- **Codec uses are computed per function, from its body.** They are the type parameters whose decoders and encoders it uses, through `Encode` bounds or by naming instantiations of other functions with them, transitively; the std codecs declare theirs. Naming an instantiation of a function uses those codecs of the arguments given to these parameters, and is judged at that line as any direct use is. A use inside generic code is judged only when its type is not a bare type parameter.
- **Conformances name implementing functions.** Giving a concrete type to a bounded parameter, calling an interface method on it or viewing it as `Dyn` uses what the impl line's implementing functions and default bodies use for that type and, for a method call, its own type arguments, a validator among them included, through derives and std conformances by their constituents and arguments. A codec use on a method's own type parameter that the conformance leaves unfixed is judged declassifying, so a use never escapes for want of an argument. This covers `Decoder` implementations and generic impls without recording every bound call of every generic body.
- **Codec uses are interface facts of the function.** They enter its interface hash through a facts line, written only when the function has some, so a release that changes them is breaking for consumers and the closure set of a naming line holds them; no other function's interface hash changes.
- **One body walk serves the codec uses and the item's own grants**, as a query over the typed body, recomputed to a fixed point within a cycle of generic functions.

## Consequences

- Wrapping a codec in a generic function no longer avoids the grant, and generic code needs no grant for its parameters' codecs.
- A generic function's interface hash depends on its body where its codec uses do.
- A bound whose interface has a generic impl reaching a codec is judged at every instantiation giving that impl's type, even when the generic code never calls the method.
