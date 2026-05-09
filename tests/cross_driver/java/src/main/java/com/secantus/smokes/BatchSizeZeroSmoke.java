package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import com.mongodb.client.MongoCollection;
import com.mongodb.event.CommandListener;
import com.mongodb.event.CommandStartedEvent;
import org.bson.Document;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.TimeUnit;

/**
 * Cross-driver batchSize:0 smoke — Java.
 * <p>
 * The Java driver's {@code FindIterable.batchSize(0)} is a Java-side
 * buffer hint, not a wire-level override — it's silently dropped from
 * the find command. This smoke goes through {@code runCommand} so
 * batchSize:0 actually lands on the wire, then issues an explicit
 * getMore against the returned cursor id and asserts both commands
 * appeared via a {@link CommandListener}.
 */
public final class BatchSizeZeroSmoke {

    private BatchSizeZeroSmoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        if (uri == null) {
            System.err.println("MONGODB_URI not set");
            System.exit(2);
        }

        List<String> seenCommands = new CopyOnWriteArrayList<>();
        CommandListener listener = new CommandListener() {
            @Override
            public void commandStarted(CommandStartedEvent event) {
                seenCommands.add(event.getCommandName());
            }
        };

        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .applyToClusterSettings(b -> b.serverSelectionTimeout(30, TimeUnit.SECONDS))
            .addCommandListener(listener)
            .build();

        try (MongoClient client = MongoClients.create(settings)) {
            MongoCollection<Document> coll = client.getDatabase("batch_zero_xd").getCollection("c");
            coll.drop();
            List<Document> docs = new ArrayList<>();
            for (int i = 0; i < 5; i++) {
                docs.add(new Document("_id", i));
            }
            coll.insertMany(docs);

            seenCommands.clear();

            // Raw find with batchSize:0 — server returns an open cursor with
            // an empty firstBatch.
            Document findRes = client.getDatabase("batch_zero_xd").runCommand(new Document()
                .append("find", "c").append("batchSize", 0));
            Document cursor = findRes.get("cursor", Document.class);
            if (cursor == null) {
                System.err.printf("find: no cursor in reply: %s%n", findRes.toJson());
                System.exit(1);
            }
            @SuppressWarnings("unchecked")
            List<Document> firstBatch = (List<Document>) cursor.get("firstBatch");
            if (firstBatch == null || !firstBatch.isEmpty()) {
                System.err.printf("firstBatch: got %s, want empty%n", firstBatch);
                System.exit(1);
            }
            long cursorId = cursor.getLong("id");
            if (cursorId == 0L) {
                System.err.println("cursor.id was zero; server should have kept the cursor open");
                System.exit(1);
            }

            // Explicit getMore against the open cursor — pulls the docs.
            Document moreRes = client.getDatabase("batch_zero_xd").runCommand(new Document()
                .append("getMore", cursorId)
                .append("collection", "c")
                .append("batchSize", 1));
            Document moreCursor = moreRes.get("cursor", Document.class);
            @SuppressWarnings("unchecked")
            List<Document> nextBatch = moreCursor == null
                ? null : (List<Document>) moreCursor.get("nextBatch");
            if (nextBatch == null || nextBatch.isEmpty()
                    || nextBatch.get(0).getInteger("_id") != 0) {
                System.err.printf("nextBatch: got %s, want first doc with _id=0%n", nextBatch);
                System.exit(1);
            }

            // Best-effort kill so we don't leak the cursor.
            try {
                client.getDatabase("batch_zero_xd").runCommand(new Document()
                    .append("killCursors", "c")
                    .append("cursors", List.of(cursorId)));
            } catch (RuntimeException ignored) {
                // Cursor already drained or closed; not material to the test.
            }

            if (!seenCommands.contains("find") || !seenCommands.contains("getMore")) {
                System.err.printf("expected find + getMore on the wire, got %s%n", seenCommands);
                System.exit(1);
            }
            System.out.println("OK");
        }
    }
}
