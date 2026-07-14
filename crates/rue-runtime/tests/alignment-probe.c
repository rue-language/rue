#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ptrace.h>
#include <sys/types.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <unistd.h>

struct breakpoint {
    uintptr_t address;
    long original_word;
};

static void fail(const char *message) {
    perror(message);
    exit(1);
}

static void wait_for_stop(pid_t child, const char *boundary) {
    int status;
    if (waitpid(child, &status, 0) < 0) {
        fail("waitpid");
    }
    if (!WIFSTOPPED(status)) {
        fprintf(stderr, "%s: child did not stop (status=%d)\n", boundary, status);
        exit(1);
    }
}

static struct user_regs_struct registers(pid_t child) {
    struct user_regs_struct regs;
    if (ptrace(PTRACE_GETREGS, child, NULL, &regs) < 0) {
        fail("ptrace GETREGS");
    }
    return regs;
}

static struct breakpoint install_breakpoint(pid_t child, uintptr_t address) {
    errno = 0;
    long word = ptrace(PTRACE_PEEKTEXT, child, (void *)address, NULL);
    if (word == -1 && errno != 0) {
        fail("ptrace PEEKTEXT");
    }
    long trap_word = (word & ~0xffL) | 0xcc;
    if (ptrace(PTRACE_POKETEXT, child, (void *)address, (void *)trap_word) < 0) {
        fail("ptrace POKETEXT");
    }
    return (struct breakpoint){address, word};
}

static void observe_breakpoint(pid_t child, struct breakpoint breakpoint,
                               const char *boundary) {
    if (ptrace(PTRACE_CONT, child, NULL, NULL) < 0) {
        fail("ptrace CONT");
    }
    wait_for_stop(child, boundary);

    struct user_regs_struct regs = registers(child);
    if (regs.rip - 1 != breakpoint.address) {
        fprintf(stderr, "%s: unexpected trap at %#llx\n", boundary,
                (unsigned long long)(regs.rip - 1));
        exit(1);
    }
    if ((regs.rsp & 15) != 8) {
        fprintf(stderr, "%s: rsp %% 16 was %llu, expected 8\n", boundary,
                (unsigned long long)(regs.rsp & 15));
        exit(1);
    }

    if (ptrace(PTRACE_POKETEXT, child, (void *)breakpoint.address,
               (void *)breakpoint.original_word) < 0) {
        fail("ptrace restore POKETEXT");
    }
    regs.rip = breakpoint.address;
    if (ptrace(PTRACE_SETREGS, child, NULL, &regs) < 0) {
        fail("ptrace SETREGS");
    }
    if (ptrace(PTRACE_SINGLESTEP, child, NULL, NULL) < 0) {
        fail("ptrace SINGLESTEP");
    }
    wait_for_stop(child, boundary);
}

int main(int argc, char **argv) {
    if (argc != 5) {
        fprintf(stderr, "usage: %s PROGRAM START HELPER MAIN\n", argv[0]);
        return 1;
    }

    uintptr_t start_address = strtoull(argv[2], NULL, 16);
    uintptr_t helper_address = strtoull(argv[3], NULL, 16);
    uintptr_t main_address = strtoull(argv[4], NULL, 16);
    pid_t child = fork();
    if (child < 0) {
        fail("fork");
    }
    if (child == 0) {
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) < 0) {
            fail("ptrace TRACEME");
        }
        execl(argv[1], argv[1], NULL);
        fail("execl");
    }

    wait_for_stop(child, "raw _start");
    if (ptrace(PTRACE_SETOPTIONS, child, NULL, PTRACE_O_EXITKILL) < 0) {
        fail("ptrace SETOPTIONS");
    }
    struct user_regs_struct regs = registers(child);
    if (regs.rip != start_address) {
        fprintf(stderr, "initial instruction was %#llx, expected _start at %#llx\n",
                (unsigned long long)regs.rip,
                (unsigned long long)start_address);
        return 1;
    }
    if ((regs.rsp & 15) != 0) {
        fprintf(stderr, "raw _start: rsp %% 16 was %llu, expected 0\n",
                (unsigned long long)(regs.rsp & 15));
        return 1;
    }

    struct breakpoint helper = install_breakpoint(child, helper_address);
    struct breakpoint rue_main = install_breakpoint(child, main_address);
    observe_breakpoint(child, helper, "Rust entry helper");
    observe_breakpoint(child, rue_main, "Rue main");

    if (ptrace(PTRACE_CONT, child, NULL, NULL) < 0) {
        fail("ptrace final CONT");
    }
    int status;
    if (waitpid(child, &status, 0) < 0) {
        fail("final waitpid");
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 37) {
        fprintf(stderr, "Rue program did not exit 37 (status=%d)\n", status);
        return 1;
    }
    return 0;
}
