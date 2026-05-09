package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import com.mongodb.client.MongoDatabase;
import org.bson.Document;
import org.bson.types.Binary;

import java.util.List;
import java.util.concurrent.TimeUnit;

/**
 * Cross-driver logical-sessions smoke — Java.
 * <p>
 * Drives ``startSession`` / ``endSessions`` / ``refreshSessions`` via
 * raw {@code runCommand} so the wire-level lsid round-trip is what's
 * exercised (not the driver's high-level {@code ClientSession} API).
 */
public final class SessionsSmoke {

    private SessionsSmoke() {}

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
            MongoDatabase admin = client.getDatabase("admin");

            // 1. startSession returns {id: BinData(4, uuid), timeoutMinutes}.
            Document started = admin.runCommand(new Document("startSession", 1));
            if (started.getDouble("ok") != 1.0) {
                System.err.printf("startSession failed: %s%n", started.toJson());
                System.exit(1);
            }
            Document idWrap = started.get("id", Document.class);
            if (idWrap == null) {
                System.err.printf("startSession id wrapper missing: %s%n", started.toJson());
                System.exit(1);
            }
            Binary lsidBin = (Binary) idWrap.get("id");
            if (lsidBin == null || lsidBin.getType() != 4 || lsidBin.getData().length != 16) {
                System.err.printf("lsid shape: %s%n", lsidBin);
                System.exit(1);
            }
            Integer timeout = started.getInteger("timeoutMinutes");
            if (timeout == null || timeout != 30) {
                System.err.printf("timeoutMinutes: %s, want 30%n", timeout);
                System.exit(1);
            }

            // 2. endSessions on the freshly-minted lsid.
            Document ended = admin.runCommand(new Document("endSessions",
                List.of(new Document("id", lsidBin))));
            if (ended.getDouble("ok") != 1.0) {
                System.err.printf("endSessions: %s%n", ended.toJson());
                System.exit(1);
            }

            // 3. refreshSessions implicit-creates an unknown lsid.
            Binary fakeLsid = new Binary((byte) 4, "0123456789abcdef".getBytes());
            Document refreshed = admin.runCommand(new Document("refreshSessions",
                List.of(new Document("id", fakeLsid))));
            if (refreshed.getDouble("ok") != 1.0) {
                System.err.printf("refreshSessions: %s%n", refreshed.toJson());
                System.exit(1);
            }

            System.out.println("OK");
        }
    }
}
