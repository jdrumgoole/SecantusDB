package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.client.ChangeStreamIterable;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import com.mongodb.client.MongoCollection;
import com.mongodb.client.MongoCursor;
import com.mongodb.client.model.changestream.ChangeStreamDocument;
import org.bson.BsonDocument;
import org.bson.BsonTimestamp;
import org.bson.Document;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;

/**
 * Cross-driver change-stream resume smoke — Java.
 * <p>
 * Open a stream, insert three docs, capture the resume token after
 * the first event, reopen with {@code resumeAfter} and verify events
 * 2 and 3 arrive. Then reopen with {@code startAtOperationTime} set
 * to a pre-insert timestamp and verify all three events replay.
 */
public final class CsResumeSmoke {

    private CsResumeSmoke() {}

    public static void main(String[] args) throws Exception {
        String uri = System.getenv("MONGODB_URI");
        if (uri == null) {
            System.err.println("MONGODB_URI not set");
            System.exit(2);
        }

        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .applyToClusterSettings(b -> b.serverSelectionTimeout(30, TimeUnit.SECONDS))
            .build();

        try (MongoClient client = MongoClients.create(settings)) {
            MongoCollection<Document> coll = client.getDatabase("cs_resume_xd").getCollection("c");
            coll.drop();

            // Capture pre-insert opTime so startAtOperationTime resumes from
            // a point earlier than every event we'll produce.
            Document hello = client.getDatabase("admin").runCommand(new Document("hello", 1));
            Document lastWrite = hello.get("lastWrite", Document.class);
            BsonTimestamp startTs = null;
            if (lastWrite != null) {
                Document opTime = lastWrite.get("opTime", Document.class);
                if (opTime != null) {
                    Object ts = opTime.get("ts");
                    if (ts instanceof BsonTimestamp) {
                        startTs = (BsonTimestamp) ts;
                    }
                }
            }
            if (startTs == null) {
                System.err.printf("hello did not include lastWrite.opTime.ts: %s%n", hello.toJson());
                System.exit(1);
            }

            // Stream 1: open, insert three docs, take e1 + capture resume token.
            ChangeStreamIterable<Document> cs1 = coll.watch().maxAwaitTime(500, TimeUnit.MILLISECONDS);
            BsonDocument resumeAfter;
            try (MongoCursor<ChangeStreamDocument<Document>> cur1 = cs1.cursor()) {
                cur1.tryNext();
                for (int id : new int[]{1, 2, 3}) {
                    coll.insertOne(new Document("_id", id));
                }
                ChangeStreamDocument<Document> e1 = nextEvent(cur1, 15000);
                int gotId = e1.getDocumentKey().getInt32("_id").getValue();
                if (gotId != 1) {
                    System.err.printf("e1 _id: got %d, want 1%n", gotId);
                    System.exit(1);
                }
                resumeAfter = e1.getResumeToken();
            }

            // Stream 2: reopen with resumeAfter; expect events 2 then 3.
            ChangeStreamIterable<Document> cs2 = coll.watch()
                .resumeAfter(resumeAfter)
                .maxAwaitTime(1, TimeUnit.SECONDS);
            try (MongoCursor<ChangeStreamDocument<Document>> cur2 = cs2.cursor()) {
                ChangeStreamDocument<Document> e2 = nextEvent(cur2, 15000);
                ChangeStreamDocument<Document> e3 = nextEvent(cur2, 15000);
                int e2id = e2.getDocumentKey().getInt32("_id").getValue();
                int e3id = e3.getDocumentKey().getInt32("_id").getValue();
                if (e2id != 2 || e3id != 3) {
                    System.err.printf("resumeAfter sequence: e2=%d e3=%d, want 2,3%n", e2id, e3id);
                    System.exit(1);
                }
            }

            // Stream 3: startAtOperationTime; expect all three events.
            ChangeStreamIterable<Document> cs3 = coll.watch()
                .startAtOperationTime(startTs)
                .maxAwaitTime(1, TimeUnit.SECONDS);
            try (MongoCursor<ChangeStreamDocument<Document>> cur3 = cs3.cursor()) {
                List<Integer> got = new ArrayList<>();
                long deadline = System.currentTimeMillis() + 15000;
                while (got.size() < 3 && System.currentTimeMillis() < deadline) {
                    ChangeStreamDocument<Document> ev = nextEvent(cur3,
                        Math.max(0, deadline - System.currentTimeMillis()));
                    got.add(ev.getDocumentKey().getInt32("_id").getValue());
                }
                if (got.size() != 3 || got.get(0) != 1 || got.get(1) != 2 || got.get(2) != 3) {
                    System.err.printf("startAtOperationTime sequence: %s, want [1,2,3]%n", got);
                    System.exit(1);
                }
            }

            System.out.println("OK");
        }
    }

    private static ChangeStreamDocument<Document> nextEvent(
            MongoCursor<ChangeStreamDocument<Document>> cursor, long timeoutMs) throws InterruptedException {
        long deadline = System.currentTimeMillis() + timeoutMs;
        while (System.currentTimeMillis() < deadline) {
            ChangeStreamDocument<Document> ev = cursor.tryNext();
            if (ev != null) {
                return ev;
            }
            Thread.sleep(150);
        }
        System.err.println("timed out waiting for next change event");
        System.exit(1);
        throw new IllegalStateException("unreachable");
    }
}
