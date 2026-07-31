/* Phase B experiment 1: does mongod's table layout explain the residual
 * single-writer gap?
 *
 * The engine keys its shared doc-shard tables (S,S,q) — every key carries the
 * (db, coll) namespace strings — while mongod gives each collection its own
 * WT table keyed by a bare int64 RecordId. Three layouts, same workload
 * (each thread inserts COUNT ~1 KiB rows in ascending key order):
 *
 *   q           per-thread table,  key_format=q    (the mongod shape)
 *   ssq         per-thread table,  key_format=SSq  (isolates KEY SHAPE:
 *               string-prefix comparisons + key bytes, same btree locality)
 *   ssq-shared  threads share tables (2 per table at N=8), key_format=SSq
 *               (adds the shared-btree/eviction contention of a shard
 *               collision — the engine's worst representative case)
 *
 * Compression off in every arm so the layout cost is isolated (the engine
 * runs zlib on doc shards; that variable is measured separately in the
 * Finding-13 sweep). Decision gate per tasks/rust-perf-findings.md: a
 * storage-format epoch is only scoped if q beats ssq/ssq-shared by >= 1.5x.
 *
 * Build:
 *   cc -O2 -pthread -I <wt_include> wt_layout_bench.c -L <wt_lib> \
 *      -lwiredtiger -o wt_layout_bench
 * Run:
 *   ./wt_layout_bench <wt_home> <n_threads> <count_per_thread> <layout>
 */

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <errno.h>
#include <string.h>
#include <time.h>
#include <wiredtiger.h>

typedef struct {
    WT_CONNECTION *conn;
    int thread_id;
    int count;
    const char *layout;
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
    char coll_name[32];
    char value_buf[1024];
    int ssq = strcmp(a->layout, "q") != 0;

    if (strcmp(a->layout, "ssq-shared") == 0)
        snprintf(table_name, sizeof(table_name), "table:s%d", a->thread_id % 4);
    else
        snprintf(table_name, sizeof(table_name), "table:t%d", a->thread_id);
    snprintf(coll_name, sizeof(coll_name), "collection_%d", a->thread_id);

    rc = a->conn->open_session(a->conn, NULL, NULL, &session);
    if (rc != 0) {
        fprintf(stderr, "thread %d: open_session: %s\n", a->thread_id, wiredtiger_strerror(rc));
        a->errc = rc;
        return NULL;
    }

    /* Idempotent create: shared-arm threads race on the same table name. */
    rc = session->create(session, table_name,
                         ssq ? "key_format=SSq,value_format=u"
                             : "key_format=q,value_format=u");
    if (rc != 0 && rc != EEXIST) {
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

    /* ~1 KiB payload, matching wt_pthread_bench. */
    memset(value_buf, 'x', sizeof(value_buf));
    WT_ITEM v;
    v.data = value_buf;
    v.size = sizeof(value_buf);

    for (int i = 1; i <= a->count; i++) {
        if (ssq)
            cursor->set_key(cursor, "perfdb", coll_name, (int64_t)i);
        else
            cursor->set_key(cursor, (int64_t)i);
        cursor->set_value(cursor, &v);
        rc = cursor->insert(cursor);
        if (rc != 0) {
            fprintf(stderr, "thread %d: insert at i=%d: %s\n", a->thread_id, i,
                    wiredtiger_strerror(rc));
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
    if (argc != 5) {
        fprintf(stderr, "usage: %s <wt_home> <n_threads> <count_per_thread> <q|ssq|ssq-shared>\n",
                argv[0]);
        return 2;
    }
    const char *home = argv[1];
    int n_threads = atoi(argv[2]);
    int count = atoi(argv[3]);
    const char *layout = argv[4];
    if (n_threads <= 0 || count <= 0 ||
        (strcmp(layout, "q") != 0 && strcmp(layout, "ssq") != 0 &&
         strcmp(layout, "ssq-shared") != 0)) {
        fprintf(stderr, "bad arguments\n");
        return 2;
    }

    WT_CONNECTION *conn = NULL;
    /* Same connection config as wt_pthread_bench's logged variant (the
     * engine's doc shards are logged by default). */
    const char *config =
        "create,session_max=1000,cache_size=1G,"
        "log=(enabled=true,file_max=128MB),transaction_sync=(enabled=false)";
    int rc = wiredtiger_open(home, NULL, config, &conn);
    if (rc != 0) {
        fprintf(stderr, "wiredtiger_open: %s\n", wiredtiger_strerror(rc));
        return 1;
    }

    pthread_t *tids = calloc(n_threads, sizeof(pthread_t));
    thread_arg_t *args = calloc(n_threads, sizeof(thread_arg_t));
    double t0 = now_secs();
    for (int i = 0; i < n_threads; i++) {
        args[i].conn = conn;
        args[i].thread_id = i;
        args[i].count = count;
        args[i].layout = layout;
        rc = pthread_create(&tids[i], NULL, worker, &args[i]);
        if (rc != 0) {
            fprintf(stderr, "pthread_create %d: %s\n", i, strerror(rc));
            return 1;
        }
    }
    int errs = 0;
    for (int i = 0; i < n_threads; i++) {
        pthread_join(tids[i], NULL);
        if (args[i].errc != 0)
            errs++;
    }
    double elapsed = now_secs() - t0;
    conn->close(conn, NULL);
    if (errs) {
        fprintf(stderr, "%d worker(s) failed\n", errs);
        return 1;
    }
    long total = (long)n_threads * count;
    printf("layout=%s threads=%d rows=%ld secs=%.3f rows_per_sec=%.0f\n", layout, n_threads,
           total, elapsed, total / elapsed);
    free(tids);
    free(args);
    return 0;
}
