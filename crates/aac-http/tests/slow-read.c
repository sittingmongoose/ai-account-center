/* Test shim for crates/aac-http/tests/lifecycle.rs (Linux, LD_PRELOAD): every read() of the one
 * file named by AAC_SLOW_READ_PATH sleeps AAC_SLOW_READ_MS (default 1500) milliseconds, as a
 * stuck share or disk would. It appends "inflight <n>" to AAC_SLOW_READ_RECEIPT each time a
 * delayed read starts, n being how many delayed reads are asleep at once in this process, so the
 * test can prove both that the delay happened and how many blocking reads ever ran together.
 * Every other descriptor (the socket and the stdin pipe included) reads normally. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static ssize_t (*real_read)(int, void *, size_t);
static int inflight;

ssize_t read(int fd, void *buf, size_t count) {
  if (!real_read) real_read = (ssize_t (*)(int, void *, size_t))dlsym(RTLD_NEXT, "read");
  const char *target = getenv("AAC_SLOW_READ_PATH");
  if (target) {
    char link[64], path[PATH_MAX];
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof path - 1);
    if (n > 0) {
      path[n] = 0;
      if (strcmp(path, target) == 0) {
        int now = __atomic_add_fetch(&inflight, 1, __ATOMIC_SEQ_CST);
        const char *receipt = getenv("AAC_SLOW_READ_RECEIPT");
        FILE *f = receipt ? fopen(receipt, "a") : NULL;
        if (f) {
          fprintf(f, "inflight %d\n", now);
          fclose(f);
        }
        const char *ms = getenv("AAC_SLOW_READ_MS");
        usleep((useconds_t)(ms ? atoi(ms) : 1500) * 1000);
        __atomic_sub_fetch(&inflight, 1, __ATOMIC_SEQ_CST);
      }
    }
  }
  return real_read(fd, buf, count);
}
