#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/stat.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <time.h>
#include <stdatomic.h>
// Fixture-only storage latency injection; never changes syscall results or permissions.
static atomic_int delayed = 0;
static void slow_read(int fd);
static void delay_for(const char *path) {
  const char *target = getenv("AAC_SLOW_STAT_PATH");
  if (!target || !path || strcmp(target, path) || atomic_exchange(&delayed, 1)) return;
  const char *mark = getenv("AAC_SLOW_MARKER");
  if (mark) {
    FILE *f = fopen(mark, "w");
    if (f) { fputs("delayed", f); fclose(f); }
  }
  struct timespec wait = {6, 200000000};
  nanosleep(&wait, 0);
}
int statx(int dirfd, const char *pathname, int flags, unsigned int mask, struct statx *buf) {
  static int (*real)(int,const char*,int,unsigned int,struct statx*);
  if (!real) real = dlsym(RTLD_NEXT, "statx");
  delay_for(pathname);
  return real(dirfd, pathname, flags, mask, buf);
}
long syscall(long number, ...) {
  static long (*real)(long,...);
  if (!real) real = dlsym(RTLD_NEXT, "syscall");
  va_list args;
  va_start(args, number);
  long a[6];
  for (int i=0; i<6; ++i) a[i] = va_arg(args, long);
  va_end(args);
  if (number == SYS_statx || number == SYS_newfstatat) delay_for((const char*)a[1]);
  return real(number, a[0], a[1], a[2], a[3], a[4], a[5]);
}

static void slow_read(int fd) {
  const char *target = getenv("AAC_SLOW_READ_PATH");
  if (!target) return;
  char link[64], name[4096];
  snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
  ssize_t size = readlink(link, name, sizeof(name)-1);
  if (size < 0) return;
  name[size] = 0;
  if (strcmp(target, name) || atomic_exchange(&delayed, 1)) return;
  const char *mark = getenv("AAC_SLOW_MARKER");
  if (mark) {
    FILE *f = fopen(mark, "w");
    if (f) { fputs("delayed", f); fclose(f); }
  }
  struct timespec wait = {1, 300000000};
  nanosleep(&wait, 0);
}
ssize_t read(int fd, void *buf, size_t count) {
  static ssize_t (*real)(int,void*,size_t);
  if (!real) real = dlsym(RTLD_NEXT, "read");
  slow_read(fd);
  return real(fd, buf, count);
}
ssize_t pread64(int fd, void *buf, size_t count, off64_t offset) {
  static ssize_t (*real)(int,void*,size_t,off64_t);
  if (!real) real = dlsym(RTLD_NEXT, "pread64");
  slow_read(fd);
  return real(fd, buf, count, offset);
}
