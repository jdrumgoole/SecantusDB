package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.client.ChangeStreamIterable;
import com.mongodb.client.MongoChangeStreamCursor;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import com.mongodb.client.MongoCollection;
import com.mongodb.client.model.changestream.ChangeStreamDocument;
import org.bson.BsonDocument;
import org.bson.Document;

import java.util.concurrent.TimeUnit;

/**
 * Cross-driver postBatchResumeToken smoke — Java.
 * <p>
 * Pin a change-stream cursor before any inserts; subsequent
 * {@code tryNext()} calls pull empty batches but the resume token
 * should still advance — that's what postBatchResumeToken delivers.
 */
public final class PbrtSmoke {

    private PbrtSmoke() {}

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
            MongoCollection<Document> coll = client.getDatabase("pbrt_xd").getCollection("c");
            coll.drop();

            ChangeStreamIterable<Document> cs = coll.watch().maxAwaitTime(500, TimeUnit.MILLISECONDS);
            try (MongoChangeStreamCursor<ChangeStreamDocument<Document>> cur = cs.cursor()) {
                String initial = tokenSig(cur.getResumeToken());
                cur.tryNext();
                Thread.sleep(200);
                cur.tryNext();
                Thread.sleep(200);
                cur.tryNext();
                String after = tokenSig(cur.getResumeToken());

                if (after == null || after.isEmpty()) {
                    System.err.printf("no resume token after empty polls (initial=%s)%n", initial);
                    System.exit(1);
                }
                if (!initial.isEmpty() && initial.equals(after)) {
                    System.err.printf("resume token did not advance across empty getMores: %s%n", after);
                    System.exit(1);
                }
                System.out.println("OK");
            }
        }
    }

    private static String tokenSig(BsonDocument token) {
        if (token == null) {
            return "";
        }
        if (token.containsKey("_data") && token.get("_data").isString()) {
            return token.getString("_data").getValue();
        }
        return token.toJson();
    }
}
