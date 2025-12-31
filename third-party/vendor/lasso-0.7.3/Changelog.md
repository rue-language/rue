# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- next-header -->
## [Unreleased] - ReleaseDate

## [0.7.3] - 2024-08-19

### Changed

- Bumped MSRV to 1.71.0
- Updated hashbrown to 0.14.0
- Updated dashmap to 0.6.0

## [0.7.2] - 2023-05-15

## [0.7.1] - 2023-05-15

## [0.7.0] - 2023-04-02

### Changed

- Bumped MSRV to 1.61.0
- Updated to 2021 edition
- Interning empty strings no longer counts towards the memory limit of any interners. Empty strings now take up zero
  bytes and therefore will neither allocate nor cause methods to return errors or throw panics if the interner is out
  of memory.
- Changed `ThreadedRodeo` to use a new lockfree interner that no longer requires taking a mutex in order to allocate
  within it
- Updated DashMap and Hashbrown dependencies

### Added

- Added blanket implementations of `Reader` and `Resolver` for `&T` and `&mut T` references to types that implement
  those traits, and `Interner` likewise for `&mut T`
- Added `Clone` implementation for `Rodeo`
- Added the `Rodeo::try_clone()` and `Rodeo::try_clone_from()` functions

## [0.6.0] - 2021-09-01

### Changed

- Changed the `Debug` implementations of all key types to be more condensed

### Added

- Added an `.into_inner()` method to each key type that exposes the backing `NonZeroU*` it's made of

## [0.5.1] - 2021-06-01

### Fixed

- Fixed compile error in release mode when using the `serialize` feature

## [0.5.0] - 2021-02-19

### Added

- Implemented `Interner`, `Reader` and `Resolver` for `&ThreadedRodeo`
- Added optional implementations of `Abomonation` for key types under the `abomonation` feature flag
- Added optional implementations of `DeepSizeOf` for key types under the `deepsize` feature flag
- Added iterators for `ThreadedRodeo`

### Changed

- Moved the `.into_reader()` and `.into_resolver()` traits from `Interner` and `Reader` into the new `IntoReader`, `IntoResolver` and `IntoReaderAndResolver` traits
- Updated dependencies

### Fixed

- Fixed race condition on key insertion for `ThreadedRodeo`

## [0.4.1] - 2021-01-03

### Changed

- Updated dependencies to latest versions
- Made `Interner`, `Reader` and `Resolver` default their generic arguments to `Spur`

## [0.4.0] - 2021-01-02

### Added

- Added the `MemoryLimits` struct for creating memory limits on interners
- Gave `Rodeo` & `ThreadedRodeo` the ability to be given a hard memory capacity, currently only limiting the amount of memory allocated within the arena it uses
- Added the `with_memory_limits`, `with_capacity_and_memory_limits` and `with_capacity_memory_limits_and_hasher` methods to `Rodeo` & `ThreadedRodeo` for creating interners with memory limits
- Added `set_memory_limits` to `Rodeo` & `ThreadedRodeo` for in-flight modification of memory limits
- Added `current_memory_usage` and `max_memory_usage` methods to `Rodeo` & `ThreadedRodeo` for introspection of current memory usage maximum memory usage
- Added `FromIterator`, `Extend` and `IntoIterator` implementations for `Rodeo`
- Added `ExactSizeIterator` implementations for `Iter` and `Strings`
- Added `IntoIterator` implementations for `RodeoReader` and `RodeoResolver`
- Added the `inline-more` feature to enable inlining (off by default)
- Added `FromIterator` and `Extend` implementations to `ThreadedRodeo`
- Added the `.contains()` and the `.contains_key()` methods to `Rodeo`, `ThreadedRodeo`, `RodeoReader` and `RodeoResolver`
- Implemented `Serialize` and `Deserialize` for `Rodeo`, `ThreadedRodeo`, `RodeoReader` and `RodeoResolver`
- Added `Eq` and `PartialEq` implementations through the various interners
- Added `Index` implementations for all interners
- Added the `Interner`, `Reader` and `Resolver` traits
- Loosened trait bounds on `Rodeo` methods

### Changed

- Debug views of all interners now show their arenas
- Made `Key::into_usize` safe
- External apis are no longer `#[inline]` by default, for that use the `inline-more` feature
- `.get_or_intern()` and `.get_or_intern_static()` now return a `Result<T, LassoError>` to allow intelligently handling failure
- Bumped MSRV to 1.40.0
- Removed dependency on serde derive

## [0.3.1] - 2020-07-24

### Added

- Added the `get_or_intern_static` and `try_get_or_intern_static` methods to `ThreadedRodeo` (Thanks to [@jonas-schievink](https://github.com/Kixiron/lasso/pull/6))

### Fixed

- [Strange double-internment bug](https://github.com/Kixiron/lasso/issues/7)

## [0.3.0] - 2020-07-12

This version really wouldn't be possible without the amazing work of @CAD97. They were an amazing asset in [improving the memory efficiency](https://github.com/Kixiron/lasso/issues/4) of lasso and I can't thank them enough

### Added

- Added the `get_or_intern_static` and `try_get_or_intern_static` methods for zero-copy static string internment
- Added the `Capacity` struct for better internment estimates and more accurate pre-allocation

### Changed

- Lasso's single-threaded configuration now supports back to Rust 1.34! Thanks to @jyn514 for their [hard work](https://github.com/Kixiron/lasso/pull/3)!
- `Rodeo` and `RodeoReader` now use less memory since they only store their interned strings' pointers once
- `Rodeo` and `RodeoReader` use a different hashing strategy for their maps, instead of using a hashed string and key pair, they now use the key hashed *as* their paired string. This allows for decreased memory usage
- The arena backing all interners now increases the amount of memory it allocates exponentially (The same doubling strategy used by `Vec` is used). This allows for fewer allocations to happen as more strings are interned
- `hashbrown` is now a default dependency due to `HashMap`'s `raw_api` not being stable
- Relaxed trait bounds of many structs and functions
- Made custom `Debug` implementations to cut the excess and unneeded output
- Exported the `Strings` and `Iter` structs
- The `with_capacity` methods now use the `Capacity` struct

### Removed

- Removed the `hashbrown-table` and `nightly` features

## 0.2.4

### Added

- Added Serde support with the `serialize` feature

## 0.2.2

### Fixed

- Fixed `Send` for `Rodeo`

## 0.2.0

### Added

- Added single-threaded interner
- Added `try_get_or_intern`
- Added feature for `hashbrown`
- Added feature for `parking_lot`
- Added `no-std` feature
- Added `Key::try_from_usize`
- Added `MiniSpur`
- Added `MicroCord`
- Removed blanket impls for `u8`-`usize` & the nonzero  varieties
- Added lifetimes to all `Rodeo` types
- Added lifetime to `Key`
- Added the ID requirement to `Key`
- Added `try_resolve`s and `resolve_unchecked`s
- Added `strings()` and `iter()` methods to `Rodeo`, `RodeoResolver` and `RodeoReader`
- Strings are now allocated via an arena allocator

### Changed

- Renamed `Lasso` to `ThreadedRodeo`
- Renamed `ReadOnlyLasso` to `RodeoReader`
- Renamed `ResolverLasso` to `RodeoResolver`
- Changed default impl of `Key::from_usize`
- Added `Send` and `Sync` bounds for `ThreadedRodeo`, `RodeoResolver` and `RodeoReader`
- Changed internals of `get_or_intern` to be `try_get_or_intern.expect()`
- `multi-threaded` is now actually disabled by default

### Removed

- Removed `.intern` from all structs
- `Rodeo` and `ThreadedRodeo` no longer implement `Clone`

### Fixed

- Fixed memory leaks and possible unsoundness
- Fixed data races on `ThreadedRodeo`
- Fixed memory leaks in `ThreadedRodeo`, for real this time

## 0.1.2
## 0.1.1
## 0.1.0

<!-- next-url -->
[Unreleased]: https://github.com/Kixiron/lasso/compare/v0.7.3...HEAD
[0.7.3]: https://github.com/Kixiron/lasso/compare/v0.7.2...v0.7.3
[0.7.2]: https://github.com/Kixiron/lasso/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/Kixiron/lasso/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/Kixiron/lasso/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/Kixiron/lasso/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/Kixiron/lasso/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/Kixiron/lasso/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/Kixiron/lasso/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/Kixiron/lasso/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/Kixiron/lasso/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/Kixiron/lasso/compare/v0.3.0
