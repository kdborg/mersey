# How V8 and JavaScriptCore hold strings

Reference for the representation decision left open in
`where-the-cli-time-goes.md`, where `bench/cli/strings` sits at 1.7× Node and
the profile is allocator-shaped. Node and Deno are V8; Bun is JavaScriptCore.
Both engines have already made these trade-offs, and one of their thresholds
says something directly about a measurement already in this repo.

## 1. Neither engine stores ASCII as sixteen bits

V8 has `SeqOneByteString` and `SeqTwoByteString`; JSC has `LChar` (8-bit) and
`UChar` (16-bit) with an `is8Bit` flag. A string that fits Latin-1 is stored one
byte per character and widened only when something non-ASCII arrives.

The observable semantics stay 16-bit code units — the width is an internal
representation, not a language decision. That matters here, because "engine
strings are UTF-16 (WTF-16) with JS-aligned code-unit semantics" is a *decided*
point (CLAUDE.md) and this does not touch it.

Mersey is always two bytes per unit. For the benchmark inputs — `"1.2.3-rc.1"`,
`"a=1&b=two"` — every byte of every buffer is half wasted, which is half the
`memmove` (6% of the profile) and a larger allocation size class on each of
them.

**Cost here:** compiled code reads `(pointer, length)` as `u16` directly, at
every site. A width flag means either branching at each read or specialising the
generated code by width, which is what V8 does with separate instance types.
Large, and it does not need `Value` to grow.

## 2. Concatenation is a rope, not a copy

`a + b` allocates a node holding two pointers and returns immediately — V8's
`ConsString`, JSC's `JSRopeString` — and the result is flattened lazily, on the
first access that needs contiguous characters. JSC packs the length and the
`is8Bit` flag into the fiber pointers to keep `sizeof(JSRopeString) == 32`
against `sizeof(JSString) == 16`.

Mersey copies on every template. `${s}#${k}` in a loop allocates and copies each
time, which is what the churn loop in `tests/jit/string-subrange.mersey` is
doing on purpose.

**Cost here:** a rope needs two pointers plus a length. `Value::Str(Rc<…>)`
could point at a rope node without growing `Value`, but every read would then
have to check for and force a flatten — and the JIT's whole string model is a
flat `(pointer, length)` pair in registers.

## 3. Substrings are shared — but only above thirteen characters

Both engines share: V8's `SlicedString` is (parent, offset, length). And both
refuse to below a threshold — V8's `SlicedString::kMinLength` and
`ConsString::kMinLength` are both **13**. Shorter than that and it copies
outright.

**This is the same answer this repo already measured.** Sharing a slice's parent
entry instead of copying its units came out neutral, and the reason recorded was
that the slices in that workload are `"1"`, `"2"`, `"rc.1"` — every one of them
under 13. V8 would copy them too. That negative result was not a local
accident.

The other half is retention, which both engines are known for: a substring keeps
its whole parent alive. The usual war story is extracting a few short strings
from a large document and pinning the document, fixed by forcing materialisation
through a JSON round-trip. That is the same cost noted when sharing was tried
here — after `s = s.slice(0, plus)` the original stays alive behind the slice.

## 4. The allocation itself is a pointer bump

This is the part that is not a string decision at all. V8's young generation is
a linear allocation buffer: no free list, no search — place the object, move the
pointer. Collection copies out the 10–20% that survive and reclaims the rest by
resetting the pointer, so a short-lived string costs essentially nothing to
allocate and nothing to free.

Mersey pays mimalloc plus an `Rc` for every string, and `Rc<Vec<u16>>` is two
allocations: the `Rc` box, then the `Vec`'s buffer. A `parse` in
`bench/cli/strings` does about sixteen of them.

This looked like the largest gap and the cheapest to close. **It was tried, and
it is not.**

A pool of released `Rc<Vec<u16>>` buffers — reused instead of freed, harvested
both from `Arena::release` and from the wholesale `clear` at the end of every
compiled call, and drawn on by `own_str` and `str_join`, the two hottest string
builders — measured *neutral*. `std-hot` 0.55 against 0.54–0.55; `strings`
44.7–45.4 against 45.2–45.5, back to back in one window.

The reason is the difference between the two halves of what a nursery buys.
mimalloc already has per-thread free lists and small-size-class caching, so a
malloc/free pair for a short string is about as cheap as a pool pop/push — the
`mi_malloc` and `mi_free` in the profile are near the floor for this allocation
*pattern*, not waste a pool can reclaim. What V8 gets that a pool cannot is the
other half: nothing is freed per object at all. The 80–90% that die are
reclaimed by resetting a pointer, and that only works because the collector
copies survivors out, which is a collector design and not an allocator one.

## Ranking, for this engine

1. **One-byte storage for Latin-1.** The biggest constant-factor win, and the
   one every production engine treats as table stakes. Large JIT change; no
   `Value` change.
2. **Fewer allocations per string** — `Rc<[u16]>` instead of `Rc<Vec<u16>>`,
   which is one allocation rather than two. Blocked at the argument boundary,
   not by the lint.
3. **Ropes.** Only pays where concatenation is hot, and fights the flat
   `(pointer, length)` model hardest.
4. **Reusing buffers instead of freeing them.** Tried; neutral. mimalloc is
   already this good.
5. **Slices below thirteen characters.** Tried; neutral. V8 agrees.

The two that remain are both about allocating *less*, not about allocating
*faster* — which is the finding underneath both negative results. This engine's
allocation cost is at the floor for its current representation, so the only
lever left is the representation.

Sources: [V8 `string.h`](https://github.com/v8/v8/blob/main/src/objects/string.h),
[Exploring V8's strings](https://iliazeus.lol/articles/js-string-optimizations-en/),
[WebKit `JSString.h`](https://github.com/WebKit/WebKit/blob/main/Source/JavaScriptCore/runtime/JSString.h),
[Orinoco: young generation garbage collection](https://v8.dev/blog/orinoco-parallel-scavenger),
[V8's mark-sweep nursery](https://wingolog.org/archives/2023/12/08/v8s-mark-sweep-nursery).
