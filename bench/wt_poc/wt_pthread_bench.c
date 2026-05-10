/* Pure-C pthread benchmark over libwiredtiger.
 *
 * Phase 3.1 gate criterion. Answers: can WiredTiger actually run
 * concurrent inserts in parallel at the C level, with no GIL involved?
 *
 *   N threads, each opening its own WT_SESSION on a shared
 *   WT_CONNECTION, each writing COUNT rows to its own table. Wall-clock
 *   time is measured around the parallel section only (setup + teardown
 *   excluded).
 *
 * If wall-clock(N=4) / wall-clock(N=1) is close to 1.0, WT scales
 * linearly at the C level and any GIL ceiling we see in Python is the
 * SWIG bindings holding the GIL across cursor ops.
 *
 * If wall-clock(N=4) is significantly worse than 1.0, WT itself has a
 * concurrency ceiling (B-tree page locks, log-file fsync ordering,
 * etc.) and re-binding in Cython won't unlock real scaling.
 *
 * Build (from this directory):
 *
 *   cc -O2 -pthread \
 *     -I$WT_INCLUDE \
 *     wt_pthread_bench.c \
 *     -L$WT_LIB -lwiredtiger \
 *     -o wt_pthread_bench
 *
 * Run:
 *
 *   ./wt_pthread_bench <wt_home_dir> <n_threads> <count_per_thread>
 *
 * The wrapper script ``bench/wt_poc/run.py`` invokes it.
 */

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <wiredtiger.h>

typedef struct {
    WT_CONNECTION *conn;
    int thread_id;
    int count;
    int errc;
} thread_arg_t;

static double now_secs(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec / 1e9;
}

static void *worker(void *arg) {
    thread_arg_t *a = (thread_arg_t *)arg;
    WT_SESSION *session = NULL;
    WT_CURSOR *cursor = NULL;
    int rc;
    char table_name[64];
    char key_buf[32];
    char value_buf[1024];

    snprintf(table_name, sizeof(table_name), "table:t%d", a->thread_id);

    rc = a->conn->open_session(a->conn, NULL, NULL, &session);
    if (rc != 0) {
        fprintf(stderr, "thread %d: open_session: %s\n", a->thread_id, wiredtiger_strerror(rc));
        a->errc = rc;
        return NULL;
    }

    rc = session->create(session, table_name, "key_format=q,value_format=u");
    if (rc != 0) {
        fprintf(stderr, "thread %d: create: %s\n", a->thread_id, wiredtiger_strerror(rc));
        a->errc = rc;
        session->close(session, NULL);
        return NULL;
    }

    rc = session->open_cursor(session, table_name, NULL, NULL, &cursor);
    if (rc != 0) {
        fprintf(stderr, "thread %d: open_cursor: %s\n", a->thread_id, wiredtiger_strerror(rc));
        a->errc = rc;
        session->close(session, NULL);
        return NULL;
    }

    /* Build an 8 KiB-ish payload — same size as the workload bench/load_writer
     * drives, so the WT page-write cost is comparable. */
    memset(value_buf, 'x', sizeof(value_buf));
    WT_ITEM v;
    v.data = value_buf;
    v.size = sizeof(value_buf);

    for (int i = 1; i <= a->count; i++) {
        cursor->set_key(cursor, (int64_t)i);
        cursor->set_value(cursor, &v);
        rc = cursor->insert(cursor);
        if (rc != 0) {
            fprintf(stderr, "thread %d: insert at i=%d: %s\n", a->thread_id, i, wiredtiger_strerror(rc));
            a->errc = rc;
            break;
        }
    }

    cursor->close(cursor);
    session->close(session, NULL);
    a->errc = 0;
    return NULL;
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s <wt_home> <n_threads> <count_per_thread>\n", argv[0]);
        return 2;
    }
    const char *home = argv[1];
    int n_threads = atoi(argv[2]);
    int count = atoi(argv[3]);
    if (n_threads <= 0 || count <= 0) {
        fprintf(stderr, "n_threads and count must both be positive\n");
        return 2;
    }

    WT_CONNECTION *conn = NULL;
    /* Default config matches Storage's in src/secantus/storage.py.
     * Compile with -DWT_NOLOG_VARIANT=1 to disable logging — used to
     * test whether WT's journal is the C-level concurrency
     * bottleneck. */
#ifdef WT_NOLOG_VARIANT
    const char *config = "create,session_max=1000,cache_size=1G";
#else
    const char *config =
        "create,session_max=1000,cache_size=1G,"
        "log=(enabled=true,file_max=10MB),"
        "transaction_sync=(enabled=false,method=fsync)";
#endif
    int rc = wiredtiger_open(home, NULL, config, &conn);
    if (rc != 0) {
        fprintf(stderr, "wiredtiger_open: %s\n", wiredtiger_strerror(rc));
        return 1;
    }

    pthread_t *tids = calloc(n_threads, sizeof(pthread_t));
    thread_arg_t *args = calloc(n_threads, sizeof(thread_arg_t));
    if (!tids || !args) {
        fprintf(stderr, "out of memory\n");
        return 1;
    }
    for (int i = 0; i < n_threads; i++) {
        args[i].conn = conn;
        args[i].thread_id = i;
        args[i].count = count;
    }

    double t0 = now_secs();
    for (int i = 0; i < n_threads; i++) {
        rc = pthread_create(&tids[i], NULL, worker, &args[i]);
        if (rc != 0) {
            fprintf(stderr, "pthread_create %d: %s\n", i, strerror(rc));
            return 1;
        }
    }
    for (int i = 0; i < n_threads; i++) {
        pthread_join(tids[i], NULL);
    }
    double t1 = now_secs();
    double elapsed = t1 - t0;

    int worker_errors = 0;
    for (int i = 0; i < n_threads; i++) {
        if (args[i].errc != 0) worker_errors++;
    }

    /* Single line of stdout — easy for the Python wrapper to parse. */
    long long total = (long long)n_threads * count;
    printf("threads=%d count=%d total=%lld elapsed=%.4f rate=%.0f errors=%d\n",
           n_threads, count, total, elapsed,
           elapsed > 0 ? total / elapsed : 0.0,
           worker_errors);

    free(tids);
    free(args);
    conn->close(conn, NULL);
    return worker_errors > 0 ? 1 : 0;
}
