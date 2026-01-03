+++
title = "Putting It Together"
weight = 10
template = "tutorial/page.html"
+++

# Putting It Together

Let's combine everything we've learned into a complete program: Quicksort.

## The Algorithm

Quicksort is a classic divide-and-conquer sorting algorithm:
1. Pick a "pivot" element
2. Partition the array so elements less than the pivot come before it
3. Recursively sort the left and right partitions

## The Implementation

```rue
fn partition(inout arr: [i32; 5], lo: u64, hi: u64) -> u64 {
    let pivot = arr[hi];
    let mut i = lo;
    let mut j = lo;

    while j < hi {
        if arr[j] <= pivot {
            // Swap arr[i] and arr[j]
            let tmp = arr[i];
            arr[i] = arr[j];
            arr[j] = tmp;
            i = i + 1;
        }
        j = j + 1;
    }

    // Move pivot to its final position
    let tmp = arr[i];
    arr[i] = arr[hi];
    arr[hi] = tmp;
    i
}

fn quicksort(inout arr: [i32; 5], lo: u64, hi: u64) {
    if lo < hi {
        let p = partition(inout arr, lo, hi);
        if p > lo {
            quicksort(inout arr, lo, p - 1);
        }
        quicksort(inout arr, p + 1, hi);
    }
}

fn main() -> i32 {
    let mut nums = [64, 25, 12, 22, 11];

    // Print before sorting
    @dbg(nums[0]);
    @dbg(nums[1]);
    @dbg(nums[2]);
    @dbg(nums[3]);
    @dbg(nums[4]);

    quicksort(inout nums, 0, 4);

    // Print after sorting
    @dbg(0);  // separator
    @dbg(nums[0]);
    @dbg(nums[1]);
    @dbg(nums[2]);
    @dbg(nums[3]);
    @dbg(nums[4]);

    nums[0]  // Returns 11 (smallest)
}
```

## What This Demonstrates

This example uses almost everything from the tutorial:

- **Functions**: `partition` and `quicksort` with parameters and return values
- **Variables**: Both mutable (`let mut`) and immutable (`let`)
- **Control flow**: `if` conditions and `while` loops
- **Arrays**: Fixed-size arrays with indexing
- **Inout parameters**: Modifying the array in place
- **Recursion**: `quicksort` calls itself

## Running It

```bash
./buck2 run //crates/rue:rue -- quicksort.rue quicksort
./quicksort
```

Output:
```
64
25
12
22
11
0
11
12
22
25
64
```

The array is sorted!

## More Examples

The [GitHub repository](https://github.com/rue-language/rue) has more examples in the `examples/` directory:

- `fibonacci.rue` - Iterative and recursive Fibonacci
- `primes.rue` - Prime number sieve with trial division
- `binary_search.rue` - Binary search on a sorted array
- `quicksort.rue` - Full quicksort with 10-element arrays
- `structs.rue` - Working with Points and Rectangles

## Next Steps

You've learned the core of Rue! For the complete language reference, read the [Language Specification](/spec/).

Rue is still in early development. If you find bugs or have ideas, please [file an issue](https://github.com/rue-language/rue/issues)!
