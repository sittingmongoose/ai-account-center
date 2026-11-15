/* Test shim for tests/spec/agent/deadline_review.py (Linux, LD_PRELOAD): the first read() of the one
 * file named by AAC_SLOW_READ_PATH sleeps three seconds, as a slow disk or network home would, and
 * appends a line to AAC_SLOW_READ_RECEIPT so the harness can prove the delay really happened. Every
 * other descriptor (the protocol pipes included) reads normally. Never loaded outside that harness. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static ssize_t (*real_read)(int, void *, size_t);
static int delayed;

ssize_t read(int fd, void *buf, size_t count) {
  if (!real_read) real_read = (ssize_t (*)(int, void *, size_t))dlsym(RTLD_NEXT, "read");
  const char *target = getenv("AAC_SLOW_READ_PATH");
  if (target && !delayed) {
    char link[64], path[PATH_MAX];
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof path - 1);
    if (n > 0) {
      path[n] = 0;
      if (strcmp(path, target) == 0) {
        delayed = 1;
        const char *receipt = getenv("AAC_SLOW_READ_RECEIPT");
        FILE *f = receipt ? fopen(receipt, "a") : NULL;
        if (f) {
          fputs("delayed\n", f);
          fclose(f);
        }
        sleep(3);
      }
    }
  }
  return real_read(fd, buf, count);
}
