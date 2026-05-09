package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.client.ChangeStreamIterable;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import com.mongodb.client.MongoCollection;
import com.mongodb.client.MongoCursor;
import com.mongodb.client.model.changestream.ChangeStreamDocument;
import org.bson.Document;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;

/**
 * Cross-driver DDL change-stream smoke — Java.
 * <p>
 * Opens a watch on a collection, performs createIndex + dropIndex, and
 * asserts the resulting events come back as {@code createIndexes} /
 * {@code dropIndexes} operationType strings.
 */
public final class DdlSmoke {

    private DdlSmoke() {}

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
            MongoCollection<Document> coll = client.getDatabase("ddl_xd").getCollection("c");
            coll.drop();
            coll.insertOne(new Document("_id", 1));

            ChangeStreamIterable<Document> stream = coll.watch().maxAwaitTime(2, TimeUnit.SECONDS);
            List<String> events = new ArrayList<>();

            // Drain the change stream on a background thread; the main
            // thread issues the DDL writes and then waits for the events
            // to land. The cursor is closed by setting `done` and letting
            // the thread fall out of its loop.
            Thread reader = new Thread(() -> {
                try (MongoCursor<ChangeStreamDocument<Document>> cursor = stream.cursor()) {
                    while (events.size() < 2) {
                        ChangeStreamDocument<Document> evt = cursor.tryNext();
                        if (evt != null) {
                            String op = evt.getOperationTypeString();
                            if (op == null && evt.getOperationType() != null) {
                                op = evt.getOperationType().getValue();
                            }
                            if (op != null) {
                                events.add(op);
                            }
                        } else {
                            try {
                                Thread.sleep(50);
                            } catch (InterruptedException ie) {
                                Thread.currentThread().interrupt();
                                return;
                            }
                        }
                    }
                } catch (Exception ignored) {
                    // Cursor closed mid-loop after we collected our events.
                }
            });
            reader.setDaemon(true);
            reader.start();

            // Settle so the change-stream cursor is registered before writes.
            Thread.sleep(300);

            coll.createIndex(new Document("x", 1));
            coll.dropIndex("x_1");

            long deadline = System.currentTimeMillis() + 8000;
            while (System.currentTimeMillis() < deadline && events.size() < 2) {
                Thread.sleep(200);
            }
            reader.interrupt();

            if (events.size() != 2
                    || !"createIndexes".equals(events.get(0))
                    || !"dropIndexes".equals(events.get(1))) {
                System.err.printf("got %s, want [createIndexes, dropIndexes]%n", events);
                System.exit(1);
            }
            System.out.println("OK");
        }
    }
}
