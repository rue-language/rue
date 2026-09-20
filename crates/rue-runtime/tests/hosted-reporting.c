#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Independently pin the C side of the caller-owned v6 descriptor ABI. */
struct failure_site {
    const unsigned char *file;
    uint64_t file_len;
    uint64_t position;
};
struct failure_report {
    struct failure_site site;
    const unsigned char *kind;
    uint64_t kind_len;
    const unsigned char *first;
    uint64_t first_len;
    const unsigned char *second;
    uint64_t second_len;
};
_Static_assert(sizeof(struct failure_site) == 24, "failure site ABI");
_Static_assert(sizeof(struct failure_report) == 72, "failure report ABI");
extern _Noreturn void __rue_panic_no_msg(const unsigned char *, uint64_t, uint64_t);
extern _Noreturn void __rue_test_fail_assert(const struct failure_report *, uint32_t);
extern void __rue_test_normalize_process(void);
extern void __rue_test_complete(void);

static pthread_mutex_t gate = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
static int child_ready;
static int start;
static struct failure_report child_report;
static unsigned char left_message[5000];
static unsigned char right_message[4100];

static void require_success(int result) {
    if (result != 0) {
        _exit(86);
    }
}

static void *report_worker(void *unused) {
    (void)unused;
    require_success(pthread_mutex_lock(&gate));
    child_ready = 1;
    require_success(pthread_cond_signal(&changed));
    while (!start) {
        require_success(pthread_cond_wait(&changed, &gate));
    }
    require_success(pthread_mutex_unlock(&gate));
    __rue_test_fail_assert(&child_report, 1);
}

int run_hosted_reporting_probe(const char *mode) {
    static const unsigned char left_file[] = "left/\"thread\none.rue";
    static const unsigned char right_file[] = "a-different/right\\worker.rue";
    static const unsigned char kind[] = "assert";
    static const char sentinel[] = "caller-owned descriptor\n";
    const char *path = getenv("RUE_HOSTED_REPORT_FILE");
    if (!path) {
        return 81;
    }
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) {
        return 82;
    }
    if (fd != 3) {
        if (dup2(fd, 3) != 3) {
            return 83;
        }
        close(fd);
    }
    if (strcmp(mode, "panic-unarmed") == 0) {
        if (write(3, sentinel, sizeof(sentinel) - 1) != sizeof(sentinel) - 1) {
            return 84;
        }
        __rue_panic_no_msg(
            left_file, sizeof(left_file) - 1, ((uint64_t)111 << 32) | 7
        );
    }

    /* Both messages cross the 4 KiB bound and expand further when escaped. */
    memset(left_message, 'L', sizeof(left_message));
    memset(right_message, 'R', sizeof(right_message));
    memcpy(left_message, "left:\"\\\n", 8);
    memcpy(right_message, "right:\n\\\"", 9);
    const struct failure_report parent_report = {
        {left_file, sizeof(left_file) - 1, ((uint64_t)111 << 32) | 7},
        kind, sizeof(kind) - 1, left_message, sizeof(left_message), NULL, 0,
    };
    child_report = (struct failure_report){
        {right_file, sizeof(right_file) - 1, ((uint64_t)999 << 32) | 123},
        kind, sizeof(kind) - 1, right_message, sizeof(right_message), NULL, 0,
    };
    __rue_test_normalize_process();
    pthread_t worker;
    require_success(pthread_create(&worker, NULL, report_worker, NULL));
    require_success(pthread_mutex_lock(&gate));
    while (!child_ready) {
        require_success(pthread_cond_wait(&changed, &gate));
    }
    start = 1;
    require_success(pthread_cond_signal(&changed));
    require_success(pthread_mutex_unlock(&gate));
    if (strcmp(mode, "complete-race") == 0) {
        __rue_test_complete();
        require_success(pthread_join(worker, NULL));
        return 85; /* The child's terminal report must end the process. */
    }
    __rue_test_fail_assert(&parent_report, 1);
}
