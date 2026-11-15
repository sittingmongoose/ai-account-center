/* Test shim for tests/spec/agent/key_store_review.py (Linux, LD_PRELOAD): logs every fsync() the
 * agent makes to AAC_SYNC_LOG, one line each: "file <path>" for a file, "dir <path> <state>" for a
 * folder, where <state> says whether AAC_SYNC_TARGET (the key file) already holds AAC_SYNC_SIZE
 * bytes, so the harness can check that the folder is synced after the rename. (The rename itself
 * goes through rustix's raw syscall in tempfile, so it is seen by its effect, not intercepted.)
 * AAC_SYNC_KILL=file or AAC_SYNC_KILL=dir ends the process with SIGKILL at that fsync, the way a
 * crash interrupts the write: before the rename, or after it. Never loaded outside that harness. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static void note(const char *line) {
  const char *log = getenv("AAC_SYNC_LOG");
  FILE *f = log ? fopen(log, "a") : NULL;
  if (f) {
    fputs(line, f);
    fputc('\n', f);
    fclose(f);
  }
}

int fsync(int fd) {
  static int (*real)(int);
  if (!real) real = (int (*)(int))dlsym(RTLD_NEXT, "fsync");
  char link[64], path[PATH_MAX], line[PATH_MAX + 64];
  snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
  ssize_t n = readlink(link, path, sizeof path - 1);
  path[n > 0 ? n : 0] = 0;
  struct stat st;
  int dir = fstat(fd, &st) == 0 && S_ISDIR(st.st_mode);
  const char *kill = getenv("AAC_SYNC_KILL");
  if (kill && strcmp(kill, dir ? "dir" : "file") == 0) {
    note(dir ? "killed dir" : "killed file");
    raise(SIGKILL);
  }
  if (dir) {
    const char *target = getenv("AAC_SYNC_TARGET");
    const char *size = getenv("AAC_SYNC_SIZE");
    struct stat t;
    int renamed = target && size && stat(target, &t) == 0 && t.st_size == atoll(size);
    snprintf(line, sizeof line, "dir %s %s", path, renamed ? "after-rename" : "before-rename");
  } else {
    snprintf(line, sizeof line, "file %s", path);
  }
  note(line);
  return real(fd);
}
