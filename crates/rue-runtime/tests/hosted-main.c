#include <stdint.h>
#include <string.h>

extern uint64_t __rue_arg_count(void);
extern unsigned char *__rue_arg_ptr(uint64_t index);
extern uint64_t __rue_arg_len(uint64_t index);
extern uint64_t __rue_env_count(void);
extern unsigned char *__rue_env_ptr(uint64_t index);
extern uint64_t __rue_env_len(uint64_t index);
extern int run_hosted_pthread_probe(void);
extern int run_hosted_reporting_probe(const char *mode);

int main(void) {
    static const char expected_argument[] = "hosted-argument";
    static const char expected_environment[] = "RUE_HOSTED_TEST=preserved";

    if (__rue_arg_count() == 2 &&
        (strcmp((const char *)__rue_arg_ptr(1), "panic-unarmed") == 0 ||
         strcmp((const char *)__rue_arg_ptr(1), "assert-race") == 0 ||
         strcmp((const char *)__rue_arg_ptr(1), "complete-race") == 0)) {
        return run_hosted_reporting_probe((const char *)__rue_arg_ptr(1));
    }

    if (__rue_arg_count() != 2 || __rue_arg_len(1) != sizeof(expected_argument) - 1 ||
        memcmp(__rue_arg_ptr(1), expected_argument, sizeof(expected_argument) - 1) != 0) {
        return 31;
    }

    for (uint64_t index = 0; index < __rue_env_count(); index++) {
        if (__rue_env_len(index) == sizeof(expected_environment) - 1 &&
            memcmp(__rue_env_ptr(index), expected_environment, sizeof(expected_environment) - 1) == 0) {
            return run_hosted_pthread_probe();
        }
    }
    return 32;
}
