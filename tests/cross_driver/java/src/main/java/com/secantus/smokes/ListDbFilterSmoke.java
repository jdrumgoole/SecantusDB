package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import org.bson.Document;

import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.TimeUnit;

/**
 * Cross-driver listDatabases filter smoke — Java.
 * <p>
 * Insert one doc into three databases, then list with
 * {@code filter: {name: "alpha"}} and assert only that one is returned.
 * Run again with {@code nameOnly: true} and assert at least three dbs
 * are listed.
 */
public final class ListDbFilterSmoke {

    private ListDbFilterSmoke() {}

    public static void main(String[] args) {
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
            for (String dbName : List.of("alpha", "beta", "gamma")) {
                client.getDatabase(dbName).getCollection("c").insertOne(new Document("_id", 1));
            }
            try {
                Document filtered = client.getDatabase("admin").runCommand(new Document()
                    .append("listDatabases", 1)
                    .append("filter", new Document("name", "alpha")));
                @SuppressWarnings("unchecked")
                List<Document> dbs = (List<Document>) filtered.get("databases");
                List<String> names = new ArrayList<>();
                if (dbs != null) {
                    for (Document d : dbs) {
                        names.add(d.getString("name"));
                    }
                }
                if (names.size() != 1 || !"alpha".equals(names.get(0))) {
                    System.err.printf("filter: got %s, want [alpha]%n", names);
                    System.exit(1);
                }

                Document namesOnly = client.getDatabase("admin").runCommand(new Document()
                    .append("listDatabases", 1)
                    .append("nameOnly", true));
                @SuppressWarnings("unchecked")
                List<Document> all = (List<Document>) namesOnly.get("databases");
                if (all == null || all.size() < 3) {
                    System.err.printf("nameOnly: got %d dbs, want >= 3%n",
                        all == null ? 0 : all.size());
                    System.exit(1);
                }
                System.out.println("OK");
            } finally {
                for (String dbName : List.of("alpha", "beta", "gamma")) {
                    try {
                        client.getDatabase(dbName).drop();
                    } catch (RuntimeException ignored) {
                        // best-effort cleanup
                    }
                }
            }
        }
    }
}
