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
// The marker holds the delay's start and end times (CLOCK_REALTIME ms) and whether it ran on
// the process's first thread, which is also the event loop. A delay paid there cannot let a
// timer fire; a delay paid by a worker or the Rust child can.
static double realtime_ms(void) {
  struct timespec now;
  clock_gettime(CLOCK_REALTIME, &now);
  return (double)now.tv_sec * 1000.0 + (double)now.tv_nsec / 1000000.0;
}
static void write_marker(double start, double end) {
  const char *mark = getenv("AAC_SLOW_MARKER");
  if (!mark) return;
  FILE *f = fopen(mark, "w");
  if (!f) return;
  fprintf(
    f, "%.0f %.0f %d %d\n", start, end, (int)gettid() == (int)getpid(), (int)getpid()
  );
  fclose(f);
}
static void inject(long seconds, long nanoseconds) {
  double start = realtime_ms();
  write_marker(start, start);
  struct timespec wait = {seconds, nanoseconds};
  nanosleep(&wait, 0);
  write_marker(start, realtime_ms());
}
static void delay_for(const char *path) {
  const char *target = getenv("AAC_SLOW_STAT_PATH");
  if (!target || !path || strcmp(target, path) || atomic_exchange(&delayed, 1)) return;
  inject(6, 200000000);
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
  inject(3, 0);
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
