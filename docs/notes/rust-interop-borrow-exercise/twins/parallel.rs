// Twin of std/parallel.rue's `join_inout` and examples/mosaic_parallel.rue.
//
// Today `join_inout` is a compiler intrinsic (`@join_inout`) plus
// `@require_transferable`. Over Rust's std it is a C1-signature wrapper around
// `std::thread::scope`, with `Send` standing in for "transferable":
//
//   R1  the scope closure and both spawned closures capture `&mut` locals;
//       Rue code only *calls* `thread::scope` (whose own signature names
//       `'env`/`'scope`, a C4 shape Rue could not declare, but can call).
//   R2  `join_inout` itself is borrow-in, nothing-out.
//
// Second-class `fn(&mut A)` callbacks (ADR-0096) are used as-is; the closures
// are only needed inside the wrapper.

use std::io::Write;
use std::sync::Mutex;

fn out(s: &str) {
    let _ = std::io::stdout().write_all(s.as_bytes());
}

pub fn join_inout<A: Send, B: Send>(left: &mut A, right: &mut B, left_fn: fn(&mut A), right_fn: fn(&mut B)) {
    std::thread::scope(|s| {
        s.spawn(|| left_fn(left));
        right_fn(right);
    });
}

pub struct SiteJob {
    pages: u64,
    report: Vec<u64>,
}

fn process_job(job: &mut SiteJob) {
    let mut acc = 0u64;
    for p in 0..job.pages {
        acc = acc.wrapping_mul(31).wrapping_add(p * p);
        job.report.push(acc % 1000);
    }
}

// A data-parallel version the Rue std has no spelling for today: split one
// slice into disjoint `&mut` chunks and hand each to a scoped thread.
fn parallel_square(xs: &mut [u64], workers: usize) {
    let chunk = xs.len().div_ceil(workers).max(1);
    std::thread::scope(|s| {
        for part in xs.chunks_mut(chunk) {
            // R1: each `part` is a `&mut [u64]` borrowed from `xs`
            s.spawn(move || {
                for x in part.iter_mut() {
                    *x *= *x;
                }
            });
        }
    });
}

fn main() {
    let mut serial_l = SiteJob { pages: 100, report: Vec::new() };
    let mut serial_r = SiteJob { pages: 140, report: Vec::new() };
    process_job(&mut serial_l);
    process_job(&mut serial_r);

    let mut l = SiteJob { pages: 100, report: Vec::new() };
    let mut r = SiteJob { pages: 140, report: Vec::new() };
    join_inout(&mut l, &mut r, process_job, process_job);
    let same = l.report == serial_l.report && r.report == serial_r.report;

    let mut xs: Vec<u64> = (1..=10).collect();
    parallel_square(&mut xs, 3);
    let sum: u64 = xs.iter().sum();

    // A lock guard is a lifetime-carrying value held in a local (R1).
    let total = Mutex::new(0u64);
    std::thread::scope(|s| {
        for k in 1..=4u64 {
            let total = &total;
            s.spawn(move || {
                let mut g = total.lock().unwrap(); // MutexGuard<'_, u64>
                *g += k;
            });
        }
    });
    let t = *total.lock().unwrap();

    out(&("same=".to_string() + if same { "true" } else { "false" } + " squares=" + &sum.to_string()
        + " total=" + &t.to_string() + "\n"));
    if !same || sum != 385 || t != 10 {
        std::process::exit(1);
    }
}
