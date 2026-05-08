package com.secantus.smokes;

import com.mongodb.ConnectionString;
import com.mongodb.MongoClientSettings;
import com.mongodb.MongoCommandException;
import com.mongodb.MongoCredential;
import com.mongodb.client.MongoClient;
import com.mongodb.client.MongoClients;
import org.bson.Document;

import java.util.List;

/**
 * Cross-driver RBAC smoke — Java.
 * <p>
 * Provisions a {@code read}-bound user via the root admin connection,
 * then asserts find works and insert is rejected with code 13 /
 * Unauthorized when authenticated as the new user.
 */
public final class RbacSmoke {

    private RbacSmoke() {}

    public static void main(String[] args) {
        String uri = System.getenv("MONGODB_URI");
        String adminPwd = System.getenv("ADMIN_PASSWORD");
        if (uri == null || adminPwd == null) {
            System.err.println("MONGODB_URI and ADMIN_PASSWORD required");
            System.exit(2);
        }

        try (MongoClient root = clientFor(uri, "root", adminPwd, "admin")) {
            root.getDatabase("shop").runCommand(new Document()
                .append("createUser", "viewer")
                .append("pwd", "vp")
                .append("roles", List.of(new Document()
                    .append("role", "read")
                    .append("db", "shop"))));
        }

        try (MongoClient viewer = clientFor(uri, "viewer", "vp", "shop")) {
            viewer.getDatabase("shop").getCollection("items")
                .find(new Document()).into(new java.util.ArrayList<>());

            MongoCommandException caught = null;
            try {
                viewer.getDatabase("shop").getCollection("items")
                    .insertOne(new Document("x", 1));
            } catch (MongoCommandException ex) {
                caught = ex;
            }
            if (caught == null) {
                System.err.println("insert should have been rejected");
                System.exit(1);
            }
            String message = caught.getErrorMessage() == null ? "" : caught.getErrorMessage();
            if (caught.getErrorCode() != 13 && !message.contains("Unauthorized")) {
                System.err.printf("unexpected error: code=%d message=%s%n",
                    caught.getErrorCode(), message);
                System.exit(1);
            }
            System.out.println("OK");
        }
    }

    private static MongoClient clientFor(String uri, String user, String pwd, String authSource) {
        MongoCredential credential = MongoCredential.createScramSha256Credential(
            user, authSource, pwd.toCharArray());
        MongoClientSettings settings = MongoClientSettings.builder()
            .applyConnectionString(new ConnectionString(uri))
            .credential(credential)
            .applyToClusterSettings(b -> b.serverSelectionTimeout(5,
                java.util.concurrent.TimeUnit.SECONDS))
            .build();
        return MongoClients.create(settings);
    }
}
