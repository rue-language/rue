#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>

struct worker_state {
    pthread_t creator;
    int worker_errno;
    int result;
    int distinct_worker;
    _Atomic int started;
};

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
