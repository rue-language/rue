# Release Notes

## 0.19.0 - 2025/02/11

## Fixed

- Fixed the `IntoIterator` impl that triggered a Rust compiler miscompilation in some versions. [#80]
    - See the associated [`rustc` issue](https://github.com/rust-lang/rust/issues/136856).

## Improvements

- Updated and improved documentation about backend properties. [#83] [#84]
- Update outdated dependencies. [#82]
- Added `serde::{Serialize,Deserialize}` impls for all crate defined symbol types. [#86]

## Internal

- Improve and modernize GitHub Actions CI. [#81]

[#80]: https://github.com/Robbepop/string-interner/pull/80
[#81]: https://github.com/Robbepop/string-interner/pull/81
[#82]: https://github.com/Robbepop/string-interner/pull/82
[#83]: https://github.com/Robbepop/string-interner/pull/83
[#84]: https://github.com/Robbepop/string-interner/pull/84
[#86]: https://github.com/Robbepop/string-interner/pull/86

## 0.18.0 - 2024/11/12

## Fixed

- The `serde` crate feature is no longer enabled via `std` crate feature. [#73]

## Removed

- Removed the unused `cfg-if` dependency. [#73]

## Changed

- Updated `hashbrown` dependency to `0.15.1`. [#73]

## Internal

- Fixed many `clippy` and `formatting` issues. [#73]

[#73]: https://github.com/Robbepop/string-interner/pull/73

## 0.17.0 - 2024/05/01

## Added

- Added `StringInterner::resolve_unchecked` method. (https://github.com/Robbepop/string-interner/pull/68)

## Fixed

- Fixed soundness issue in `BufferBackend::resolve`. (https://github.com/Robbepop/string-interner/pull/68)

## 0.16.0 - 2024/05/01

## Added

- Added `StringInterner::iter` method. (https://github.com/Robbepop/string-interner/pull/65)

## Changed

- Optimized `BufferBackend::{resolve, iter}` methods. (https://github.com/Robbepop/string-interner/pull/64)

## Fixed

- Fixed unsoundness issue in `BucketBackend`. (https://github.com/Robbepop/string-interner/pull/66)

## Removed

- Removed `SimpleBackend` since it served no real purpose. (https://github.com/Robbepop/string-interner/commit/549db6c2efeac5acb5e8084e69fa22891ae14019)

## 0.15.0 - 2024/02/08

## Changed

- Update to `hashbrown` version `0.14.0`. (https://github.com/Robbepop/string-interner/pull/58)
- Improve `no_std` support. (https://github.com/Robbepop/string-interner/pull/44)
- Fix bug in `BufferBackend::with_capacity` method. (https://github.com/Robbepop/string-interner/pull/54)

## 0.14.0 - 2021/10/27

## Added

- Added the new `BufferBackend` string interner backend.
	- This backend focuses on minimum memory consumption and allocations
	  at the costs of decreased symbol resolution performance.
	- Use this when memory consumption is your main concern.
- Added example of how to use a different string interner backend or symbol.
- Added library docs comparing all the different string interner backends.

## Changed

- The `string_interner` crate now uses the Rust 2021 edition.
- The `DefaultBackend` now is the `StringBackend` and no longer the `BucketBackend`.
- The generic `S` symbol parameter of all string interner backends
  now defaults to the `DefaultSymbol`.
- The `Backend` trait is no longer generic over a symbol `S` but instead
  has a `Symbol` associated type now.
- The `StringInterner` no longer has a generic `S` symbol parameter and
  now instead uses the `Symbol` associated type from its used backend `B`.

## Dev. Note

- The `memory_consumption` tests now shrink the string interners before querying
  their memory consumption. This yields more stable numbers than before.
- The `memory_consumption` test now also tests the total amount of allocations
  and deallocations made by the string interner backends.
- Add `README` section about benchmarking the crate.

## 0.13.0 - 2021/08/25

- Update `hashbrown` dependency from version `0.9` to version `0.11`.
- Add `shrink_to_fit` method to `StringInterner` via backend. (#36)
- Add support more than 4G of interned strings with `StringBackend`. (#37)
- Remove `S: Symbol` trait bound from interner backends.
- Remove `S: Symbol` trait bound from `Clone impl` for `StringBackend`.

- Reworked the memory and allocation tests
	- Run them via `cargo test -- --test-threads 1`
- CI now tests the whole build for windows, linux (ubuntu) and macos.
- Add `cargo-audit` and `cargo-outdated` checks to CI pipeline.
- Remove no longer needed `jemalloc` `dev-dependency`.

## 0.12.2 - 2021/01/11

- Ensure cloned `StringInterner` can still look up the same symbols.
  [#34](https://github.com/Robbepop/string-interner/pull/34) (Thanks @alamb)
    - This requires `BuildHasher: Clone` trait bound for `StringInterner`'s `Clone` impl.

## 0.12.1 - 2020/11/14

- The `BucketBackend` now implements `Send` + `Sync`.
- Implemented some minor internal improvements.
- Update dependencies:
	- `hashbrown 0.8` -> `0.9`
	- `cfg-if 0.1` -> `1.0`

## 0.12.0 - 2020/07/15

- Make `DefaultBackend` generic over its symbol type.
	- This simplifies type ascription of string interners that do not use the
	  default symbol type.
	  - E.g. `StringInterner<usize>` is now possible to write (again).
- Add `backends` crate feature.
	- Enabled by default.
	- Disable this if you do not use any of the backends provided by the
	  `string-interner` crate.

## 0.11.3 - 2020/07/15

- Add `Symbol` implementation for `usize`.

## 0.11.2 - 2020/07/15

- Add new `StringBackend` that is optimized for memory allocations and footprint.
	- Use it if your memory constraints are most important to you.

## 0.11.1 - 2020/07/14

Special thanks to [Ten0](https://github.com/Ten0) for help with this release!

- Remove usage of `unsafe` in `Symbol::try_from_usize` methods.
- Remove no longer correct `unsafe impls` for `Send` and `Sync` for `StringInterner`.
- Add new crate feature `more-inline` that puts more `#[inline]` on public methods.
	- The new `more-inline` crate feature is enabled by default. If you want to
	  turn it off use `--no-default-features`.
	- Enabling `more-inline` also enables `hashbrown/more-inline`.
- Remove `&B: IntoIter` trait bound from `Clone` impl of `StringInterner`

## 0.11.0 - 2020/07/14

Thanks a lot (again) to [CAD97](https://dev.to/cad97) who is the vanguard
of the technical improvements in this release with their
[blog post](https://dev.to/cad97/string-interners-in-rust-797).

- Significantly improved `StringInterner`'s memory consumption independend
	from the used internment backend.
- Significantly improved `StringInterner`'s throughput for interning strings.
- Change the `Backend` trait:
	- `intern` is no longer `unsafe`
	- `intern` returns `S` (symbol type) instead of `(InternedStr, S)`
	- same as above for `intern_static`
	- add `unsafe fn resolve_unchecked` which does the same as `resolve`
		but explicitely without bounds checking
- No longer export `backend::InternedStr` type
- Make `hashbrown` a mandatory dependency.
	**Note:** We depend on it for the moment for its `raw_entry` API that has not yet been
	stabilized for Rust. Also benchmarks show that it is 20-30% faster than Rust's
	hashmap implementation.
- Benchmarks now show performance when using `FxBuildHasher` as build hasher.

## 0.10.1 - 2020/07/14

- Allow to intern `&'static str` using `get_or_intern_static` API.
	- This is a common use case and more efficient since the interner can
		skip some allocations in this special case.
- Fix bug in `SymbolU16` and `SymbolU32` that instantiating them with values
	greater or equal to `u16::MAX` or `u32::MAX` respectively caused them to
	panic instead of returning `None`.
	- Special thanks to [Ten0](https://github.com/Ten0) for reporting the issue!
- Add a bunch of additional unit tests to further solifidy the implementation.

## 0.10.0 - 2020/07/13

Special thanks to [CAD97](https://dev.to/cad97) who motivated me to craft this
release through [their blog post](https://dev.to/cad97/string-interners-in-rust-797)
"String interners in Rust".
Also I want to thank [matklad](https://github.com/matklad) who wrote a nice
[blog post](https://matklad.github.io/2020/03/22/fast-simple-rust-interner.html)
that inspired the design of the new `BucketBackend` for `StringInterner`.

- Implement pluggable backends for `StringInterner`.
	Uses the new `BucketBackend` by default which results in significant
	performance boosts and lower memory consumption as well as fewer overall
	memory allocations.

	This makes it possible for dependencies to alter the behavior of internment.
	The `string-interner` crate comes with 2 predefined backends:
	1. `SimpleBackend`: Which is how the `StringInterner` of previous versions
		worked by default. It performs one allocation per interned string.
	2. `BucketBackend`: Tries to minimize memory allocations and packs
		interned strings densely. This is the new default behavior for this crate.
- Due to the above introduction of backends some APIs have been removed:
	- `reserve`
	- `capacity`
	- the entire `iter` module
		- Note: Simple iteration through the `StringInterer`'s interned strings
				and their symbols is still possible if the used backend supports
				iteration.
	- `resolve_unchecked`: Has no replacement, yet but might be reintroduced
							in future versions again.
	- `shrink_to_fit`: The API design was never really a good fit for interners.

## 0.9.0 - 2020/07/12

- Remove `Ord` trait bound from `Symbol` trait
	- Also change `Symbol::from_usize(usize) -> Self` to `Symbol::try_from_usize(usize) -> Option<Self>`
- Minor performance improvements for `DefaultSymbol::try_from_usize`
- Put all iterator types into the `iter` sub module
- Put all symbol types into the `symbol` sub module
- Add new symbol types:
	- `SymbolU16`: 16-bit wide symbol
	- `SymbolU32`: 32-bit wide symbol (default)
	- `SymbolUsize`: same size as `usize`
- Various internal improvements and reorganizations

## 0.8.0 - 2020/07/12

- Make it possible to use this crate in `no_std` environments
	- Use the new `hashbrown` crate feature together with `no_std`
- Rename `Sym` to `DefaultSymbol`
- Add `IntoIterator` impl for `&StringInterner`
- Add some `#[inline]` annotations which improve performance for queries
- Various internal improvements (uses `Pin` self-referentials now)

## 0.7.1 - 2019/09/01

- **CRITICAL** fix use after free bug in `StringInterner::clone()`
- implement `std::iter::Extend` for `StringInterner`
- `Sym::from_usize` now avoids using `unsafe` code
- optimize `FromIterator` impl of `StringInterner`
- move to Rust 2018 edition

Thanks [YOSHIOKA Takuma](https://github.com/lo48576) for implementing this release.

## 0.7.0 - 2019/08/07

- changed license from MIT to MIT/APACHE2.0
- removed generic impl of `Symbol` for types that are `From<usize>` and `Into<usize>`
- removed `StringInterner::clear` API since its usage breaks invariants
- added `StringInterner::{capacity, reserve}` APIs
- introduced a new default symbol type `Sym` that is a thin wrapper around `NonZeroU32` (idea by [koute][gh-user-koute])
- made `DefaultStringInterner` a type alias for the new `StringInterner<Sym>`
- added convenient `FromIterator` impl to `StringInterner<S: Sym>`
- dev
	- rewrote all unit tests (serde tests are still missing)
	- entirely refactored benchmark framework
	- added `html_root_url` to crate root

Thanks [matklad][gh-user-madklad] for suggestions and impulses

## 0.6.4 - 2019/09/04

- **CRITICAL:** fix use after free bug in `StringInterner::clone` implementation.

## 0.6.3 - 2017/09/20

- fixed a bug that `StringInterner`'s `Send` impl didn't respect its generic `HashBuilder` parameter. Fixes GitHub [issue #4][gh-issue-4].

## 0.6.2 - 2017/08/13

- added `shrink_to_fit` public method to `StringInterner` - (by artemshein)

## 0.6.1  - 2017/07/31

- fixed a bug that inserting non-owning string types (e.g. `str`) was broken due to dangling pointers (Thanks to artemshein for fixing it!)

## 0.6.0 - 2017/07/09

- added optional serde serialization and deserialization support
- more efficient and generic `PartialEq` implementation for `StringInterner`
- made `StringInterner` generic over `BuildHasher` to allow for custom hashers

## 0.5.0 - 2017/07/08

- added `IntoIterator` trait implementation for `StringInterner`
- greatly simplified iterator code

## 0.4.0 - 2017/05/20

- removed restrictive constraint for `Unsigned` for `Symbol`

## 0.3.3 - 2017/02/27

- added `Send` and `Sync` to `InternalStrRef` to make `StringInterner` itself `Send` and `Sync`

## 0.2.1 - 2017/02/10

## 0.2.0 - 2017/02/10

## 0.1.0 - 2017/02/06

[gh-issue-4]: (https://github.com/Robbepop/string-interner/issues/4)

[gh-user-koute]: https://github.com/koute
[gh-user-madklad]: https://github.com/matklad
