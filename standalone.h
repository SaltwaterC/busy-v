/* Small POSIX compatibility layer for the extracted vi editor. */
#ifndef BUSY_V_H
#define BUSY_V_H

#define _GNU_SOURCE
#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <inttypes.h>
#include <poll.h>
#include <regex.h>
#include <signal.h>
#include <setjmp.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <termios.h>
#include <unistd.h>

#define KEYCODE_UP        (-2)
#define KEYCODE_DOWN      (-3)
#define KEYCODE_RIGHT     (-4)
#define KEYCODE_LEFT      (-5)
#define KEYCODE_HOME      (-6)
#define KEYCODE_END       (-7)
#define KEYCODE_INSERT    (-8)
#define KEYCODE_DELETE    (-9)
#define TERMIOS_RAW_CRNL_INPUT  (1 << 1)
#define TERMIOS_RAW_CRNL_OUTPUT (1 << 2)
#define TERMIOS_RAW_CRNL (TERMIOS_RAW_CRNL_INPUT | TERMIOS_RAW_CRNL_OUTPUT)

typedef struct llist_t {
	struct llist_t *link;
	char *data;
} llist_t;

static int standalone_argc;

static void *xmalloc(size_t n)
{
	void *p = malloc(n ? n : 1);
	if (!p) { perror("vi: malloc"); exit(EXIT_FAILURE); }
	return p;
}

static void *xzalloc(size_t n)
{
	void *p = calloc(1, n ? n : 1);
	if (!p) { perror("vi: calloc"); exit(EXIT_FAILURE); }
	return p;
}

static void *xrealloc(void *p, size_t n)
{
	p = realloc(p, n ? n : 1);
	if (!p) { perror("vi: realloc"); exit(EXIT_FAILURE); }
	return p;
}

static char *xstrdup(const char *s)
{
	char *p = strdup(s);
	if (!p) { perror("vi: strdup"); exit(EXIT_FAILURE); }
	return p;
}

static char *xstrndup(const char *s, size_t n)
{
	char *p = strndup(s, n);
	if (!p) { perror("vi: strndup"); exit(EXIT_FAILURE); }
	return p;
}

static char *xasprintf(const char *fmt, ...)
{
	va_list ap;
	char *p;
	va_start(ap, fmt);
	if (vasprintf(&p, fmt, ap) < 0) { va_end(ap); perror("vi: vasprintf"); exit(EXIT_FAILURE); }
	va_end(ap);
	return p;
}

static char *xasprintf_and_free(char *old, const char *fmt, ...)
{
	va_list ap;
	char *p;
	va_start(ap, fmt);
	if (vasprintf(&p, fmt, ap) < 0) { va_end(ap); perror("vi: vasprintf"); exit(EXIT_FAILURE); }
	va_end(ap);
	free(old);
	return p;
}
#define xasprintf_inplace(dst, ...) ((dst) = xasprintf_and_free((dst), __VA_ARGS__))

static void vi_error_and_die(const char *msg)
{
	fprintf(stderr, "vi: %s\n", msg);
	exit(EXIT_FAILURE);
}

static void vi_show_usage(void)
{
	fputs("usage: vi [-c command] [-R] [-H] [file ...]\n", stderr);
}

static int vi_putchar(int c) { return putchar(c); }
static int fputs_stdout(const char *s) { return fputs(s, stdout); }
static int fflush_all(void) { return fflush(NULL); }

static ssize_t safe_read(int fd, void *buf, size_t len)
{
	ssize_t n;
	do n = read(fd, buf, len); while (n < 0 && errno == EINTR);
	return n;
}

static ssize_t full_read(int fd, void *buf, size_t len)
{
	ssize_t total = 0;
	while (len) {
		ssize_t n = safe_read(fd, buf, len);
		if (n < 0) return total ? total : n;
		if (!n) break;
		total += n;
		buf = (char *)buf + n;
		len -= n;
	}
	return total;
}

static ssize_t full_write(int fd, const void *buf, size_t len)
{
	ssize_t total = 0;
	while (len) {
		ssize_t n = write(fd, buf, len);
		if (n < 0) {
			if (errno == EINTR) continue;
			return total ? total : n;
		}
		if (!n) break;
		total += n;
		buf = (const char *)buf + n;
		len -= n;
	}
	return total;
}

static int safe_poll(struct pollfd *fds, nfds_t n, int timeout)
{
	/* Let SIGWINCH interrupt the editor's input wait. */
	return poll(fds, n, timeout);
}

static int get_terminal_width_height(int fd, unsigned *width, unsigned *height)
{
	struct winsize ws = { 0 };
	int err = ioctl(fd, TIOCGWINSZ, &ws) < 0 || !ws.ws_row || !ws.ws_col;
	if (width) *width = ws.ws_col ? ws.ws_col : 80;
	if (height) *height = ws.ws_row ? ws.ws_row : 24;
	return err;
}

static int set_termios_to_raw(int fd, struct termios *old, int flags)
{
	struct termios t;
	if (tcgetattr(fd, old) < 0) return -1;
	t = *old;
	t.c_lflag &= ~(ICANON | ECHO | ECHONL);
	t.c_cc[VMIN] = 1;
	t.c_cc[VTIME] = 0;
	if (flags & TERMIOS_RAW_CRNL_INPUT) t.c_iflag &= ~(IXON | ICRNL);
	if (flags & TERMIOS_RAW_CRNL_OUTPUT) t.c_oflag &= ~ONLCR;
	return tcsetattr(fd, TCSANOW, &t);
}

static int tcsetattr_stdin_TCSANOW(const struct termios *t)
{
	return tcsetattr(STDIN_FILENO, TCSANOW, t);
}

static void llist_add_to_end(llist_t **head, void *data)
{
	llist_t **p = head;
	while (*p) p = &(*p)->link;
	*p = xzalloc(sizeof(**p));
	(*p)->data = data;
}

static void *llist_pop(llist_t **head)
{
	llist_t *p = *head;
	void *data;
	if (!p) return NULL;
	*head = p->link;
	data = p->data;
	free(p);
	return data;
}

static uint32_t getopt32(char **argv, const char *optstring, ...)
{
	uint32_t flags = 0;
	va_list ap;
	char *cmds = NULL;
	char clean[64];
	unsigned bit = 0;
	char *out = clean;
	int c;
	va_start(ap, optstring);
	/* vi has one variadic sink: the repeated -c arguments. */
	if (strstr(optstring, "c:")) cmds = NULL;
	for (const char *p = optstring; *p; p++) {
		if (*p != '*') *out++ = *p;
	}
	*out = '\0';
	optind = 1;
	while ((c = getopt(standalone_argc, argv, clean)) != -1) {
		const char *pos = strchr(clean, c);
		if (pos) {
			for (const char *p = clean; p < pos; p++)
				if (*p != ':') bit++;
			flags |= 1u << bit;
			bit = 0;
		}
		if (c == 'c') llist_add_to_end((llist_t **)va_arg(ap, void *), xstrdup(optarg));
	}
	va_end(ap);
	(void)cmds;
	return flags;
}

static unsigned long vi_strtou(const char *s, char **end, int base)
{
	char *p;
	unsigned long n;
	errno = 0;
	n = strtoul(s, &p, base);
	if (end) *end = p;
	return n;
}

static void safe_strncpy(char *dst, const char *src, size_t n)
{
	if (n) {
		strncpy(dst, src, n - 1);
		dst[n - 1] = '\0';
	}
}

static char *concat_path_file(const char *dir, const char *file)
{
	return xasprintf("%s/%s", dir, file);
}

static void *xmalloc_open_read_close(const char *name, size_t *maxsz)
{
	struct stat st;
	int fd = open(name, O_RDONLY);
	char *buf;
	ssize_t n;
	if (fd < 0) return NULL;
	if (fstat(fd, &st) < 0 || st.st_size < 0) { close(fd); return NULL; }
	if (maxsz && (size_t)st.st_size > *maxsz) st.st_size = *maxsz;
	buf = xmalloc((size_t)st.st_size + 1);
	n = full_read(fd, buf, (size_t)st.st_size);
	close(fd);
	if (n < 0) { free(buf); return NULL; }
	buf[n] = '\0';
	if (maxsz) *maxsz = (size_t)n;
	return buf;
}

static char *skip_whitespace(const char *s)
{
	while (*s && isspace((unsigned char)*s)) s++;
	return (char *)s;
}

static char *skip_non_whitespace(const char *s)
{
	while (*s && !isspace((unsigned char)*s)) s++;
	return (char *)s;
}

static char *last_char_is(const char *s, int c)
{
	size_t n = strlen(s);
	return n && (unsigned char)s[n - 1] == (unsigned char)c ? (char *)s + n - 1 : NULL;
}

static int index_in_strings(const char *strings, const char *key)
{
	int i = 0;
	while (*strings) {
		if (!strcmp(strings, key)) return i;
		strings += strlen(strings) + 1;
		i++;
	}
	return -1;
}

static int read_key_sequence(int fd, char *buffer, int timeout)
{
	struct pollfd pfd = { .fd = fd, .events = POLLIN };
	unsigned char c;
	errno = 0;
	if (!buffer[0]) {
		if (timeout >= -1 && safe_poll(&pfd, 1, timeout < 0 ? -1 : timeout) <= 0) return -1;
		if (read(fd, &c, 1) != 1) return -1;
	} else {
		c = (unsigned char)buffer[1];
		memmove(buffer, buffer + 1, (unsigned char)buffer[0]--);
	}
	if (c != 27) return c;
	{
		int ready = safe_poll(&pfd, 1, 50);
		if (ready < 0) return -1;
		if (ready == 0) return 27;
	}
	if (read(fd, &buffer[1], 1) != 1) return -1;
	if (buffer[1] != '[' && buffer[1] != 'O') { buffer[0] = 1; return 27; }
	{
		int ready = safe_poll(&pfd, 1, 50);
		if (ready < 0) return -1;
		if (ready == 0 || read(fd, &buffer[2], 1) != 1) { buffer[0] = 0; return 27; }
	}
	buffer[0] = 0;
	if (buffer[2] == 'A') return KEYCODE_UP;
	if (buffer[2] == 'B') return KEYCODE_DOWN;
	if (buffer[2] == 'C') return KEYCODE_RIGHT;
	if (buffer[2] == 'D') return KEYCODE_LEFT;
	if (buffer[2] == 'H') return KEYCODE_HOME;
	if (buffer[2] == 'F') return KEYCODE_END;
	if (buffer[1] == '[' && buffer[2] == '2') return KEYCODE_INSERT;
	if (buffer[1] == '[' && buffer[2] == '3') return KEYCODE_DELETE;
	return 27;
}

static int64_t safe_read_key(int fd, char *buffer, int timeout)
{
	return read_key_sequence(fd, buffer, timeout);
}

#define vi_main main

#endif
