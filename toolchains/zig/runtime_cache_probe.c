/*
 * The translation unit toolchains/zig:runtime-cache links, once per target
 * triple, so that Zig compiles its runtime support (the glibc start files and
 * non-shared stubs, libunwind, compiler_rt) into a global cache that every
 * later link action starts from.  Only that cache is kept; the executable the
 * link produces is thrown away, so the program's behaviour is irrelevant and
 * it should stay small enough to add nothing measurable to the link.
 */
int main(void) {
    return 0;
}
