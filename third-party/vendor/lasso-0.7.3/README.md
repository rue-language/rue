[![CI][1]][0]
[![Security Audit][2]][0]
[![Coverage][3]][4]
[![Docs.rs][6]][7]
[![Crates.io][8]][9]

A multithreaded and single threaded [string interner](https://en.wikipedia.org/wiki/String_interning) that allows strings to be cached with a minimal memory footprint,
associating them with a unique [key] that can be used to retrieve them at any time. A [`Rodeo`] allows `O(1)`
internment and resolution and can be turned into a [`RodeoReader`] to allow for contention-free resolutions
with both key to str and str to key operations. It can also be turned into a [`RodeoResolver`] with only
key to str operations for the lowest possible memory usage.

## Which interner do I use?

For single-threaded workloads [`Rodeo`] is encouraged, while multi-threaded applications should use [`ThreadedRodeo`].
Both of these are the only way to intern strings, but most applications will hit a stage where they are done interning
strings, and at that point is where the choice between [`RodeoReader`] and [`RodeoResolver`]. If the user needs to get
keys for strings still, then they must use the [`RodeoReader`] (although they can still transfer into a  [`RodeoResolver`])
at this point. For users who just need key to string resolution, the [`RodeoResolver`] gives contention-free access at the
minimum possible memory usage. Note that to gain access to [`ThreadedRodeo`] the `multi-threaded` feature is required.

| Interner          | Thread-safe | Intern String | str to key | key to str | Contention Free | Memory Usage |
| ----------------- | :---------: | :-----------: | :--------: | :--------: | :-------------: | :----------: |
| [`Rodeo`]         |      ❌      |       ✅       |     ✅      |     ✅      |       N/A       |    Medium    |
| [`ThreadedRodeo`] |      ✅      |       ✅       |     ✅      |     ✅      |        ❌        |     Most     |
| [`RodeoReader`]   |      ✅      |       ❌       |     ✅      |     ✅      |        ✅        |    Medium    |
| [`RodeoResolver`] |      ✅      |       ❌       |     ❌      |     ✅      |        ✅        |    Least     |

## Cargo Features

By default `lasso` has one dependency, `hashbrown`, and only [`Rodeo`] is exposed. Hashbrown is used since the [`raw_entry` api] is currently unstable in the standard library's hashmap.
The raw hashmap API is used for custom hashing within the hashmaps, which works to dramatically reduce memory usage
To make use of [`ThreadedRodeo`], you must enable the `multi-threaded` feature.

* `multi-threaded` - Enables [`ThreadedRodeo`], the interner for multi-threaded tasks
* `ahasher` - Use [`ahash`]'s `RandomState` as the default hasher
* `no-std` - Enables `no_std` + `alloc` support for [`Rodeo`] and [`ThreadedRodeo`]
  * Automatically enables the following required features:
    * `ahasher` - `no_std` hashing function
* `serialize` - Implements `Serialize` and `Deserialize` for all `Spur` types and all interners
* `inline-more` - Annotate external apis with `#[inline]`

## Example: Using Rodeo

```rust
use lasso::Rodeo;

let mut rodeo = Rodeo::default();
let key = rodeo.get_or_intern("Hello, world!");

// Easily retrieve the value of a key and find the key for values
assert_eq!("Hello, world!", rodeo.resolve(&key));
assert_eq!(Some(key), rodeo.get("Hello, world!"));

// Interning the same string again will yield the same key
let key2 = rodeo.get_or_intern("Hello, world!");

assert_eq!(key, key2);
```

## Example: Using ThreadedRodeo

```rust
use lasso::ThreadedRodeo;
use std::{thread, sync::Arc};

let rodeo = Arc::new(ThreadedRodeo::default());
let key = rodeo.get_or_intern("Hello, world!");

// Easily retrieve the value of a key and find the key for values
assert_eq!("Hello, world!", rodeo.resolve(&key));
assert_eq!(Some(key), rodeo.get("Hello, world!"));

// Interning the same string again will yield the same key
let key2 = rodeo.get_or_intern("Hello, world!");

assert_eq!(key, key2);

// ThreadedRodeo can be shared across threads
let moved = Arc::clone(&rodeo);
let hello = thread::spawn(move || {
    assert_eq!("Hello, world!", moved.resolve(&key));
    moved.get_or_intern("Hello from the thread!")
})
.join()
.unwrap();

assert_eq!("Hello, world!", rodeo.resolve(&key));
assert_eq!("Hello from the thread!", rodeo.resolve(&hello));
```

## Example: Creating a RodeoReader

```rust
use lasso::Rodeo;

// Rodeo and ThreadedRodeo are interchangeable here
let mut rodeo = Rodeo::default();

let key = rodeo.get_or_intern("Hello, world!");
assert_eq!("Hello, world!", rodeo.resolve(&key));

let reader = rodeo.into_reader();

// Reader keeps all the strings from the parent
assert_eq!("Hello, world!", reader.resolve(&key));
assert_eq!(Some(key), reader.get("Hello, world!"));

// The Reader can now be shared across threads, no matter what kind of Rodeo created it
```

## Example: Creating a RodeoResolver

```rust
use lasso::Rodeo;

// Rodeo and ThreadedRodeo are interchangeable here
let mut rodeo = Rodeo::default();

let key = rodeo.get_or_intern("Hello, world!");
assert_eq!("Hello, world!", rodeo.resolve(&key));

let resolver = rodeo.into_resolver();

// Resolver keeps all the strings from the parent
assert_eq!("Hello, world!", resolver.resolve(&key));

// The Resolver can now be shared across threads, no matter what kind of Rodeo created it
```

## Example: Making a custom-ranged key

Sometimes you want your keys to only inhabit (or *not* inhabit) a certain range of values so that you can have custom [niches].
This allows you to pack more data into what would otherwise be unused space, which can be critical for memory-sensitive applications.

```rust
use lasso::{Key, Rodeo};

// First make our key type, this will be what we use as handles into our interner
#[derive(Copy, Clone, PartialEq, Eq)]
struct NicheKey(u32);

// This will reserve the upper 255 values for us to use as niches
const NICHE: usize = 0xFF000000;

// Implementing `Key` is unsafe and requires that anything given to `try_from_usize` must produce the
// same `usize` when `into_usize` is later called
unsafe impl Key for NicheKey {
    fn into_usize(self) -> usize {
        self.0 as usize
    }

    fn try_from_usize(int: usize) -> Option<Self> {
        if int < NICHE {
            // The value isn't in our niche range, so we're good to go
            Some(Self(int as u32))
        } else {
            // The value interferes with our niche, so we return `None`
            None
        }
    }
}

// To make sure we're upholding `Key`'s safety contract, let's make two small tests
#[test]
fn value_in_range() {
    let key = NicheKey::try_from_usize(0).unwrap();
    assert_eq!(key.into_usize(), 0);

    let key = NicheKey::try_from_usize(NICHE - 1).unwrap();
    assert_eq!(key.into_usize(), NICHE - 1);
}

#[test]
fn value_out_of_range() {
    let key = NicheKey::try_from_usize(NICHE);
    assert!(key.is_none());

    let key = NicheKey::try_from_usize(u32::max_value() as usize);
    assert!(key.is_none());
}

// And now we're done and can make `Rodeo`s or `ThreadedRodeo`s that use our custom key!
let mut rodeo: Rodeo<NicheKey> = Rodeo::new();
let key = rodeo.get_or_intern("It works!");
assert_eq!(rodeo.resolve(&key), "It works!");
```

## Example: Creation using `FromIterator`

```rust
use lasso::Rodeo;
use core::iter::FromIterator;

// Works for both `Rodeo` and `ThreadedRodeo`
let rodeo = Rodeo::from_iter(vec![
    "one string",
    "two string",
    "red string",
    "blue string",
]);

assert!(rodeo.contains("one string"));
assert!(rodeo.contains("two string"));
assert!(rodeo.contains("red string"));
assert!(rodeo.contains("blue string"));
```

```rust
use lasso::Rodeo;
use core::iter::FromIterator;

// Works for both `Rodeo` and `ThreadedRodeo`
let rodeo: Rodeo = vec!["one string", "two string", "red string", "blue string"]
    .into_iter()
    .collect();

assert!(rodeo.contains("one string"));
assert!(rodeo.contains("two string"));
assert!(rodeo.contains("red string"));
assert!(rodeo.contains("blue string"));
```

## Benchmarks

Benchmarks were gathered with [Criterion.rs](https://github.com/bheisler/criterion.rs)  
OS: Windows 10  
CPU: Ryzen 9 3900X at 3800Mhz  
RAM: 3200Mhz  
Rustc: Stable 1.44.1  

### Rodeo

#### STD RandomState

| Method                       |   Time    |  Throughput  |
| :--------------------------- | :-------: | :----------: |
| `resolve`                    | 1.9251 μs | 13.285 GiB/s |
| `try_resolve`                | 1.9214 μs | 13.311 GiB/s |
| `resolve_unchecked`          | 1.4356 μs | 17.816 GiB/s |
| `get_or_intern` (empty)      | 60.350 μs | 433.96 MiB/s |
| `get_or_intern` (filled)     | 57.415 μs | 456.15 MiB/s |
| `try_get_or_intern` (empty)  | 58.978 μs | 444.06 MiB/s |
| `try_get_or_intern (filled)` | 57.421 μs | 456.10 MiB/s |
| `get` (empty)                | 37.288 μs | 702.37 MiB/s |
| `get` (filled)               | 55.095 μs | 475.36 MiB/s |

#### AHash

| Method                       |   Time    |  Throughput  |
| :--------------------------- | :-------: | :----------: |
| `try_resolve`                | 1.9282 μs | 13.264 GiB/s |
| `resolve`                    | 1.9404 μs | 13.181 GiB/s |
| `resolve_unchecked`          | 1.4328 μs | 17.851 GiB/s |
| `get_or_intern` (empty)      | 38.029 μs | 688.68 MiB/s |
| `get_or_intern` (filled)     | 33.650 μs | 778.30 MiB/s |
| `try_get_or_intern` (empty)  | 39.392 μs | 664.84 MiB/s |
| `try_get_or_intern (filled)` | 33.435 μs | 783.31 MiB/s |
| `get` (empty)                | 12.565 μs | 2.0356 GiB/s |
| `get` (filled)               | 26.545 μs | 986.61 MiB/s |

#### FXHash

| Method                       |   Time    |  Throughput  |
| :--------------------------- | :-------: | :----------: |
| `resolve`                    | 1.9014 μs | 13.451 GiB/s |
| `try_resolve`                | 1.9278 μs | 13.267 GiB/s |
| `resolve_unchecked`          | 1.4449 μs | 17.701 GiB/s |
| `get_or_intern` (empty)      | 32.523 μs | 805.27 MiB/s |
| `get_or_intern` (filled)     | 30.281 μs | 864.88 MiB/s |
| `try_get_or_intern` (empty)  | 31.630 μs | 828.00 MiB/s |
| `try_get_or_intern (filled)` | 31.002 μs | 844.78 MiB/s |
| `get` (empty)                | 12.699 μs | 2.0141 GiB/s |
| `get` (filled)               | 29.220 μs | 896.28 MiB/s |


### ThreadedRodeo

#### STD RandomState

| Method                       | Time (1 Thread) | Throughput (1 Thread) | Time (24 Threads) | Throughput (24 Threads) |
| :--------------------------- | :-------------: | :-------------------: | :---------------: | :---------------------: |
| `resolve`                    |    54.336 μs    |     482.00 MiB/s      |     364.27 μs     |      71.897 MiB/s       |
| `try_resolve`                |    54.582 μs    |     479.82 MiB/s      |     352.67 μs     |      74.261 MiB/s       |
| `get_or_intern` (empty)      |    266.03 μs    |     98.447 MiB/s      |        N\A        |           N\A           |
| `get_or_intern` (filled)     |    103.04 μs    |     254.17 MiB/s      |     441.42 μs     |      59.331 MiB/s       |
| `try_get_or_intern` (empty)  |    261.80 μs    |     100.04 MiB/s      |        N\A        |           N\A           |
| `try_get_or_intern` (filled) |    102.61 μs    |     255.25 MiB/s      |     447.42 μs     |      58.535 MiB/s       |
| `get` (empty)                |    80.346 μs    |     325.96 MiB/s      |        N\A        |           N\A           |
| `get` (filled)               |    92.669 μs    |     282.62 MiB/s      |     439.24 μs     |      59.626 MiB/s       |

#### AHash

| Method                       | Time (1 Thread) | Throughput (1 Thread) | Time (24 Threads) | Throughput (24 Threads) |
| :--------------------------- | :-------------: | :-------------------: | :---------------: | :---------------------: |
| `resolve`                    |    22.261 μs    |     1.1489 GiB/s      |     265.46 μs     |      98.658 MiB/s       |
| `try_resolve`                |    22.378 μs    |     1.1429 GiB/s      |     268.58 μs     |      97.513 MiB/s       |
| `get_or_intern` (empty)      |    157.86 μs    |     165.91 MiB/s      |        N\A        |           N\A           |
| `get_or_intern` (filled)     |    56.320 μs    |     465.02 MiB/s      |     357.13 μs     |      73.335 MiB/s       |
| `try_get_or_intern` (empty)  |    161.46 μs    |     162.21 MiB/s      |        N\A        |           N\A           |
| `try_get_or_intern` (filled) |    55.874 μs    |     468.73 MiB/s      |     360.25 μs     |      72.698 MiB/s       |
| `get` (empty)                |    43.520 μs    |     601.79 MiB/s      |        N\A        |           N\A           |
| `get` (filled)               |    53.720 μs    |     487.52 MiB/s      |     360.66 μs     |      72.616 MiB/s       |

#### FXHash

| Method                       | Time (1 Thread) | Throughput (1 Thread) | Time (24 Threads) | Throughput (24 Threads) |
| :--------------------------- | :-------------: | :-------------------: | :---------------: | :---------------------: |
| `try_resolve`                |    17.289 μs    |     1.4794 GiB/s      |     238.29 μs     |      109.91 MiB/s       |
| `resolve`                    |    19.833 μs    |     1.2896 GiB/s      |     237.05 μs     |      110.48 MiB/s       |
| `get_or_intern` (empty)      |    130.97 μs    |     199.97 MiB/s      |        N\A        |           N\A           |
| `get_or_intern` (filled)     |    42.630 μs    |     614.35 MiB/s      |     301.60 μs     |      86.837 MiB/s       |
| `try_get_or_intern` (empty)  |    129.30 μs    |     202.55 MiB/s      |        N\A        |           N\A           |
| `try_get_or_intern` (filled) |    42.508 μs    |     616.12 MiB/s      |     337.29 μs     |      77.648 MiB/s       |
| `get` (empty)                |    28.001 μs    |     935.30 MiB/s      |        N\A        |           N\A           |
| `get` (filled)               |    37.700 μs    |     694.68 MiB/s      |     292.15 μs     |      89.645 MiB/s       |

### RodeoReader

#### STD RandomState

| Method              | Time (1 Thread) | Throughput (1 Thread) | Time (24 Threads) | Throughput  (24 Threads) |
| :------------------ | :-------------: | :-------------------: | :---------------: | :----------------------: |
| `resolve`           |    1.9398 μs    |     13.185 GiB/s      |     4.3153 μs     |       5.9269 GiB/s       |
| `try_resolve`       |    1.9315 μs    |     13.242 GiB/s      |     4.1956 μs     |       6.0959 GiB/s       |
| `resolve_unchecked` |    1.4416 μs    |     17.741 GiB/s      |     3.1204 μs     |       8.1964 GiB/s       |
| `get` (empty)       |    38.886 μs    |     673.50 MiB/s      |        N\A        |           N\A            |
| `get` (filled)      |    56.271 μs    |     465.42 MiB/s      |     105.12 μs     |       249.14 MiB/s       |

#### AHash

| Method              | Time (1 Thread) | Throughput (1 Thread) | Time (24 Threads) | Throughput (24 Threads) |
| :------------------ | :-------------: | :-------------------: | :---------------: | :---------------------: |
| `resolve`           |    1.9404 μs    |     13.181 GiB/s      |     4.1881 μs     |      6.1069 GiB/s       |
| `try_resolve`       |    1.8932 μs    |     13.509 GiB/s      |     4.2410 μs     |      6.0306 GiB/s       |
| `resolve_unchecked` |    1.4128 μs    |     18.103 GiB/s      |     3.1691 μs     |      8.0703 GiB/s       |
| `get` (empty)       |    11.952 μs    |     2.1399 GiB/s      |        N\A        |           N\A           |
| `get` (filled)      |    27.093 μs    |     966.65 MiB/s      |     56.269 μs     |      465.44 MiB/s       |

#### FXHash

| Method              | Time (1 Thread) | Throughput (1 Thread) | Time (24 Threads) | Throughput (24 Threads) |
| :------------------ | :-------------: | :-------------------: | :---------------: | :---------------------: |
| `resolve`           |    1.8987 μs    |     13.471 GiB/s      |     4.2117 μs     |      6.0727 GiB/s       |
| `try_resolve`       |    1.9103 μs    |     13.389 GiB/s      |     4.2254 μs     |      6.0529 GiB/s       |
| `resolve_unchecked` |    1.4469 μs    |     17.677 GiB/s      |     3.0923 μs     |      8.2709 GiB/s       |
| `get` (empty)       |    12.994 μs    |     1.9682 GiB/s      |        N\A        |           N\A           |
| `get` (filled)      |    29.745 μs    |     880.49 MiB/s      |     52.387 μs     |      499.93 MiB/s       |

### RodeoResolver

| Method              | Time (1 Thread) | Throughput (1 Thread) | Time (24 Threads) | Throughput (24 Threads) |
| :------------------ | :-------------: | :-------------------: | :---------------: | :---------------------: |
| `resolve`           |    1.9416 μs    |     13.172 GiB/s      |     3.9114 μs     |      6.5387 GiB/s       |
| `try_resolve`       |    1.9264 μs    |     13.277 GiB/s      |     3.9289 μs     |      6.5097 GiB/s       |
| `resolve_unchecked` |    1.6638 μs    |     15.372 GiB/s      |     3.1741 μs     |      8.0578 GiB/s       |

[0]: https://github.com/Kixiron/lasso
[1]: https://github.com/Kixiron/lasso/workflows/CI/badge.svg
[2]: https://github.com/Kixiron/lasso/workflows/Security%20Audit/badge.svg
[3]: https://coveralls.io/repos/github/Kixiron/lasso/badge.svg?branch=master
[4]: https://coveralls.io/github/Kixiron/lasso?branch=master
[6]: https://docs.rs/lasso/badge.svg
[7]: https://docs.rs/lasso
[8]: https://img.shields.io/crates/v/lasso.svg
[9]: https://crates.io/crates/lasso
[key]: crate::Key
[`Rodeo`]: crate::Rodeo
[`ThreadedRodeo`]: crate::ThreadedRodeo
[`RodeoResolver`]: crate::RodeoResolver
[`RodeoReader`]: crate::RodeoReader
[`hashbrown`]: https://crates.io/crates/hashbrown
[`ahash`]: https://crates.io/crates/ahash
[`raw_entry` api]: https://github.com/rust-lang/rust/issues/56167
[niches]: https://rust-lang.github.io/unsafe-code-guidelines/glossary.html#niche
