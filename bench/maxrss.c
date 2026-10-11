/* Runs a command and prints its peak resident set size in KiB. A tiny
 * parent keeps fork's inherited footprint out of the measurement. */
#include <stdio.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>
#include <fcntl.h>

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    pid_t p = fork();
    if (p == 0) {
        int fd = open("/dev/null", O_WRONLY);
        dup2(fd, 1);
        execvp(argv[1], argv + 1);
        _exit(127);
    }
    int st;
    struct rusage ru;
    wait4(p, &st, 0, &ru);
    printf("%ld\n", ru.ru_maxrss);
    return WIFEXITED(st) ? WEXITSTATUS(st) : 1;
}
