/* Disposable fixture executable, no vendor SDK/auth/network/model operations. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <limits.h>

int main(int argc, char **argv) {
    const char *root = getenv("FIXTURE_ROOT"), *cid = getenv("FIXTURE_CID");
    const char *sentinel = getenv("FIXTURE_PRIVATE");
    if (!root || !cid || !sentinel || strcmp(getcwd((char[PATH_MAX]){0}, PATH_MAX), root)) return 2;
    if (argc == 3 && !strcmp(argv[1], "--conversation")) cid = argv[2];
    else if (argc != 1) return 2;
    if (strlen(cid) != 36 || strspn(cid, "0123456789abcdef-") != 36) return 2;
    char path[PATH_MAX], account[512] = {0};
    snprintf(path, sizeof(path), "%s/fictional-account.json", root);
    FILE *auth = fopen(path, "r"); if (!auth) return 2;
    if (!fgets(account, sizeof(account), auth)) return 2;
    fclose(auth);
    int second = strstr(account, "second@example.com") != NULL;
    snprintf(path, sizeof(path), "%s/%s.db", root, cid);
    FILE *opened = fopen(path, "a+"); if (!opened) return 2;
    printf("{\"event\":\"ready\",\"email\":\"%s@example.com\",\"subject\":\"fictional-%s\","
           "\"conversationId\":\"%s\",\"cwd\":\"%s\",\"pty\":\"%s\",\"envSentinel\":\"%s\"}\n",
           second ? "second" : "first", second ? "second" : "first", cid, root, ttyname(0), sentinel);
    fflush(stdout);
    char input[16384];
    while (fgets(input, sizeof(input), stdin)) {
        if (!strcmp(input, "fixture-exit\n")) break;
        snprintf(path, sizeof(path), "%s/inputs", root);
        FILE *log = fopen(path, "a"); if (!log) return 2;
        fputs(input, log); fclose(log);
        puts("{\"event\":\"ready\"}"); fflush(stdout);
    }
    fclose(opened); return 0;
}
