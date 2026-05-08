package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.CursorType;
import com.mongodb.MongoClientSettings;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import com.mongodb.client.MongoCollection;
import com.mongodb.client.MongoCursor;
import com.mongodb.client.MongoDatabase;
import com.mongodb.client.model.CreateCollectionOptions;
import org.bson.Document;

import java.util.concurrent.TimeUnit;

/**
 * Cross-driver tailable cursor smoke — Java.
 * <p>
 * Open a tailable cursor on a capped collection, drain the seeded
 * doc, then insert another and verify the cursor surfaces it without
 * being reopened.
 */
public final class TailableSmoke {

    private TailableSmoke() {}

    public static void main(String[] args) throws Exception {
        String uri = System.getenv("MONGODB_URI");
        if (uri == null) {
            System.err.println("MONGODB_URI not set");
            System.exit(2);
        }

        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .applyToClusterSettings(b -> b.serverSelectionTimeout(5, TimeUnit.SECONDS))
            .build();

        try (MongoClient client = MongoClients.create(settings)) {
            MongoDatabase db = client.getDatabase("tailable_xd");
            db.drop();
            db.createCollection("logs", new CreateCollectionOptions()
                .capped(true).sizeInBytes(64 * 1024));
            MongoCollection<Document> coll = db.getCollection("logs");
            coll.insertOne(new Document("_id", 1));

            try (MongoCursor<Document> cursor = coll.find(new Document())
                    .cursorType(CursorType.Tailable)
                    .cursor()) {
                Document first = cursor.next();
                if (first == null || first.getInteger("_id") != 1) {
                    System.err.printf("first: %s, want {_id: 1}%n",
                        first == null ? null : first.toJson());
                    System.exit(1);
                }

                coll.insertOne(new Document("_id", 2));

                long deadline = System.currentTimeMillis() + 5000;
                while (System.currentTimeMillis() < deadline) {
                    Document ev = cursor.tryNext();
                    if (ev != null) {
                        if (ev.getInteger("_id") != 2) {
                            System.err.printf("second: %s, want {_id: 2}%n", ev.toJson());
                            System.exit(1);
                        }
                        System.out.println("OK");
                        return;
                    }
                    Thread.sleep(100);
                }
                System.err.println("tailable cursor did not surface the new doc within 5s");
                System.exit(1);
            }
        }
    }
}
