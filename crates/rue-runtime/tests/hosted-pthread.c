#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <string.h>

struct worker_state {
    pthread_t creator;
    int worker_errno;
    int result;
    int distinct_worker;
    _Atomic int started;
};

typedef void (*rue_callback)(void *);
extern uint32_t __rue_join_inout(void *left_context, uintptr_t left_code,
                                 void *right_context, uintptr_t right_code);
extern unsigned char *__rue_alloc(uint64_t size, uint64_t align);
extern void __rue_free(unsigned char *ptr, uint64_t size, uint64_t align);
extern _Noreturn void __rue_panic_no_msg(const unsigned char *, uint64_t, uint64_t);

struct trap_barrier {
    _Atomic int worker_ready;
    _Atomic int parent_ready;
};

struct trap_side {
    struct trap_barrier *barrier;
};

static void trap_worker_panics(void *opaque) {
    struct trap_side *side = opaque;
    atomic_store_explicit(&side->barrier->worker_ready, 1, memory_order_release);
    while (!atomic_load_explicit(&side->barrier->parent_ready, memory_order_acquire)) {
    }
    __rue_panic_no_msg(NULL, 0, 0);
}

static void trap_parent_blocks(void *opaque) {
    struct trap_side *side = opaque;
    atomic_store_explicit(&side->barrier->parent_ready, 1, memory_order_release);
    for (;;) {
        (void)atomic_load_explicit(&side->barrier->parent_ready, memory_order_relaxed);
    }
}

static void trap_worker_blocks(void *opaque) {
    struct trap_side *side = opaque;
    atomic_store_explicit(&side->barrier->worker_ready, 1, memory_order_release);
    for (;;) {
        (void)atomic_load_explicit(&side->barrier->worker_ready, memory_order_relaxed);
    }
}

static void trap_parent_panics(void *opaque) {
    struct trap_side *side = opaque;
    atomic_store_explicit(&side->barrier->parent_ready, 1, memory_order_release);
    while (!atomic_load_explicit(&side->barrier->worker_ready, memory_order_acquire)) {
    }
    __rue_panic_no_msg(NULL, 0, 0);
}

int run_hosted_join_trap_probe(const char *mode) {
    static struct trap_barrier barrier;
    struct trap_side worker = {.barrier = &barrier};
    struct trap_side parent = {.barrier = &barrier};
    atomic_store_explicit(&barrier.worker_ready, 0, memory_order_relaxed);
    atomic_store_explicit(&barrier.parent_ready, 0, memory_order_relaxed);
    rue_callback worker_callback;
    rue_callback parent_callback;
    if (strcmp(mode, "join-worker-trap") == 0) {
        worker_callback = trap_worker_panics;
        parent_callback = trap_parent_blocks;
    } else {
        worker_callback = trap_worker_blocks;
        parent_callback = trap_parent_panics;
    }
    (void)__rue_join_inout(&worker, (uintptr_t)(rue_callback)worker_callback,
                           &parent, (uintptr_t)(rue_callback)parent_callback);
    return 86;
}

struct join_worker_state {
    _Atomic int worker_calls;
    _Atomic int entered;
    _Atomic int parent_entered;
    _Atomic int parent_allocated;
    _Atomic int worker_freed;
    int value;
    pthread_t creator;
    int distinct_worker;
    int allocation_ok;
    int allocation_value_ok;
    int nested_worker_ok;
    unsigned char *allocation;
};

struct join_parent_state {
    struct join_worker_state *worker;
    _Atomic int parent_calls;
    _Atomic int entered;
    int value;
    int nested_parent_ok;
};

struct nested_barrier {
    _Atomic int worker_entered;
    _Atomic int parent_entered;
};

struct nested_worker_state {
    struct nested_barrier *barrier;
    _Atomic int calls;
};

struct nested_parent_state {
    struct nested_barrier *barrier;
    _Atomic int calls;
};

static void nested_worker(void *opaque) {
    struct nested_worker_state *state = opaque;
    atomic_fetch_add_explicit(&state->calls, 1, memory_order_relaxed);
    atomic_store_explicit(&state->barrier->worker_entered, 1, memory_order_release);
    while (!atomic_load_explicit(&state->barrier->parent_entered, memory_order_acquire)) {
    }
}

static void nested_parent(void *opaque) {
    struct nested_parent_state *state = opaque;
    atomic_fetch_add_explicit(&state->calls, 1, memory_order_relaxed);
    atomic_store_explicit(&state->barrier->parent_entered, 1, memory_order_release);
    while (!atomic_load_explicit(&state->barrier->worker_entered, memory_order_acquire)) {
    }
}

static int run_nested_probe(struct nested_barrier *barrier,
                            struct nested_worker_state *worker,
                            struct nested_parent_state *parent) {
    worker->barrier = barrier;
    parent->barrier = barrier;
    worker->calls = 0;
    parent->calls = 0;
    return (int)__rue_join_inout(
        worker, (uintptr_t)(rue_callback)nested_worker,
        parent, (uintptr_t)(rue_callback)nested_parent);
}

static void join_worker(void *opaque) {
    struct join_worker_state *state = opaque;
    atomic_fetch_add_explicit(&state->worker_calls, 1, memory_order_relaxed);
    state->distinct_worker = !pthread_equal(state->creator, pthread_self());
    state->value = 101;
    atomic_store_explicit(&state->entered, 1, memory_order_release);
    while (!atomic_load_explicit(&state->parent_entered, memory_order_acquire)) {
    }
    while (!atomic_load_explicit(&state->parent_allocated, memory_order_acquire)) {
    }
    state->allocation_value_ok = state->value == 101 &&
                                state->allocation != NULL && state->allocation[0] == 0xa5;
    if (state->allocation != NULL) {
        __rue_free(state->allocation, 64, 8);
    }
    atomic_store_explicit(&state->worker_freed, 1, memory_order_release);

    struct nested_barrier barrier = {0};
    struct nested_worker_state nested_worker_state = {0};
    struct nested_parent_state nested_parent_state = {0};
    state->nested_worker_ok =
        run_nested_probe(&barrier, &nested_worker_state, &nested_parent_state) == 0 &&
        atomic_load_explicit(&nested_worker_state.calls, memory_order_acquire) == 1 &&
        atomic_load_explicit(&nested_parent_state.calls, memory_order_acquire) == 1;
}

static void join_parent(void *opaque) {
    struct join_parent_state *state = opaque;
    struct join_worker_state *worker = state->worker;
    atomic_fetch_add_explicit(&state->parent_calls, 1, memory_order_relaxed);
    atomic_store_explicit(&state->entered, 1, memory_order_release);
    atomic_store_explicit(&worker->parent_entered, 1, memory_order_release);
    while (!atomic_load_explicit(&worker->entered, memory_order_acquire)) {
    }
    unsigned char *allocation = __rue_alloc(64, 8);
    worker->allocation_ok = allocation != NULL;
    worker->allocation = allocation;
    if (allocation != NULL) {
        allocation[0] = 0xa5;
    }
    atomic_store_explicit(&worker->parent_allocated, 1, memory_order_release);
    while (!atomic_load_explicit(&worker->worker_freed, memory_order_acquire)) {
    }
    state->value = 202;

    struct nested_barrier barrier = {0};
    struct nested_worker_state nested_worker_state = {0};
    struct nested_parent_state nested_parent_state = {0};
    state->nested_parent_ok =
        run_nested_probe(&barrier, &nested_worker_state, &nested_parent_state) == 0 &&
        atomic_load_explicit(&nested_worker_state.calls, memory_order_acquire) == 1 &&
        atomic_load_explicit(&nested_parent_state.calls, memory_order_acquire) == 1;
}

/*
 * Both callbacks publish an entry point and wait for the other side before
 * proceeding. The parent allocates and the worker frees, so the Rue heap is
 * exercised across pthread ownership rather than only within one thread.
 */
int run_hosted_join_probe(void) {
    if (__rue_join_inout(NULL, (uintptr_t)(rue_callback)join_worker,
                         NULL, (uintptr_t)(rue_callback)join_parent) != 2) {
        return 46;
    }
    for (int iteration = 0; iteration < 32; iteration++) {
        struct join_worker_state worker = {
            .creator = pthread_self(),
            .entered = 0,
        };
        struct join_parent_state parent = {
            .worker = &worker,
        };
        uint32_t status = __rue_join_inout(
            &worker, (uintptr_t)(rue_callback)join_worker,
            &parent, (uintptr_t)(rue_callback)join_parent);
        if (status != 0 || atomic_load_explicit(&worker.worker_calls, memory_order_acquire) != 1 ||
            atomic_load_explicit(&parent.parent_calls, memory_order_acquire) != 1 ||
            !worker.distinct_worker || !worker.allocation_ok || !worker.allocation_value_ok ||
            worker.value != 101 || parent.value != 202 ||
            !atomic_load_explicit(&worker.entered, memory_order_acquire) ||
            !atomic_load_explicit(&parent.entered, memory_order_acquire) ||
            !worker.nested_worker_ok || !parent.nested_parent_ok) {
            return 45;
        }
    }
    return 0;
}

static void *worker_main(void *opaque) {
    struct worker_state *state = opaque;
    state->distinct_worker = !pthread_equal(state->creator, pthread_self());
    errno = ERANGE;
    state->worker_errno = errno;
    state->result = 73;
    atomic_store_explicit(&state->started, 1, memory_order_release);
    return (void *)(uintptr_t)73;
}

int run_hosted_pthread_probe(void) {
    for (int iteration = 0; iteration < 32; iteration++) {
        struct worker_state state = {
            .creator = pthread_self(),
            .started = 0,
        };
        errno = EAGAIN;

        pthread_t worker;
        if (pthread_create(&worker, NULL, worker_main, &state) != 0) {
            return 41;
        }
        while (!atomic_load_explicit(&state.started, memory_order_acquire)) {
        }
        if (!state.distinct_worker) {
            return 42;
        }

        void *result = NULL;
        if (pthread_join(worker, &result) != 0 || (uintptr_t)result != 73) {
            return 43;
        }
        if (state.result != 73 || state.worker_errno != ERANGE || errno != EAGAIN) {
            return 44;
        }
    }
    return 37;
}
